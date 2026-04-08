use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use oxigraph::store::Store;

use crate::backend::initialize_store;
use crate::backup::{
    DatasetBackupArtifact, DatasetBackupFormat, DatasetRestoreReport, DatasetRestoreRequest,
    export_dataset, restore_dataset,
};
use crate::config::StoreConfig;
use crate::error::StoreResult;
use crate::graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport,
    GraphWriteRequest,
};
use crate::graph_store_executor::{execute_graph_delete, execute_graph_read, execute_graph_write};
use crate::loader::preload_ontology;
use crate::query::{SerializedQueryResult, SparqlQueryRequest};
use crate::query_executor::execute_query;
use crate::snapshot::StoreDatasetSnapshot;
use crate::staging::{
    StagedMutationPreview, snapshot_after_graph_delete, snapshot_after_graph_write,
    snapshot_after_restore, snapshot_after_update,
};
use crate::stats::{StoreStats, collect_stats};
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};
use crate::update_executor::execute_update;

#[derive(Clone)]
pub struct StoreService {
    config: StoreConfig,
    store: Store,
    preloaded_ontology: Option<PathBuf>,
    revision: Arc<AtomicU64>,
}

impl fmt::Debug for StoreService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreService")
            .field("config", &self.config)
            .field("preloaded_ontology", &self.preloaded_ontology)
            .finish()
    }
}

impl StoreService {
    pub fn new(config: StoreConfig) -> StoreResult<Self> {
        config.validate()?;
        let store = initialize_store(&config)?;
        let preloaded_ontology = preload_ontology(&store, &config)?;
        let initial_revision = if collect_stats(&store)?.is_empty {
            0
        } else {
            1
        };

        Ok(Self {
            config,
            store,
            preloaded_ontology,
            revision: Arc::new(AtomicU64::new(initial_revision)),
        })
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    pub fn preloaded_ontology_path(&self) -> Option<&Path> {
        self.preloaded_ontology.as_deref()
    }

    pub fn current_revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub fn stats(&self) -> StoreResult<StoreStats> {
        collect_stats(&self.store)
    }

    pub fn export_dataset(
        &self,
        format: DatasetBackupFormat,
    ) -> StoreResult<DatasetBackupArtifact> {
        export_dataset(&self.store, self.current_revision(), format)
    }

    pub fn restore_dataset(
        &self,
        request: &DatasetRestoreRequest,
    ) -> StoreResult<DatasetRestoreReport> {
        let mut report = restore_dataset(&self.store, request, self.current_revision() + 1)?;
        report.revision = self.bump_revision();
        Ok(report)
    }

    pub fn dataset_snapshot(&self) -> StoreResult<StoreDatasetSnapshot> {
        StoreDatasetSnapshot::capture(&self.store, self.current_revision())
    }

    pub fn execute_query(
        &self,
        request: &SparqlQueryRequest,
    ) -> StoreResult<SerializedQueryResult> {
        execute_query(&self.store, request)
    }

    pub fn execute_query_str(&self, query: &str) -> StoreResult<SerializedQueryResult> {
        self.execute_query(&SparqlQueryRequest::new(query))
    }

    pub fn execute_update(
        &self,
        request: &SparqlUpdateRequest,
    ) -> StoreResult<UpdateExecutionReport> {
        let mut report = execute_update(&self.store, request)?;
        report.revision = self.bump_revision();
        Ok(report)
    }

    pub fn execute_update_str(&self, update: &str) -> StoreResult<UpdateExecutionReport> {
        self.execute_update(&SparqlUpdateRequest::new(update))
    }

    pub fn preview_update_snapshot(
        &self,
        request: &SparqlUpdateRequest,
    ) -> StoreResult<StoreDatasetSnapshot> {
        Ok(self.preview_update(request)?.snapshot)
    }

    pub fn preview_update(
        &self,
        request: &SparqlUpdateRequest,
    ) -> StoreResult<StagedMutationPreview> {
        snapshot_after_update(&self.store, request, self.current_revision() + 1)
    }

    pub fn preview_graph_write(
        &self,
        request: &GraphWriteRequest,
    ) -> StoreResult<StagedMutationPreview> {
        snapshot_after_graph_write(&self.store, request, self.current_revision() + 1)
    }

    pub fn preview_graph_delete(&self, target: &GraphTarget) -> StoreResult<StagedMutationPreview> {
        snapshot_after_graph_delete(&self.store, target, self.current_revision() + 1)
    }

    pub fn preview_restore(
        &self,
        request: &DatasetRestoreRequest,
    ) -> StoreResult<StagedMutationPreview> {
        snapshot_after_restore(&self.store, request, self.current_revision() + 1)
    }

    pub fn execute_graph_read(&self, request: &GraphReadRequest) -> StoreResult<GraphReadResult> {
        execute_graph_read(&self.store, request)
    }

    pub fn execute_graph_write(
        &self,
        request: &GraphWriteRequest,
    ) -> StoreResult<GraphWriteReport> {
        let mut report = execute_graph_write(&self.store, request)?;
        report.revision = self.bump_revision();
        Ok(report)
    }

    pub fn execute_graph_delete(&self, target: &GraphTarget) -> StoreResult<GraphDeleteReport> {
        let mut report = execute_graph_delete(&self.store, target)?;
        report.revision = self.bump_revision();
        Ok(report)
    }

    fn bump_revision(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::AcqRel) + 1
    }
}
