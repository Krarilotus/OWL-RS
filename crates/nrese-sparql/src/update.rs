//! SPARQL 1.1 Update execution into one engine [`Transaction`].
//!
//! Every operation of a request is applied to the same transaction, in order, and each
//! `WHERE` clause reads the transaction (base plus earlier operations). The caller decides
//! whether to commit: this module never publishes anything, which is what lets the mutation
//! pipeline validate the delta and check the deadline before the commit.
//!
//! Graph existence follows ADR-0002: a named graph exists iff it holds a quad. `CREATE GRAPH`
//! therefore only fails (without `SILENT`) on a non-empty graph and otherwise stores
//! nothing; `CLEAR`/`DROP` of a named graph fail (without `SILENT`) if it's empty.
//!
//! Graph-level access control: `WHERE` clauses read only what [`UpdateOptions::access`]
//! allows, `CLEAR`/`DROP`/`CREATE` see only those graphs (the others are absent), and an
//! operation that would insert or delete a quad in a graph outside
//! [`UpdateOptions::writable`] fails the request ([`UpdateError::Forbidden`]), whether the
//! quad is there or not: the answer says nothing about graphs the user may not read.

use std::collections::HashMap;
use std::sync::Arc;

use nrese_engine::{GraphSelector, QuadPattern, TermId, Transaction};
use nrese_rdf::{BlankNode, GraphName as OxGraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use nrese_sparql_syntax::algebra::GraphTarget;
use nrese_sparql_syntax::term::{GraphName, GroundQuad, Quad as DataQuad};
use nrese_sparql_syntax::{GraphUpdateOperation, Update};
use thiserror::Error;

use crate::results::{CancellationToken, QueryDatasetSpecification, QueryEvaluationError};

#[derive(Clone, Default)]
pub struct UpdateOptions {
    /// Protocol `using-graph-uri` / `using-named-graph-uri`. Applies to every
    /// `DELETE`/`INSERT ... WHERE` operation, replacing its own `USING` clauses.
    pub using: Option<QueryDatasetSpecification>,
    /// The default graph of `WHERE` clauses without a dataset is the merge of all graphs
    /// ([`QueryOptions::union_default_graph`](crate::QueryOptions)).
    pub union_default_graph: bool,
    pub cancellation: Option<CancellationToken>,
    /// Who answers `SERVICE` calls in `WHERE` clauses ([`crate::service`]).
    pub services: Option<crate::Services>,
    /// The graphs `WHERE` clauses may read ([`crate::QueryOptions::access`]).
    pub access: Option<Arc<crate::GraphAccess>>,
    /// The graphs the update may change. `None`: every graph.
    pub writable: Option<Arc<crate::GraphAccess>>,
}

impl UpdateOptions {
    /// Fails unless the update may change `graph`.
    fn check_writable(&self, graph: &OxGraphName) -> Result<(), UpdateError> {
        match &self.writable {
            Some(writable) if !writable.allows_graph(graph) => {
                Err(UpdateError::Forbidden(graph.clone()))
            }
            _ => Ok(()),
        }
    }

    /// Whether the update sees the graph `id` (its user may read it).
    fn sees(&self, tx: &Transaction<'_>, id: TermId) -> bool {
        self.access
            .as_ref()
            .is_none_or(|access| access.allows_id(tx, id))
    }
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error(transparent)]
    Evaluation(#[from] QueryEvaluationError),
    #[error("graph {0} already exists")]
    GraphAlreadyExists(NamedNode),
    #[error("LOAD <{0}> is not enabled on this server")]
    LoadNotAllowed(NamedNode),
    #[error("update cancelled")]
    Cancelled,
    /// The update would change a graph its user may not write.
    #[error(
        "the update would change {}, which the requester may not write",
        graph_label(.0)
    )]
    Forbidden(OxGraphName),
}

/// `graph` in a message: its IRI, or "the default graph".
pub fn graph_label(graph: &OxGraphName) -> String {
    match graph {
        OxGraphName::DefaultGraph => "the default graph".to_owned(),
        graph => graph.to_string(),
    }
}

