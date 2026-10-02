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

    /// Applies the command to `tx` without committing it; reports carry revision 0 until
    /// [`MutationCommitReport::committed`]. The evaluation token lets the caller stop a
    /// long-running `WHERE` clause.
    pub(crate) fn apply(
        &self,
        tx: &mut Transaction<'_>,
        cancellation: &CancellationToken,
        union_default_graph: bool,
        services: Option<nrese_sparql::Services>,
    ) -> Result<MutationCommitReport, StoreError> {
        let mut update = |tx: &mut Transaction<'_>, request: &SparqlUpdateRequest| {
            apply_sparql_update(
                tx,
                request,
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
                apply_statements(tx, request, &mut update)?;
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

/// Applies a SPARQL update request to `tx`.
pub(crate) fn apply_sparql_update(
    tx: &mut Transaction<'_>,
    request: &SparqlUpdateRequest,
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
    };
    apply_update(tx, &update, &options)?;
    Ok(())
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
