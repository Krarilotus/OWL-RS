//! SPARQL 1.1 Update execution into one engine [`Transaction`].
//!
//! Every operation of a request is applied to the same transaction, in order, and each
//! `WHERE` clause reads the transaction's state before its operation (base plus earlier
//! operations): as it is, or, with [`UpdateOptions::read`], as that [`ReadSource`] gives it
//! for the update's reader, in the statements [`UpdateOptions::read_model`] names (a reader
//! who sees no inferred statement reads the asserted ones). The caller decides whether to
//! commit: this module never publishes anything, which is what lets the mutation pipeline
//! validate the delta and check the deadline before the commit.
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

use nrese_engine::{
    EncodedQuad, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, Transaction,
};
use nrese_rdf::{
    BlankNode, GraphName as OxGraphName, GraphNameRef, NamedNode, NamedOrBlankNode, Quad, Term,
};
use nrese_sparql_syntax::algebra::GraphTarget;
use nrese_sparql_syntax::term::{GraphName, GroundQuad, Quad as DataQuad};
use nrese_sparql_syntax::{GraphUpdateOperation, TransferOp, Update};
use thiserror::Error;

use crate::results::{QueryDatasetSpecification, QueryEvaluationError};

/// What an update's `WHERE` clauses read, for its reader: the caller's decision of which
/// statements that reader sees ([`UpdateOptions::read`]).
pub trait ReadSource: Send + Sync {
    /// The data a `WHERE` clause reads: `tx`'s state before its operation (its base, with
    /// the request's earlier operations), as the update's reader sees it. An error fails the
    /// operation as the clause's evaluation would.
    fn state(&self, tx: &Transaction<'_>) -> Result<Snapshot, QueryEvaluationError>;
}

#[derive(Clone, Default)]
pub struct UpdateOptions {
    /// Protocol `using-graph-uri` / `using-named-graph-uri`. Applies to every
    /// `DELETE`/`INSERT ... WHERE` operation, replacing its own `USING` clauses.
    pub using: Option<QueryDatasetSpecification>,
    /// The default graph of `WHERE` clauses without a dataset is the merge of all graphs
    /// ([`QueryOptions::union_default_graph`](crate::QueryOptions)).
    pub union_default_graph: bool,
    /// Its cancellation, and what each `WHERE` evaluation may use.
    pub resources: crate::Resources,
    /// Who answers `SERVICE` calls in `WHERE` clauses ([`crate::service`]).
    pub services: Option<crate::Services>,
    /// The graphs `WHERE` clauses may read ([`crate::QueryOptions::access`]).
    pub access: Option<Arc<crate::GraphAccess>>,
    /// The graphs the update may change. `None`: every graph.
    pub writable: Option<Arc<crate::GraphAccess>>,
    /// GeoSPARQL relations in `WHERE` clauses read the asserted statements only
    /// ([`crate::QueryOptions::geosparql_stated_only`]).
    pub geosparql_stated_only: bool,
    /// What each `WHERE` clause reads in place of the transaction's state as it is: asked
    /// once per `DELETE`/`INSERT … WHERE` operation, on the state before it, the request's
    /// earlier operations included. [`Self::access`] still restricts the graphs it reads.
    /// `None`: the state as it is.
    pub read: Option<Arc<dyn ReadSource>>,
    /// Which statements each `WHERE` clause reads ([`crate::QueryOptions::read_model`]): the
    /// asserted ones only for a reader who sees no inferred statement. Default: both.
    pub read_model: nrese_engine::ReadModel,
    /// What searches in `WHERE` clauses rank and select among
    /// ([`crate::QueryOptions::search_scope`]).
    pub search_scope: crate::SearchScope,
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
        if options.resources.check().is_err() {
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
                tx.try_insert(quad.as_ref()).map_err(pending_error)?;
            }
        }
        GraphUpdateOperation::DeleteData { data } => {
            for quad in data {
                let quad = ground_quad(quad);
                options.check_writable(&quad.graph_name)?;
                tx.try_remove(quad.as_ref()).map_err(pending_error)?;
            }
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern,
        } => {
            // On the committed state, or after earlier operations of the request changed
            // something, on a snapshot of the pending state; as the reader's read source
            // gives it, where there is one. The whole WHERE result is computed against the
            // state before this operation.
            let read = options
                .read
                .as_ref()
                .map(|read| read.state(tx))
                .transpose()?;
            let pending = (read.is_none()
                && (tx.pending() != (0, 0) || tx.inferred_pending() != (0, 0)))
                .then(|| tx.pending_snapshot());
            let state = read.as_ref().or(pending.as_ref()).unwrap_or(tx.base());
            let query_options = crate::query::QueryOptions {
                resources: options.resources.clone(),
                union_default_graph: options.union_default_graph,
                dataset: options.using.clone(),
                services: options.services.clone(),
                access: options.access.clone(),
                geosparql_stated_only: options.geosparql_stated_only,
                read_model: options.read_model,
                search_scope: options.search_scope,
                ..crate::query::QueryOptions::default()
            };
            // Charged while they live, dropped once applied (D16-039).
            let changes = crate::native::delete_insert(
                state,
                pattern,
                delete,
                insert,
                using.as_ref(),
                update.base_iri.as_ref(),
                &query_options,
            )?;
            for quad in changes.deletes.iter().chain(changes.inserts.iter()) {
                options.check_writable(&quad.graph_name)?;
            }
            // Every deletion before any insertion (SPARQL 1.1 Update 3.1.3): a quad one
            // solution deletes and another inserts is present afterwards.
            for quad in changes.deletes.iter() {
                tx.try_remove(quad.as_ref()).map_err(pending_error)?;
            }
            for quad in changes.inserts.iter() {
                tx.try_insert(quad.as_ref()).map_err(pending_error)?;
            }
        }
        GraphUpdateOperation::Load { silent, source, .. } => {
            if !silent {
                return Err(UpdateError::LoadNotAllowed(source.clone()));
            }
        }
        GraphUpdateOperation::Create { graph, silent } => {
            // Another store's name for the default graph names it: it exists iff it holds a
            // quad, as a named graph does.
            let holds = |id| match id == TermId::DEFAULT_GRAPH {
                true => tx
                    .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::in_graph(id))
                    .next()
                    .is_some(),
                false => tx.contains_named_graph(id),
            };
            let exists = graph_id(tx, graph).is_some_and(|id| holds(id) && options.sees(tx, id));
            if exists && !silent {
                return Err(UpdateError::GraphAlreadyExists(graph.clone()));
            }
        }
        GraphUpdateOperation::Clear { graph, .. } | GraphUpdateOperation::Drop { graph, .. } => {
            clear(tx, graph, options)?;
        }
        GraphUpdateOperation::Transfer { op, from, to, .. } => {
            // SPARQL 1.1 Update §3.2.3–3.2.5: a transfer between two names of one store graph
            // changes nothing (review r1 of slice 2, S2-F1); the names are resolved first.
            if graph_name(from) == graph_name(to) {
                return Ok(());
            }
            let target = |graph: &GraphName| match graph {
                GraphName::NamedNode(name) => GraphTarget::NamedNode(name.clone()),
                GraphName::DefaultGraph => GraphTarget::DefaultGraph,
            };
            if *op != TransferOp::Add {
                clear(tx, &target(to), options)?;
            }
            add(tx, from, to, options)?;
            if *op == TransferOp::Move {
                clear(tx, &target(from), options)?;
            }
        }
    }
    Ok(())
}

