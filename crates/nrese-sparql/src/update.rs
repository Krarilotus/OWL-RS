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
use oxrdf::{BlankNode, GraphName as OxGraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use spareval::{
    CancellationToken, DeleteInsertQuad, QueryDatasetSpecification, QueryEvaluationError,
};
use spargebra::algebra::GraphTarget;
use spargebra::term::{GraphName, GroundQuad, GroundTerm, Quad as DataQuad};
use spargebra::{GraphUpdateOperation, Update};
use thiserror::Error;

use crate::dataset::EngineDataset;
use crate::query::evaluator;

#[derive(Clone, Default)]
pub struct UpdateOptions {
    /// Protocol `using-graph-uri` / `using-named-graph-uri`. Applies to every
    /// `DELETE`/`INSERT ... WHERE` operation, replacing its own `USING` clauses.
    pub using: Option<QueryDatasetSpecification>,
    /// The default graph of `WHERE` clauses without a dataset is the merge of all graphs
    /// ([`QueryOptions::union_default_graph`](crate::QueryOptions)).
    pub union_default_graph: bool,
    pub cancellation: Option<CancellationToken>,
    /// Evaluate `WHERE` clauses on spareval even where the native executor could
    /// (differential testing).
    pub force_spareval: bool,
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
            // Natively when the WHERE reads only committed data: the first operation of a
            // request, or after operations that changed nothing.
            let native = (!options.force_spareval
                && update.base_iri.is_none()
                && tx.pending() == (0, 0)
                && tx.inferred_pending() == (0, 0))
                .then(|| {
                    let query_options = crate::query::QueryOptions {
                        cancellation: options.cancellation.clone(),
                        union_default_graph: options.union_default_graph,
                        dataset: options.using.clone(),
                        ..crate::query::QueryOptions::default()
                    };
                    crate::native::delete_insert(
                        tx.base(),
                        pattern,
                        delete,
                        insert,
                        using.as_ref(),
                        &query_options,
                    )
                })
                .flatten();
            // The whole WHERE result is computed against the state before this operation.
            let (deletes, inserts) = match native {
                Some(changes) => changes?,
                None => {
                    let evaluator = evaluator(options.cancellation.as_ref());
                    let prepared = evaluator.prepare_delete_insert(
                        delete.clone(),
                        insert.clone(),
                        update.base_iri.clone(),
                        // The adapter presents the operation's dataset as the store.
                        None,
                        pattern,
                    );
                    let mut deletes = Vec::new();
                    let mut inserts = Vec::new();
                    let dataset = EngineDataset::new(&*tx).reading(
                        options.union_default_graph,
                        options.using.as_ref(),
                        using.as_ref(),
                    );
                    for change in prepared.execute(dataset)? {
                        match change? {
                            DeleteInsertQuad::Delete(quad) => deletes.push(quad),
                            DeleteInsertQuad::Insert(quad) => inserts.push(quad),
                        }
                    }
                    (deletes, inserts)
                }
            };
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
        match &quad.object {
            Term::BlankNode(node) => rename(node).into(),
            term => term.clone(),
        },
        graph_name(&quad.graph_name),
    )
}

fn ground_quad(quad: &GroundQuad) -> Quad {
    Quad::new(
        quad.subject.clone(),
        quad.predicate.clone(),
        match &quad.object {
            GroundTerm::NamedNode(node) => Term::from(node.clone()),
            GroundTerm::Literal(literal) => literal.clone().into(),
        },
        graph_name(&quad.graph_name),
    )
}
