use std::fmt;
use std::path::{Path, PathBuf};

use nrese_engine::{Engine, EngineConfig, QuadPattern, ReadModel};
use nrese_sparql::CancellationToken;

use crate::backup::{
    DatasetBackupArtifact, DatasetBackupFormat, DatasetRestoreReport, DatasetRestoreRequest,
    export_dataset,
};
use crate::bulk_load::{BulkLoadReport, BulkLoadRequest, bulk_load};
use crate::config::{StoreConfig, StoreMode};
use crate::error::StoreResult;
use crate::graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport,
    GraphWriteRequest,
};
use crate::graph_store_executor::execute_graph_read;
use crate::loader::preload_ontology;
use crate::mutation::{MutationCommand, MutationCommitReport};
use crate::query::{SerializedQueryResult, SparqlQueryRequest};
use crate::query_executor::{PreparedQuery, execute_query, run_query};
use crate::snapshot::StoreDatasetSnapshot;
use crate::stats::{StoreStats, collect_stats};
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};
use crate::view::decoded_quads;

/// The dataset and the operations on it. Reads run on an engine snapshot and never wait for
/// writers. Production writes go through [`MutationPipeline`](crate::MutationPipeline),
/// which adds the validation gates; the `execute_*` write methods here apply commands
/// directly (tools, tests, bootstrapping).
#[derive(Clone)]
pub struct StoreService {
    config: StoreConfig,
    engine: Engine,
    preloaded_ontology: Option<PathBuf>,
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
        let engine = match config.mode {
            StoreMode::InMemory => Engine::new(EngineConfig::default())?,
            StoreMode::OnDisk => Engine::open(&config.data_dir, EngineConfig::default())?,
        };
        let preloaded_ontology = preload_ontology(&engine, &config)?;
        Ok(Self {
            config,
            engine,
            preloaded_ontology,
        })
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    pub fn preloaded_ontology_path(&self) -> Option<&Path> {
        self.preloaded_ontology.as_deref()
    }

    /// Revision of the latest commit. Persistent in on-disk mode; unchanged by commits
    /// without net effect.
    pub fn current_revision(&self) -> u64 {
        self.engine.snapshot().revision()
    }

    pub fn stats(&self) -> StoreResult<StoreStats> {
        Ok(collect_stats(&self.engine.snapshot()))
    }

    pub fn export_dataset(
        &self,
        format: DatasetBackupFormat,
    ) -> StoreResult<DatasetBackupArtifact> {
        export_dataset(&self.engine.snapshot(), format)
    }

    /// The asserted dataset in the v1 reasoner's string form. O(dataset).
    pub fn dataset_snapshot(&self) -> StoreResult<StoreDatasetSnapshot> {
        let snapshot = self.engine.snapshot();
        StoreDatasetSnapshot::capture(
            decoded_quads(&snapshot, ReadModel::Asserted, &QuadPattern::all()),
            snapshot.revision(),
        )
    }

    pub fn execute_query(
        &self,
        request: &SparqlQueryRequest,
    ) -> StoreResult<SerializedQueryResult> {
        execute_query(&self.engine.snapshot(), request)
    }

    /// Evaluates a prepared query on the latest snapshot, writing results to `out` as they
    /// are produced. Cancelling `cancellation` stops evaluation promptly; the output is then
    /// incomplete.
    pub fn run_query(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
    ) -> StoreResult<()> {
        run_query(&self.engine.snapshot(), prepared, cancellation, out)
    }

    pub fn execute_query_str(&self, query: &str) -> StoreResult<SerializedQueryResult> {
        self.execute_query(&SparqlQueryRequest::new(query))
    }

    pub fn execute_graph_read(&self, request: &GraphReadRequest) -> StoreResult<GraphReadResult> {
        execute_graph_read(&self.engine.snapshot(), request)
    }

