use std::fmt;
use std::path::{Path, PathBuf};

use nrese_engine::{Engine, EngineConfig, ReadModel};
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
use crate::stats::{StoreStats, collect_stats};
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};

/// The dataset and the operations on it. Reads run on an engine snapshot and never wait for
/// writers. Production writes go through [`MutationPipeline`](crate::MutationPipeline),
/// which adds the validation gates; the `execute_*` write methods here apply commands
/// directly (tools, tests, bootstrapping).
#[derive(Clone)]
pub struct StoreService {
    config: StoreConfig,
    engine: Engine,
    preloaded_ontology: Option<PathBuf>,
    /// Whether the reasoning marker file may exist (see [`Self::materialised_for`]); saves
    /// a file-system call per write once it is gone.
    marker: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The ruleset recorded this process (also for in-memory stores, which have no file).
    materialised: std::sync::Arc<std::sync::Mutex<Option<&'static str>>>,
}

/// The file recording which ruleset the inferred stack is exact for.
const REASONING_MARKER: &str = "reasoning.state";

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
        let before = engine.snapshot().revision();
        let preloaded_ontology = preload_ontology(&engine, &config)?;
        let service = Self {
            config,
            engine,
            preloaded_ontology,
            marker: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            materialised: std::sync::Arc::default(),
        };
        if service.engine.snapshot().revision() != before {
            service.invalidate_reasoning()?;
        }
        Ok(service)
    }

    fn marker_path(&self) -> Option<PathBuf> {
        (self.config.mode == StoreMode::OnDisk).then(|| self.config.data_dir.join(REASONING_MARKER))
    }

    /// The ruleset whose closure the inferred stack holds exactly, as recorded by the last
    /// [`Self::rematerialise`] and kept by reasoner-v2 commits since; `None` if unknown
    /// (in-memory stores, or a write that didn't maintain the inferences).
    pub fn materialised_for(&self) -> Option<String> {
        if let Some(ruleset) = *self.materialised.lock().unwrap_or_else(|p| p.into_inner()) {
            return Some(ruleset.to_owned());
        }
        let path = self.marker_path()?;
        std::fs::read_to_string(path)
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|name| !name.is_empty())
    }

    /// Forgets the reasoning marker: called before any write that doesn't maintain the
    /// inferred stack, so a crash can leave it missing but never stale.
    pub fn invalidate_reasoning(&self) -> StoreResult<()> {
        use std::sync::atomic::Ordering;
        *self.materialised.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if !self.marker.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        if let Some(path) = self.marker_path() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(crate::error::StoreError::Io(error)),
            }
        }
        Ok(())
    }

    fn record_reasoning(&self, ruleset: &'static str) -> StoreResult<()> {
        *self.materialised.lock().unwrap_or_else(|p| p.into_inner()) = Some(ruleset);
        let Some(path) = self.marker_path() else {
            return Ok(());
        };
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, ruleset).map_err(crate::error::StoreError::Io)?;
        std::fs::rename(&temporary, &path).map_err(crate::error::StoreError::Io)?;
        self.marker
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Empties the inferred stack (reasoning switched off), forgetting the marker.
    pub fn clear_inferred(&self) -> StoreResult<u64> {
        self.invalidate_reasoning()?;
        let rematerialisation = self.engine.rematerialisation();
        if rematerialisation.base().len_in(ReadModel::Inferred) == 0 {
            return Ok(0);
        }
        Ok(rematerialisation.finish(Vec::new())?.inferred_deleted)
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
        self.invalidate_reasoning()?;
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
        self.invalidate_reasoning()?;
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
        let program =
            crate::reasoning::Program::new(ruleset, &|term| rematerialisation.intern(term));
        let closure = crate::reasoning::materialise(
            &program,
            rematerialisation.base().quads_for_pattern_in(
                nrese_engine::ReadModel::Asserted,
                &nrese_engine::QuadPattern::all(),
            ),
        );
        let inferred = closure.inferred.len() as u64;
        let summary = rematerialisation.finish(closure.inferred)?;
        self.record_reasoning(ruleset.name())?;
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
