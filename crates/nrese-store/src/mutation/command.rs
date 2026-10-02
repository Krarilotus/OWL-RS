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
        cancellation: &CancellationToken,
        union_default_graph: bool,
        services: Option<nrese_sparql::Services>,
    ) -> Result<MutationCommitReport, StoreError> {
        if let Self::Restore(_) = self {
            requester.write.require_all("a restore")?;
        }
        let report =
            self.apply_unchecked(tx, requester, cancellation, union_default_graph, services)?;
        if let Some(writable) = requester.write.access() {
            check_writable(tx, writable)?;
        }
        Ok(report)
    }

    fn apply_unchecked(
        &self,
        tx: &mut Transaction<'_>,
        requester: &crate::Requester,
        cancellation: &CancellationToken,
        union_default_graph: bool,
        services: Option<nrese_sparql::Services>,
    ) -> Result<MutationCommitReport, StoreError> {
        let mut update = |tx: &mut Transaction<'_>, request: &SparqlUpdateRequest| {
            apply_sparql_update(
                tx,
                request,
                requester,
                cancellation,
                union_default_graph,
                services.clone(),
            )
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

/// Applies a SPARQL update request to `tx` for `requester`.
pub(crate) fn apply_sparql_update(
    tx: &mut Transaction<'_>,
    request: &SparqlUpdateRequest,
    requester: &crate::Requester,
    cancellation: &CancellationToken,
    union_default_graph: bool,
    services: Option<nrese_sparql::Services>,
) -> Result<(), StoreError> {
    let update = SparqlParser::new().parse_update(&request.update)?;
    let options = UpdateOptions {
        using: crate::query_executor::protocol_dataset(
            &request.using_graphs,
            &request.using_named_graphs,
        )?,
        cancellation: Some(cancellation.clone()),
        union_default_graph,
        services,
        access: requester.read.access().cloned(),
        writable: requester.write.access().cloned(),
    };
    apply_update(tx, &update, &options).map_err(|error| match error {
        nrese_sparql::UpdateError::Forbidden(graph) => StoreError::Forbidden(format!(
            "the update would change {graph}, which the requester may not write"
        )),
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
        Some(_) => Err(StoreError::Forbidden(
            "the request would change a graph the requester may not write".to_owned(),
        )),
    }
}
