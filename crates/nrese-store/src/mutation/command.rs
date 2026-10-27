use std::sync::Arc;

use nrese_engine::Transaction;
use nrese_sparql::{CancellationToken, UpdateOptions, apply_update};
use nrese_sparql_syntax::SparqlParser;

use crate::backup::{DatasetRestoreReport, DatasetRestoreRequest, apply_restore};
use crate::error::StoreError;
use crate::graph_store::{GraphDeleteReport, GraphTarget, GraphWriteReport, GraphWriteRequest};
use crate::graph_store_executor::{apply_graph_delete, apply_graph_write};
use crate::statements::{StatementsRequest, apply_statements};
use crate::tell::TellRequest;
use crate::update::SparqlUpdateRequest;

/// Every write entry point of the product. All of them go through the same pipeline, so
/// validation gates and commit semantics cannot drift between entry points.
#[derive(Debug, Clone)]
pub enum MutationCommand {
    Update(SparqlUpdateRequest),
    Tell(TellRequest),
    GraphWrite(GraphWriteRequest),
    GraphDelete(GraphTarget),
    Restore(DatasetRestoreRequest),
    /// RDF4J's statement operations, in order ([`crate::statements`]).
    Statements(StatementsRequest),
    /// A write of any other front-end (a GQL or Cypher `CREATE`, `SET`, `DELETE`): its
    /// planner makes the delta, the pipeline gates and commits it as every other write's.
    Delta(Arc<dyn DeltaPlanner>),
}

/// The extension point for writes: what a front-end's write does to the dataset, planned
/// into the pipeline's transaction. The pipeline then gates it exactly as a SPARQL update
/// (the requester's write scope over every graph the transaction changes, reasoning,
/// consistency, SHACL, cancellation) and commits it; no front-end needs a command of its
/// own, and none can commit around the gates.
pub trait DeltaPlanner: Send + Sync + std::fmt::Debug {
    /// What the front-end calls the write, for logs and errors (`gql`, `cypher`, ...).
    fn name(&self) -> &str;

    /// Applies the write to `tx` for `requester`, without committing: the statements `tx`
    /// then inserts and deletes are the write's delta. What it reads, it reads within
    /// `requester.read` (a pattern of the write must not see a graph the requester may not
    /// read); `context` carries the evaluation's cancellation, memory budget and the
    /// store's settings ([`UpdateContext::update_options`] for a write lowered to SPARQL
    /// Update's algebra). An error rolls the whole write back.
    fn plan(
        &self,
        tx: &mut Transaction<'_>,
        requester: &crate::Requester,
        context: &UpdateContext<'_>,
    ) -> Result<(), StoreError>;
}

/// The entry point a mutation came from; transport layers use it to map errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Update,
    Tell,
    GraphWrite,
    GraphDelete,
    Restore,
    Statements,
    /// [`MutationCommand::Delta`].
    Delta,
}

#[derive(Debug, Clone)]
pub enum MutationCommitReport {
    /// A SPARQL update, TELL, statements or a front-end's write; `revision` is the dataset
    /// revision after the commit.
    Applied {
        revision: u64,
    },
    GraphWrite(GraphWriteReport),
    GraphDelete(GraphDeleteReport),
    Restore(DatasetRestoreReport),
}

impl MutationCommand {
    pub fn kind(&self) -> MutationKind {
        match self {
            Self::Update(_) => MutationKind::Update,
            Self::Tell(_) => MutationKind::Tell,
            Self::GraphWrite(_) => MutationKind::GraphWrite,
            Self::GraphDelete(_) => MutationKind::GraphDelete,
            Self::Restore(_) => MutationKind::Restore,
            Self::Statements(_) => MutationKind::Statements,
            Self::Delta(_) => MutationKind::Delta,
        }
    }