/// Applies all operations of `update` to `tx`. On error, `tx` may hold part of the request;
/// the caller must drop it (abort) rather than commit.
pub fn apply_update(
    tx: &mut Transaction<'_>,
    update: &Update,
    options: &UpdateOptions,
) -> Result<(), UpdateError> {
    for operation in &update.operations {
        if options
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(UpdateError::Cancelled);
        }
        apply_operation(tx, operation, update, options)?;
    }
    Ok(())
}

fn apply_operation(
    tx: &mut Transaction<'_>,
    operation: &GraphUpdateOperation,
    update: &Update,
    options: &UpdateOptions,
) -> Result<(), UpdateError> {
    match operation {
        GraphUpdateOperation::InsertData { data } => {
            // Blank nodes in INSERT DATA are fresh per request (SPARQL 1.1 Update §3.1.1).
            let mut fresh = HashMap::new();
            for quad in data {
                let quad = fresh_quad(quad, &mut fresh);
                options.check_writable(&quad.graph_name)?;
                tx.insert(quad.as_ref());
            }
        }
        GraphUpdateOperation::DeleteData { data } => {
            for quad in data {
                let quad = ground_quad(quad);
                options.check_writable(&quad.graph_name)?;
                tx.remove(quad.as_ref());
            }
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern,
        } => {
            // On the committed state, or after earlier operations of the request changed
            // something, on a snapshot of the pending state. The whole WHERE result is
            // computed against the state before this operation.
            let pending = (tx.pending() != (0, 0) || tx.inferred_pending() != (0, 0))
                .then(|| tx.pending_snapshot());
            let query_options = crate::query::QueryOptions {
                cancellation: options.cancellation.clone(),
                union_default_graph: options.union_default_graph,
                dataset: options.using.clone(),
                services: options.services.clone(),
                access: options.access.clone(),
                ..crate::query::QueryOptions::default()
            };
            let (deletes, inserts) = crate::native::delete_insert(
                pending.as_ref().unwrap_or(tx.base()),
                pattern,
                delete,
                insert,
                using.as_ref(),
                update.base_iri.as_ref(),
                &query_options,
            )?;
            for quad in deletes.iter().chain(&inserts) {
                options.check_writable(&quad.graph_name)?;
            }
            // Every deletion before any insertion (SPARQL 1.1 Update 3.1.3): a quad one
            // solution deletes and another inserts is present afterwards.
            for quad in &deletes {
                tx.remove(quad.as_ref());
            }
            for quad in &inserts {
                tx.insert(quad.as_ref());
            }
        }
        GraphUpdateOperation::Load { silent, source, .. } => {
            if !silent {
                return Err(UpdateError::LoadNotAllowed(source.clone()));
            }
        }
        GraphUpdateOperation::Create { graph, silent } => {
            let exists = tx
                .lookup(graph.as_ref().into())
                .is_some_and(|id| tx.contains_named_graph(id) && options.sees(tx, id));
            if exists && !silent {
                return Err(UpdateError::GraphAlreadyExists(graph.clone()));
            }
        }
        GraphUpdateOperation::Clear { graph, .. } | GraphUpdateOperation::Drop { graph, .. } => {
            clear(tx, graph, options)?;
        }
    }
    Ok(())
}

