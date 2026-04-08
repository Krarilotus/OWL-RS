use nrese_store::{
    DatasetRestoreReport, DatasetRestoreRequest, GraphDeleteReport, GraphTarget, GraphWriteReport,
    GraphWriteRequest, MutationDeltaPreview, SparqlUpdateRequest, StoreError, StoreService,
};

use crate::error::ApiError;
use crate::policy::PolicyConfig;

#[derive(Debug, Clone)]
pub enum MutationCommand {
    Update(SparqlUpdateRequest),
    GraphWrite(GraphWriteRequest),
    GraphDelete(GraphTarget),
    Restore(DatasetRestoreRequest),
}

#[derive(Debug, Clone)]
pub enum MutationCommitReport {
    Applied,
    GraphWrite(GraphWriteReport),
    GraphDelete(GraphDeleteReport),
    Restore(DatasetRestoreReport),
}

impl MutationCommand {
    pub fn preview(
        &self,
        store: &StoreService,
    ) -> Result<nrese_store::StagedMutationPreview, StoreError> {
        match self {
            Self::Update(request) => store.preview_update(request),
            Self::GraphWrite(request) => store.preview_graph_write(request),
            Self::GraphDelete(target) => store.preview_graph_delete(target),
            Self::Restore(request) => store.preview_restore(request),
        }
    }

    pub fn commit(&self, store: &StoreService) -> Result<MutationCommitReport, StoreError> {
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
        }
    }

    pub fn map_store_error(&self, policy: &PolicyConfig, error: StoreError) -> ApiError {
        match self {
            Self::Update(_) => policy.bad_request_for_sparql_parse_error(error.to_string()),
            Self::GraphWrite(_) | Self::GraphDelete(_) => ApiError::bad_request(error.to_string()),
            Self::Restore(_) => match error {
                StoreError::Loader(_) | StoreError::RdfParse(_) => {
                    ApiError::bad_request(error.to_string())
                }
                other => ApiError::internal(other.to_string()),
            },
        }
    }

    pub fn delta<'a>(
        &self,
        preview: &'a nrese_store::StagedMutationPreview,
    ) -> &'a MutationDeltaPreview {
        &preview.delta
    }
}