    /// Applies and commits `command` without validation gates.
    pub fn apply(&self, command: &MutationCommand) -> StoreResult<MutationCommitReport> {
        let mut tx = self.engine.transaction();
        let report = command.apply(&mut tx, &CancellationToken::new())?;
        let summary = tx.commit()?;
        Ok(report.committed(summary.revision))
    }

    pub fn execute_update(
        &self,
        request: &SparqlUpdateRequest,
    ) -> StoreResult<UpdateExecutionReport> {
        match self.apply(&MutationCommand::Update(request.clone()))? {
            MutationCommitReport::Applied { revision } => Ok(UpdateExecutionReport {
                applied: true,
                revision,
            }),
            other => unreachable!("update produced {other:?}"),
        }
    }

    pub fn execute_update_str(&self, update: &str) -> StoreResult<UpdateExecutionReport> {
        self.execute_update(&SparqlUpdateRequest::new(update))
    }

    pub fn execute_graph_write(
        &self,
        request: &GraphWriteRequest,
    ) -> StoreResult<GraphWriteReport> {
        match self.apply(&MutationCommand::GraphWrite(request.clone()))? {
            MutationCommitReport::GraphWrite(report) => Ok(report),
            other => unreachable!("graph write produced {other:?}"),
        }
    }

    pub fn execute_graph_delete(&self, target: &GraphTarget) -> StoreResult<GraphDeleteReport> {
        match self.apply(&MutationCommand::GraphDelete(target.clone()))? {
            MutationCommitReport::GraphDelete(report) => Ok(report),
            other => unreachable!("graph delete produced {other:?}"),
        }
    }

    /// Loads RDF files through the engine's bulk path: parallel parsing and interning, one
    /// sort, one revision, no validation gates. For initial loads and full restores; use
    /// the mutation pipeline for regular writes.
    pub fn bulk_load(&self, request: &BulkLoadRequest) -> StoreResult<BulkLoadReport> {
        bulk_load(&self.engine, request)
    }

    /// Replaces the inferred stack with `ruleset`'s closure over the asserted data, as one
    /// revision (see [`crate::reasoning`]). For after bulk loads, at startup and after a
    /// ruleset change; commits keep it current afterwards.
    pub fn rematerialise(
        &self,
        ruleset: nrese_reasoner::v2::rulesets::Ruleset,
    ) -> StoreResult<crate::reasoning::MaterialisationReport> {
        let started = std::time::Instant::now();
        let rematerialisation = self.engine.rematerialisation();
        let asserted = rematerialisation
            .base()
            .len_in(nrese_engine::ReadModel::Asserted);
        let closure = crate::reasoning::materialise(
            ruleset,
            rematerialisation.base().quads_for_pattern_in(
                nrese_engine::ReadModel::Asserted,
                &nrese_engine::QuadPattern::all(),
            ),
            &|term| rematerialisation.intern(term),
        );
        let inferred = closure.inferred.len() as u64;
        let summary = rematerialisation.finish(closure.inferred)?;
        tracing::info!(
            ruleset = ruleset.name(),
            revision = summary.revision,
            asserted,
            inferred,
            inferred_inserted = summary.inferred_inserted,
            inferred_deleted = summary.inferred_deleted,
            violations = closure.violations.len(),
            rounds = closure.rounds,
            ms = started.elapsed().as_millis() as u64,
            "inferred stack rematerialised"
        );
        Ok(crate::reasoning::MaterialisationReport {
            ruleset: ruleset.name(),
            revision: summary.revision,
            asserted,
            inferred,
            inferred_inserted: summary.inferred_inserted,
            inferred_deleted: summary.inferred_deleted,
            violations: closure.violations.len(),
            rounds: closure.rounds,
            elapsed: started.elapsed(),
        })
    }

    pub fn restore_dataset(
        &self,
        request: &DatasetRestoreRequest,
    ) -> StoreResult<DatasetRestoreReport> {
        match self.apply(&MutationCommand::Restore(request.clone()))? {
            MutationCommitReport::Restore(report) => Ok(report),
            other => unreachable!("restore produced {other:?}"),
        }
    }
}
