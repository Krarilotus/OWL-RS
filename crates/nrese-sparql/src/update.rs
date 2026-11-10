//! SPARQL 1.1 Update execution into one engine transaction: a write [`Transaction`], or a
//! [`PrivateTransaction`] whose new terms never enter the dictionary ([`UpdateTarget`]).
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

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use nrese_engine::{
    EncodedQuad, GraphSelector, PendingView, PendingWrite, PrivateTransaction, QuadPattern,
    ReadModel, Snapshot, TermId, Transaction,
};
use nrese_exec::Budget;
use nrese_rdf::{
    BlankNode, GraphName as OxGraphName, GraphNameRef, NamedNode, NamedOrBlankNode, Quad, Term,
};
use nrese_sparql_syntax::algebra::GraphTarget;
use nrese_sparql_syntax::term::{GraphName, GroundQuad, Quad as DataQuad};
use nrese_sparql_syntax::{GraphUpdateOperation, TransferOp, Update};
use thiserror::Error;

use crate::results::{QueryDatasetSpecification, QueryEvaluationError};
use crate::view::{PendingRead, ReadView, engine_refusal};

/// What an update's `WHERE` clauses read, for its reader: the caller's decision of which
/// statements that reader sees ([`UpdateOptions::read`]). One method per transaction kind,
/// both required: a private transaction is never read as its base or as a write transaction.
pub trait ReadSource: Send + Sync {
    /// The data a `WHERE` clause reads: `tx`'s state before its operation (its base, with
    /// the request's earlier operations), as the update's reader sees it, its pending
    /// snapshots made against `budget`. An error fails the operation as the clause's
    /// evaluation would.
    fn state(
        &self,
        tx: &Transaction<'_>,
        budget: &Arc<Budget>,
    ) -> Result<Snapshot, QueryEvaluationError>;

    /// [`Self::state`] of a private transaction, in its own vocabulary.
    fn private_state(
        &self,
        tx: &PrivateTransaction<'_>,
        budget: &Arc<Budget>,
    ) -> Result<Snapshot, QueryEvaluationError>;
}

/// What an update applies to: either kind of transaction ([`PendingWrite`], sealed in the
/// engine), read by its own kind's [`ReadSource`] method.
pub trait UpdateTarget: PendingWrite {
    /// What `source` gives as this transaction's state for a `WHERE` clause.
    fn read_from(
        &self,
        source: &dyn ReadSource,
        budget: &Arc<Budget>,
    ) -> Result<Snapshot, QueryEvaluationError>;
}

impl UpdateTarget for Transaction<'_> {
    fn read_from(
        &self,
        source: &dyn ReadSource,
        budget: &Arc<Budget>,
    ) -> Result<Snapshot, QueryEvaluationError> {
        source.state(self, budget)
    }
}

