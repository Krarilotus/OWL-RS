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

use std::collections::HashMap;

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
                tx.insert(fresh_quad(quad, &mut fresh).as_ref());
            }
        }
        GraphUpdateOperation::DeleteData { data } => {
            for quad in data {
                tx.remove(ground_quad(quad).as_ref());
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
                .is_some_and(|id| tx.contains_named_graph(id));
            if exists && !silent {
                return Err(UpdateError::GraphAlreadyExists(graph.clone()));
            }
        }
        GraphUpdateOperation::Clear { graph, .. } | GraphUpdateOperation::Drop { graph, .. } => {
            clear(tx, graph);
        }
    }
    Ok(())
}

/// `CLEAR` and `DROP` are the same operation here: the store doesn't record empty graphs,
/// so a graph exists iff it holds statements. For the same reason, clearing or dropping a
/// graph that holds nothing succeeds, with or without `SILENT` (SPARQL 1.1 Update §3.2
/// leaves that to stores that don't record empty graphs; RDF4J clients, and so
/// ResearchSpace, clear a graph before they write it, whether it exists or not).
fn clear(tx: &mut Transaction<'_>, target: &GraphTarget) {
    let graph = match target {
        GraphTarget::NamedNode(name) => match tx.lookup(name.as_ref().into()) {
            Some(id) => GraphSelector::Exact(id),
            None => return,
        },
        GraphTarget::DefaultGraph => GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        GraphTarget::NamedGraphs => GraphSelector::AnyNamed,
        GraphTarget::AllGraphs => GraphSelector::Any,
    };
    tx.remove_matching(&QuadPattern {
        graph,
        ..QuadPattern::all()
    });
}

fn graph_name(graph: &GraphName) -> OxGraphName {
    match graph {
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
