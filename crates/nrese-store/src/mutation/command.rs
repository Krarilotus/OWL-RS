use crate::backup::{DatasetRestoreReport, DatasetRestoreRequest};
use crate::error::StoreError;
use crate::graph_store::{GraphDeleteReport, GraphTarget, GraphWriteReport, GraphWriteRequest};
use crate::service::StoreService;
use crate::staging::StagedMutationPreview;
use crate::tell::{TellRequest, compile_tell_update};
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
}

/// The entry point a mutation came from; transport layers use it to map errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Update,
    Tell,
    GraphWrite,
    GraphDelete,
    Restore,
}

#[derive(Debug, Clone)]
pub enum MutationCommitReport {
    Applied,
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
        }
    }

    /// Normalises entry-point specific requests (`TELL` becomes `INSERT DATA`).
    pub(crate) fn normalize(self) -> Result<Self, StoreError> {
        match self {
            Self::Tell(request) => compile_tell_update(&request).map(Self::Update),
            other => Ok(other),
        }
    }

    pub(crate) fn preview(
        &self,
        store: &StoreService,
    ) -> Result<StagedMutationPreview, StoreError> {
        match self {
            Self::Update(request) => store.preview_update(request),
            Self::GraphWrite(request) => store.preview_graph_write(request),
            Self::GraphDelete(target) => store.preview_graph_delete(target),
            Self::Restore(request) => store.preview_restore(request),
            Self::Tell(_) => unreachable!("TELL is normalised to an update before preview"),
        }
    }

    pub(crate) fn commit(&self, store: &StoreService) -> Result<MutationCommitReport, StoreError> {
        match self {
            Self::Update(request) => store
                .execute_update(request)
                .map(|_| MutationCommitReport::Applied),
            Self::GraphWrite(request) => store
                .execute_graph_write(request)
                .map(MutationCommitReport::GraphWrite),
            Self::GraphDelete(target) => store
                .execute_graph_delete(target)
                .map(MutationCommitReport::GraphDelete),
            Self::Restore(request) => store
                .restore_dataset(request)
                .map(MutationCommitReport::Restore),
            Self::Tell(_) => unreachable!("TELL is normalised to an update before commit"),
        }
    }
}
