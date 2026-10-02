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
}

#[derive(Debug, Clone)]
pub enum MutationCommitReport {
    /// A SPARQL update or TELL; `revision` is the dataset revision after the commit.
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
        if let Self::Restore(_) = self {
            requester.write.require_all("a restore")?;
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
        }
    }
}

/// What updates are evaluated with besides their transaction and requester.
pub(crate) struct UpdateContext<'a> {
    /// Stops a long-running `WHERE` clause.
    pub cancellation: &'a CancellationToken,
    pub union_default_graph: bool,
    pub services: Option<nrese_sparql::Services>,
    /// The repository's namespaces: what a prefix an update uses without declaring it
    /// means (as for queries, [`crate::PreparedQuery::parse_with`]).
    pub namespaces: crate::NamespaceMap,
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
    let options = UpdateOptions {
        using: crate::query_executor::protocol_dataset(
            &request.using_graphs,
            &request.using_named_graphs,
        )?,
        cancellation: Some(context.cancellation.clone()),
        union_default_graph: context.union_default_graph,
        services: context.services.clone(),
        access: requester.read.access().cloned(),
        writable: requester.write.access().cloned(),
    };
    apply_update(tx, &update, &options).map_err(|error| match error {
        nrese_sparql::UpdateError::Forbidden(graph) => crate::Refusal::Write(graph).into(),
        error => error.into(),
    })
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