/// `CLEAR` and `DROP` are the same operation here: the store doesn't record empty graphs,
/// so a graph exists iff it holds statements. For the same reason, clearing or dropping a
/// graph that holds nothing succeeds, with or without `SILENT` (SPARQL 1.1 Update §3.2
/// leaves that to stores that don't record empty graphs; RDF4J clients, and so
/// ResearchSpace, clear a graph before they write it, whether it exists or not).
///
/// Under access control a graph the user may not read is absent: clearing it does nothing,
/// and `CLEAR NAMED`/`ALL` clear the readable graphs only, each of which must be writable.
fn clear(
    tx: &mut Transaction<'_>,
    target: &GraphTarget,
    options: &UpdateOptions,
) -> Result<(), UpdateError> {
    let remove = |tx: &mut Transaction<'_>, graph| {
        tx.remove_matching(&QuadPattern {
            graph,
            ..QuadPattern::all()
        });
    };
    let restricted = options.access.is_some() || options.writable.is_some();
    let graphs: Vec<TermId> = match target {
        GraphTarget::NamedNode(name) => match tx.lookup(name.as_ref().into()) {
            Some(id) => vec![id],
            None => return Ok(()),
        },
        GraphTarget::DefaultGraph => vec![TermId::DEFAULT_GRAPH],
        GraphTarget::NamedGraphs if !restricted => {
            remove(tx, GraphSelector::AnyNamed);
            return Ok(());
        }
        GraphTarget::AllGraphs if !restricted => {
            remove(tx, GraphSelector::Any);
            return Ok(());
        }
        GraphTarget::NamedGraphs => tx.named_graphs(),
        GraphTarget::AllGraphs => {
            let mut graphs = tx.named_graphs();
            graphs.push(TermId::DEFAULT_GRAPH);
            graphs
        }
    };
    let graphs: Vec<TermId> = graphs
        .into_iter()
        .filter(|&id| options.sees(tx, id))
        .collect();
    for &id in &graphs {
        let empty = tx
            .quads_for_pattern_in(
                nrese_engine::ReadModel::Asserted,
                &QuadPattern::in_graph(id),
            )
            .next()
            .is_none();
        // Clearing an empty graph changes nothing.
        if empty {
            continue;
        }
        let graph = if id == TermId::DEFAULT_GRAPH {
            OxGraphName::DefaultGraph
        } else {
            match tx.decode(id) {
                Some(Term::NamedNode(node)) => node.into(),
                Some(Term::BlankNode(node)) => node.into(),
                _ => continue,
            }
        };
        options.check_writable(&graph)?;
    }
    for id in graphs {
        remove(tx, GraphSelector::Exact(id));
    }
    Ok(())
}

fn graph_name(graph: &GraphName) -> OxGraphName {
    match graph {
        GraphName::NamedNode(name) if crate::compat::names_default_graph(name.as_str()) => {
            OxGraphName::DefaultGraph
        }
        GraphName::NamedNode(name) => name.clone().into(),
        GraphName::DefaultGraph => OxGraphName::DefaultGraph,
    }
}

fn fresh_quad(quad: &DataQuad, fresh: &mut HashMap<BlankNode, BlankNode>) -> Quad {
    let mut rename = |node: &BlankNode| fresh.entry(node.clone()).or_default().clone();
    Quad::new(
        match &quad.subject {
            NamedOrBlankNode::NamedNode(node) => NamedOrBlankNode::from(node.clone()),
            NamedOrBlankNode::BlankNode(node) => rename(node).into(),
        },
        quad.predicate.clone(),
        fresh_term(&quad.object, &mut rename),
        graph_name(&quad.graph_name),
    )
}

/// `term` with its blank nodes, inside triple terms too, renamed fresh.
fn fresh_term(term: &Term, rename: &mut impl FnMut(&BlankNode) -> BlankNode) -> Term {
    match term {
        Term::BlankNode(node) => rename(node).into(),
        Term::Triple(triple) => nrese_rdf::Triple::new(
            match &triple.subject {
                NamedOrBlankNode::NamedNode(node) => NamedOrBlankNode::from(node.clone()),
                NamedOrBlankNode::BlankNode(node) => rename(node).into(),
            },
            triple.predicate.clone(),
            fresh_term(&triple.object, rename),
        )
        .into(),
        term => term.clone(),
    }
}

fn ground_quad(quad: &GroundQuad) -> Quad {
    Quad::new(
        quad.subject.clone(),
        quad.predicate.clone(),
        Term::from(quad.object.clone()),
        graph_name(&quad.graph_name),
    )
}