    /// Applies the command to `tx` for `requester` without committing it; reports carry
    /// revision 0 until [`MutationCommitReport::committed`]. The evaluation token lets the
    /// caller stop a long-running `WHERE` clause.
    ///
    /// Graph-level access control: the commands check what they would change as they go
    /// (for the clearest error); then every graph `tx` changes is checked against the
    /// requester's write scope, so no command, present or future, changes a graph its
    /// requester may not write.
    pub(crate) fn apply(
        &self,
        tx: &mut Transaction<'_>,
        requester: &crate::Requester,
        context: &UpdateContext<'_>,
    ) -> Result<MutationCommitReport, StoreError> {
        match self {
            Self::Restore(_) => requester.write.require_all("a restore")?,
            // The target, whether or not the write changes it: a write of statements the
            // graph has already would otherwise succeed where a new one is refused, telling
            // a requester who may not read the graph what it holds (the review of
            // 3 October 2026, A3).
            Self::Tell(request) => require_writable(requester, &request.target)?,
            Self::GraphWrite(request) => require_writable(requester, &request.target)?,
            Self::GraphDelete(target) => require_writable(requester, target)?,
            Self::Update(_) | Self::Statements(_) | Self::Delta(_) => {}
        }
        let report = self.apply_unchecked(tx, requester, context)?;
        if let Some(writable) = requester.write.access() {
            check_writable(tx, writable)?;
        }
        Ok(report)
    }

    fn apply_unchecked(
        &self,
        tx: &mut Transaction<'_>,
        requester: &crate::Requester,
        context: &UpdateContext<'_>,
    ) -> Result<MutationCommitReport, StoreError> {
        let mut update = |tx: &mut Transaction<'_>, request: &SparqlUpdateRequest| {
            apply_sparql_update(tx, request, requester, context)
        };
        match self {
            Self::Update(request) => {
                update(tx, request)?;
                Ok(MutationCommitReport::Applied { revision: 0 })
            }
            Self::Statements(request) => {
                apply_statements(tx, request, requester, &mut update)?;
                Ok(MutationCommitReport::Applied { revision: 0 })
            }
            Self::Tell(request) => {
                apply_graph_write(tx, &request.as_graph_write())?;
                Ok(MutationCommitReport::Applied { revision: 0 })
            }
            Self::GraphWrite(request) => {
                apply_graph_write(tx, request).map(MutationCommitReport::GraphWrite)
            }
            Self::GraphDelete(target) => {
                apply_graph_delete(tx, target).map(MutationCommitReport::GraphDelete)
            }
            Self::Restore(request) => apply_restore(tx, request).map(MutationCommitReport::Restore),
            Self::Delta(planner) => {
                planner.plan(tx, requester, context)?;
                Ok(MutationCommitReport::Applied { revision: 0 })
            }
        }
    }
}

/// What writes are evaluated with besides their transaction and requester.
pub struct UpdateContext<'a> {
    /// The request's memory envelope within the catalog's budget: the `WHERE` clauses (and
    /// a front-end's patterns) draw from it, as the request's DL gate does.
    pub(crate) memory: nrese_exec::Envelope,
    /// Stops a long-running `WHERE` clause.
    pub(crate) cancellation: &'a CancellationToken,
    pub(crate) union_default_graph: bool,
    /// [`crate::StoreConfig::geosparql_stated_only`].
    pub(crate) geosparql_stated_only: bool,
    pub(crate) services: Option<nrese_sparql::Services>,
    /// The repository's namespaces: what a prefix an update uses without declaring it
    /// means (as for queries, [`crate::PreparedQuery::parse_with`]).
    pub(crate) namespaces: crate::NamespaceMap,
}

impl UpdateContext<'_> {
    /// Fires when the caller cancels the write: a long evaluation stops at it.
    pub fn cancellation(&self) -> &CancellationToken {
        self.cancellation
    }

    /// The repository's namespaces, by prefix.
    pub fn namespaces(&self) -> &crate::NamespaceMap {
        &self.namespaces
    }

    /// What the executor evaluates a write's patterns with: the cancellation and the
    /// request's memory envelope (within the catalog's budget, as its DL gate draws).
    pub fn resources(&self) -> nrese_sparql::Resources {
        nrese_sparql::Resources {
            cancellation: Some(self.cancellation.clone()),
            memory: self.memory.clone(),
            ..Default::default()
        }
    }

    /// The options of a SPARQL update for `requester` over the `using` dataset: what a
    /// write lowered to SPARQL Update's algebra runs [`nrese_sparql::apply_update`] with,
    /// the requester's graph access included.
    pub fn update_options(
        &self,
        requester: &crate::Requester,
        using: Option<nrese_sparql::QueryDatasetSpecification>,
    ) -> UpdateOptions {
        UpdateOptions {
            using,
            resources: self.resources(),
            union_default_graph: self.union_default_graph,
            services: self.services.clone(),
            access: requester.read.access().cloned(),
            writable: requester.write.access().cloned(),
            geosparql_stated_only: self.geosparql_stated_only,
        }
    }
}