/// `ADD`, and the copy step of `MOVE` and `COPY`: every asserted statement of `from`, in the
/// state before the operation, inserted into `to`. Its input is the graph as stored, not what
/// a `WHERE` clause reads ([`GraphUpdateOperation::Transfer`]): no union default graph, inferred
/// statement or protocol dataset. A graph the user may not read is absent, so nothing is
/// copied from it; inserting into a graph the user may not write fails the request.
///
/// Memory (SYN-54 review r1, F1): the source's statements are read into a buffer charged to
/// the request's envelope, and their room in the pending changes is reserved from the
/// transaction's budget before it grows ([`Transaction::try_reserve_inserts`], where the
/// transaction has one: the mutation pipeline's and a pending read's do). A copy past either
/// is refused with the typed memory error, and the destination's name is interned only once
/// nothing can be refused.
fn add(
    tx: &mut Transaction<'_>,
    from: &GraphName,
    to: &GraphName,
    options: &UpdateOptions,
) -> Result<(), UpdateError> {
    let memory = |error| UpdateError::Evaluation(QueryEvaluationError::MemoryLimit(error));
    let cancelled = |_: nrese_exec::Stop| UpdateError::Cancelled;
    let source = match graph_name(from) {
        OxGraphName::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
        OxGraphName::NamedNode(name) => tx.lookup(name.as_ref().into()),
        OxGraphName::BlankNode(_) => None,
    };
    let Some(source) = source.filter(|&id| options.sees(tx, id)) else {
        return Ok(());
    };
    let ticker = nrese_exec::Ticker::new(options.resources.stop()).map_err(cancelled)?;
    // The state before this operation, the request's earlier operations included.
    let mut quads = nrese_exec::ChargedVec::new(Arc::new(options.resources.memory.part()));
    for quad in tx.quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::in_graph(source)) {
        ticker.tick(1).map_err(cancelled)?;
        quads.push(quad).map_err(memory)?;
    }
    if quads.is_empty() {
        return Ok(());
    }
    let to = graph_name(to);
    options.check_writable(&to)?;
    if tx.charges_pending() {
        tx.try_reserve_inserts(quads.len()).map_err(pending_error)?;
    }
    let target = match &to {
        OxGraphName::DefaultGraph => TermId::DEFAULT_GRAPH,
        OxGraphName::NamedNode(name) => tx.intern(name.as_ref().into()),
        OxGraphName::BlankNode(node) => tx.intern(node.as_ref().into()),
    };
    for &quad in quads.iter() {
        ticker.tick(1).map_err(cancelled)?;
        tx.insert_encoded(EncodedQuad {
            graph: target,
            ..quad
        });
    }
    ticker.finish().map_err(cancelled)?;
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
    // The retractions are charged as they grow (D16-039).
    let remove = |tx: &mut Transaction<'_>, graph| {
        tx.remove_matching(&QuadPattern {
            graph,
            ..QuadPattern::all()
        })
        .map(drop)
        .map_err(pending_error)
    };
    let restricted = options.access.is_some() || options.writable.is_some();
    let graphs: Vec<TermId> = match target {
        GraphTarget::NamedNode(name) => match graph_id(tx, name) {
            Some(id) => vec![id],
            None => return Ok(()),
        },
        GraphTarget::DefaultGraph => vec![TermId::DEFAULT_GRAPH],
        GraphTarget::NamedGraphs if !restricted => return remove(tx, GraphSelector::AnyNamed),
        GraphTarget::AllGraphs if !restricted => return remove(tx, GraphSelector::Any),
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
        remove(tx, GraphSelector::Exact(id))?;
    }
    Ok(())
}

/// A refused change to the pending changes as the update's error: the typed memory error where
/// their growth is past the request's envelope, anything else unexpected.
fn pending_error(error: nrese_engine::EngineError) -> UpdateError {
    match error {
        nrese_engine::EngineError::MemoryLimit(error) => {
            UpdateError::Evaluation(QueryEvaluationError::MemoryLimit(error))
        }
        error => UpdateError::Evaluation(QueryEvaluationError::Unexpected(error.to_string())),
    }
}

/// The store graph an operation's graph name names ([`crate::compat::graph_named`]).
fn graph_name(graph: &GraphName) -> OxGraphName {
    match graph {
        GraphName::NamedNode(name) => crate::compat::graph_named(name).into_owned(),
        GraphName::DefaultGraph => OxGraphName::DefaultGraph,
    }
}

/// The id of the store graph `name` names ([`crate::compat::graph_named`]), if the store
/// knows that name.
fn graph_id(tx: &Transaction<'_>, name: &NamedNode) -> Option<TermId> {
    match crate::compat::graph_named(name) {
        GraphNameRef::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
        GraphNameRef::NamedNode(name) => tx.lookup(name.into()),
        GraphNameRef::BlankNode(node) => tx.lookup(node.into()),
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