impl UpdateTarget for PrivateTransaction<'_> {
    fn read_from(
        &self,
        source: &dyn ReadSource,
        budget: &Arc<Budget>,
    ) -> Result<Snapshot, QueryEvaluationError> {
        source.private_state(self, budget)
    }
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
    fn sees(&self, view: &impl ReadView, id: TermId) -> bool {
        self.access
            .as_ref()
            .is_none_or(|access| access.allows_id(view, id))
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

/// Applies all operations of `update` to `tx`, a write transaction or a private one. On
/// error, `tx` may hold part of the request; the caller must drop it (abort) rather than
/// commit. A refused write or `WHERE` snapshot (a private transaction's charges) fails the
/// request with [`QueryEvaluationError::MemoryLimit`]; nothing is skipped.
pub fn apply_update<T: UpdateTarget>(
    tx: &mut T,
    update: &Update,
    options: &UpdateOptions,
) -> Result<(), UpdateError> {
    // What a `WHERE` clause's snapshot of the pending state is charged to: one budget for the
    // request, drawn from its envelope, so an unchanged state's snapshot is the one kept, not
    // made again (the pending-snapshot memo is per budget).
    let budget = Arc::new(options.resources.memory.part());
    for operation in &update.operations {
        if options.resources.check().is_err() {
            return Err(UpdateError::Cancelled);
        }
        apply_operation(tx, operation, update, options, &budget)?;
    }
    Ok(())
}

fn apply_operation<T: UpdateTarget>(
    tx: &mut T,
    operation: &GraphUpdateOperation,
    update: &Update,
    options: &UpdateOptions,
    budget: &Arc<Budget>,
) -> Result<(), UpdateError> {
    match operation {
        GraphUpdateOperation::InsertData { data } => {
            // Blank nodes in INSERT DATA are fresh per request (SPARQL 1.1 Update §3.1.1).
            let mut fresh = HashMap::new();
            for quad in data {
                let quad = fresh_quad(quad, &mut fresh);
                options.check_writable(&quad.graph_name)?;
                tx.try_insert(quad.as_ref()).map_err(engine_refusal)?;
            }
        }
        GraphUpdateOperation::DeleteData { data } => {
            for quad in data {
                let quad = ground_quad(quad);
                options.check_writable(&quad.graph_name)?;
                tx.remove(quad.as_ref()).map_err(engine_refusal)?;
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
            // state before this operation. A pending snapshot is charged to the request's
            // envelope by its builder before it is made, and for as long as it is held
            // (D16-039); refused, the operation fails with the request's memory error.
            let view = PendingRead::new(&*tx, budget);
            let state: Cow<'_, Snapshot> = match &options.read {
                Some(read) => Cow::Owned(tx.read_from(read.as_ref(), budget)?),
                None => view.evaluation_snapshot()?,
            };
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
                &state,
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
            drop(state);
            // Every deletion before any insertion (SPARQL 1.1 Update 3.1.3): a quad one
            // solution deletes and another inserts is present afterwards.
            for quad in changes.deletes.iter() {
                tx.remove(quad.as_ref()).map_err(engine_refusal)?;
            }
            for quad in changes.inserts.iter() {
                tx.try_insert(quad.as_ref()).map_err(engine_refusal)?;
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
            let view = PendingRead::new(&*tx, budget);
            let exists =
                graph_id(&*tx, graph).is_some_and(|id| holds(id) && options.sees(&view, id));
            if exists && !silent {
                return Err(UpdateError::GraphAlreadyExists(graph.clone()));
            }
        }
        GraphUpdateOperation::Clear { graph, .. } | GraphUpdateOperation::Drop { graph, .. } => {
            clear(tx, graph, options, budget)?;
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
                clear(tx, &target(to), options, budget)?;
            }
            add(tx, from, to, options, budget)?;
            if *op == TransferOp::Move {
                clear(tx, &target(from), options, budget)?;
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
/// transaction's budget before it grows ([`PendingWrite::try_reserve_inserts`], where the
/// transaction has one, [`PendingWrite::charges_pending`]: the mutation pipeline's, a pending
/// read's and every private one do).
/// A copy past either is refused with the typed memory error, and the destination's name is
/// interned only once nothing but that interning can be refused.
fn add<T: UpdateTarget>(
    tx: &mut T,
    from: &GraphName,
    to: &GraphName,
    options: &UpdateOptions,
    budget: &Arc<Budget>,
) -> Result<(), UpdateError> {
    let memory = |error| UpdateError::Evaluation(QueryEvaluationError::MemoryLimit(error));
    let cancelled = |_: nrese_exec::Stop| UpdateError::Cancelled;
    let source = match graph_name(from) {
        OxGraphName::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
        OxGraphName::NamedNode(name) => tx.lookup(name.as_ref().into()),
        OxGraphName::BlankNode(_) => None,
    };
    let view = PendingRead::new(&*tx, budget);
    let Some(source) = source.filter(|&id| options.sees(&view, id)) else {
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
        tx.try_reserve_inserts(quads.len())
            .map_err(engine_refusal)?;
    }
    let target = match &to {
        OxGraphName::DefaultGraph => Ok(TermId::DEFAULT_GRAPH),
        OxGraphName::NamedNode(name) => tx.try_intern(name.as_ref().into()),
        OxGraphName::BlankNode(node) => tx.try_intern(node.as_ref().into()),
    }
    .map_err(engine_refusal)?;
    for &quad in quads.iter() {
        ticker.tick(1).map_err(cancelled)?;
        tx.try_insert_encoded(EncodedQuad {
            graph: target,
            ..quad
        })
        .map_err(engine_refusal)?;
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
fn clear<T: UpdateTarget>(
    tx: &mut T,
    target: &GraphTarget,
    options: &UpdateOptions,
    budget: &Arc<Budget>,
) -> Result<(), UpdateError> {
    // The retractions are charged as they grow (D16-039).
    let remove = |tx: &mut T, graph| -> Result<(), UpdateError> {
        tx.remove_matching(&QuadPattern {
            graph,
            ..QuadPattern::all()
        })
        .map(drop)
        .map_err(|error| engine_refusal(error).into())
    };
    let restricted = options.access.is_some() || options.writable.is_some();
    let graphs: Vec<TermId> = match target {
        GraphTarget::NamedNode(name) => match graph_id(&*tx, name) {
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
    let view = PendingRead::new(&*tx, budget);
    let graphs: Vec<TermId> = graphs
        .into_iter()
        .filter(|&id| options.sees(&view, id))
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

/// The store graph an operation's graph name names ([`crate::compat::graph_named`]).
fn graph_name(graph: &GraphName) -> OxGraphName {
    match graph {
        GraphName::NamedNode(name) => crate::compat::graph_named(name).into_owned(),
        GraphName::DefaultGraph => OxGraphName::DefaultGraph,
    }
}

/// The id of the store graph `name` names ([`crate::compat::graph_named`]), if the store
/// knows that name.
fn graph_id(tx: &impl PendingView, name: &NamedNode) -> Option<TermId> {
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