/// Applies a SPARQL update request to `tx` for `requester`.
pub(crate) fn apply_sparql_update(
    tx: &mut Transaction<'_>,
    request: &SparqlUpdateRequest,
    requester: &crate::Requester,
    context: &UpdateContext<'_>,
) -> Result<(), StoreError> {
    let update = match SparqlParser::new().parse_update(&request.update) {
        Ok(update) => update,
        Err(error) => {
            let mut parser = SparqlParser::new();
            for (prefix, namespace) in &context.namespaces {
                parser = match parser
                    .clone()
                    .with_prefix(prefix.clone(), namespace.clone())
                {
                    Ok(with) => with,
                    Err(_) => parser,
                };
            }
            // A mistake of its own: the error without the namespaces.
            parser.parse_update(&request.update).map_err(|_| error)?
        }
    };
    let using = crate::query_executor::protocol_dataset(
        &request.using_graphs,
        &request.using_named_graphs,
    )?;
    let options = context.update_options(requester, using);
    // Its WHERE's memory refusal as its reader may be told it (`ReadScope::told`).
    requester.read.told(
        apply_update(tx, &update, &options).map_err(|error| match error {
            nrese_sparql::UpdateError::Forbidden(graph) => crate::Refusal::Write(graph).into(),
            error => StoreError::from(error),
        }),
    )
}

impl MutationCommitReport {
    /// Stamps the revision the commit produced.
    pub(crate) fn committed(self, revision: u64) -> Self {
        match self {
            Self::Applied { .. } => Self::Applied { revision },
            Self::GraphWrite(report) => Self::GraphWrite(GraphWriteReport { revision, ..report }),
            Self::GraphDelete(report) => {
                Self::GraphDelete(GraphDeleteReport { revision, ..report })
            }
            Self::Restore(report) => Self::Restore(DatasetRestoreReport { revision, ..report }),
        }
    }
}

/// Fails if `tx` changes a graph outside `writable`.
fn check_writable(
    tx: &nrese_engine::Transaction<'_>,
    writable: &nrese_sparql::GraphAccess,
) -> Result<(), StoreError> {
    let mut graphs: Vec<nrese_engine::TermId> = tx
        .inserted()
        .chain(tx.deleted())
        .map(|quad| quad.graph)
        .collect();
    graphs.sort_unstable();
    graphs.dedup();
    match graphs
        .into_iter()
        .find(|&graph| !writable.allows_id(tx, graph))
    {
        None => Ok(()),
        Some(graph) => Err(crate::Refusal::Write(graph_name(tx, graph)).into()),
    }
}

/// Refuses a graph-level command whose target `requester` may not write.
fn require_writable(
    requester: &crate::Requester,
    target: &crate::GraphTarget,
) -> Result<(), StoreError> {
    let Some(writable) = requester.write.access() else {
        return Ok(());
    };
    let graph = target.graph_name()?;
    if writable.allows_graph(&graph) {
        Ok(())
    } else {
        Err(crate::Refusal::Write(graph).into())
    }
}

/// The graph `id` of `tx` as a name.
pub(crate) fn graph_name(
    tx: &nrese_engine::Transaction<'_>,
    id: nrese_engine::TermId,
) -> nrese_rdf::GraphName {
    match tx.decode(id) {
        _ if id == nrese_engine::TermId::DEFAULT_GRAPH => nrese_rdf::GraphName::DefaultGraph,
        Some(nrese_rdf::Term::NamedNode(node)) => nrese_rdf::GraphName::NamedNode(node),
        Some(nrese_rdf::Term::BlankNode(node)) => nrese_rdf::GraphName::BlankNode(node),
        _ => nrese_rdf::GraphName::DefaultGraph,
    }
}
