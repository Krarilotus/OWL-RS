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
use crate::query_executor::{PreparedQuery, explain_prepared, run_query};
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
    /// Whether the reasoning state file may exist (see [`Self::reasoning_state`]); saves a
    /// file-system call per write once it is gone.
    marker: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The reasoning state recorded this process (also for in-memory stores, which have no
    /// file).
    materialised: std::sync::Arc<std::sync::Mutex<Option<crate::ReasoningState>>>,
    /// The latest full materialisation's report (startup, load, ruleset change).
    last_materialisation:
        std::sync::Arc<std::sync::Mutex<Option<crate::reasoning::MaterialisationReport>>>,
    query_cache: std::sync::Arc<crate::query_cache::QueryCache>,
    /// What every query gets from the store: the default graph's meaning and the budget
    /// all running queries share.
    settings: crate::query_executor::StoreSettings,
}

/// The file recording what the inferred stack is exact for ([`crate::reasoning_state`]).
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
            StoreMode::OnDisk => {
                let defaults = EngineConfig::default();
                let engine_config = EngineConfig {
                    durability: nrese_engine::DurabilityConfig {
                        verify_on_open: config.verify_on_open,
                        map_checkpoints: config.map_checkpoints,
                        bulk_load_memory: (config.bulk_load_memory_bytes > 0)
                            .then_some(config.bulk_load_memory_bytes),
                        ..defaults.durability
                    },
                    ..defaults
                };
                Engine::open(&config.data_dir, engine_config)?
            }
        };
        let before = engine.snapshot().revision();
        let preloaded_ontology = preload_ontology(&engine, &config)?;
        let query_cache = std::sync::Arc::new(crate::query_cache::QueryCache::new(
            config.query_cache_bytes,
        ));
        let settings = crate::query_executor::StoreSettings {
            union_default_graph: config.union_default_graph,
            query_memory: (config.total_query_memory_bytes > 0)
                .then(|| nrese_sparql::SharedBudget::new(config.total_query_memory_bytes)),
            services: std::sync::Arc::default(),
            equality_closed: std::sync::Arc::default(),
        };
        let service = Self {
            query_cache,
            settings,
            config,
            engine,
            preloaded_ontology,
            marker: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            materialised: std::sync::Arc::default(),
            last_materialisation: std::sync::Arc::default(),
        };
        if service.engine.snapshot().revision() != before {
            service.invalidate_reasoning()?;
        }
        if let Some(state) = service.reasoning_state() {
            service.note_equality(&state);
        }
        Ok(service)
    }

    /// Whether the recorded reasoning closes the data under `owl:sameAs` (a ruleset with
    /// the equality rules): queries may then rely on it.
    fn note_equality(&self, state: &crate::ReasoningState) {
        let closed = nrese_reasoner::RuleProgram::closes_equality(&state.ruleset);
        self.settings
            .equality_closed
            .store(closed, std::sync::atomic::Ordering::Release);
    }

    fn marker_path(&self) -> Option<PathBuf> {
        (self.config.mode == StoreMode::OnDisk).then(|| self.config.data_dir.join(REASONING_MARKER))
    }

    /// What the inferred stack is exact for, as recorded by the last
    /// [`Self::rematerialise`] and kept by reasoner-v2 commits since; `None` if unknown
    /// (never materialised, or a write that didn't maintain the inferences).
    pub fn reasoning_state(&self) -> Option<crate::ReasoningState> {
        if let Some(state) = &*self.materialised.lock().unwrap_or_else(|p| p.into_inner()) {
            return Some(state.clone());
        }
        let path = self.marker_path()?;
        let text = std::fs::read_to_string(path).ok()?;
        crate::ReasoningState::from_text(&text)
    }

    /// Whether the data is consistent under the recorded reasoning (see
    /// [`crate::reasoning_state`] for the quarantine this reports).
    pub fn consistency(&self) -> crate::ConsistencyStatus {
        self.reasoning_state()
            .map_or(crate::ConsistencyStatus::Unknown, |state| {
                state.consistency()
            })
    }

    pub fn invalidate_reasoning(&self) -> StoreResult<()> {
        use std::sync::atomic::Ordering;
        self.settings
            .equality_closed
            .store(false, Ordering::Release);
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

    fn record_reasoning(&self, state: crate::ReasoningState) -> StoreResult<()> {
        self.note_equality(&state);
        let text = state.to_text();
        *self.materialised.lock().unwrap_or_else(|p| p.into_inner()) = Some(state);
        let Some(path) = self.marker_path() else {
            return Ok(());
        };
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, text).map_err(crate::error::StoreError::Io)?;
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

    /// The engine's sizes: quads, runs, index and dictionary memory.
    pub fn engine_stats(&self) -> nrese_engine::EngineStats {
        self.engine.stats()
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
        let prepared = PreparedQuery::parse(request)?;
        let mut payload = Vec::new();
        self.run_query(&prepared, &CancellationToken::new(), &mut payload)?;
        Ok(SerializedQueryResult {
            kind: prepared.kind(),
            media_type: prepared.media_type(),
            payload,
        })
    }

    /// Hits, misses and size of the query result cache.
    pub fn query_cache_stats(&self) -> crate::query_cache::QueryCacheStats {
        self.query_cache.stats()
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
        let snapshot = self.engine.snapshot();
        let settings = &self.settings;
        if !self.query_cache.enabled() || prepared.volatile() {
            return run_query(&snapshot, prepared, settings, cancellation, out);
        }
        let key = crate::query_cache::CacheKey {
            request: prepared.cache_request(),
            revision: snapshot.revision(),
        };
        let mut out = out;
        if let Some(bytes) = self.query_cache.get(&key) {
            out.write_all(&bytes)?;
            return Ok(());
        }
        let mut tee = crate::query_cache::Tee {
            inner: out,
            copy: Some(Vec::new()),
            limit: self.query_cache.max_entry(),
        };
        run_query(&snapshot, prepared, settings, cancellation, &mut tee)?;
        if let Some(copy) = tee.copy {
            self.query_cache.insert(key, copy);
        }
        Ok(())
    }

    /// Runs a prepared query on the latest snapshot to completion and reports how it ran:
    /// the executor, each operator with estimated and actual rows, and times.
    pub fn explain_query(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::Explanation> {
        explain_prepared(
            &self.engine.snapshot(),
            prepared,
            &self.settings,
            cancellation,
        )
    }

    /// Bytes of intermediate results the running queries hold now, the most they held at
    /// once, and the limit; `None` without a limit
    /// ([`StoreConfig::total_query_memory_bytes`]).
    pub fn query_memory(&self) -> Option<(usize, usize, usize)> {
        let budget = self.settings.query_memory.as_ref()?;
        Some((budget.used(), budget.peak(), budget.limit()))
    }

    pub fn execute_query_str(&self, query: &str) -> StoreResult<SerializedQueryResult> {
        self.execute_query(&SparqlQueryRequest::new(query))
    }

    pub fn execute_graph_read(&self, request: &GraphReadRequest) -> StoreResult<GraphReadResult> {
        execute_graph_read(&self.engine.snapshot(), request)
    }

    /// The statements matching `pattern` (RDF4J's `GET /statements`): asserted only, or
    /// with the inferred ones.
    pub fn read_statements(
        &self,
        pattern: &crate::StatementPattern,
        infer: bool,
    ) -> StoreResult<Vec<nrese_rdf::Quad>> {
        crate::statements::read_statements(&self.engine.snapshot(), read_model(infer), pattern)
    }

    /// How many statements match `pattern` (RDF4J's `/size`).
    pub fn count_statements(&self, pattern: &crate::StatementPattern, infer: bool) -> u64 {
        crate::statements::count_statements(&self.engine.snapshot(), read_model(infer), pattern)
    }

    /// At most `limit` resources whose labels' or local names' words begin with the words
    /// of `typed`, best first ([`crate::autocomplete`]).
    pub fn autocomplete(
        &self,
        typed: &str,
        limit: usize,
        infer: bool,
    ) -> Vec<crate::autocomplete::Suggestion> {
        crate::autocomplete::autocomplete(&self.engine.snapshot(), read_model(infer), typed, limit)
    }

    /// The named graphs (RDF4J's `/contexts`).
    pub fn contexts(&self) -> Vec<nrese_rdf::Term> {
        crate::statements::contexts(&self.engine.snapshot())
    }

    /// Installs who answers `SERVICE` calls (for every clone of this store); the first
    /// client installed stays. Without one, `SERVICE` is an error.
    pub fn set_service_client(&self, client: std::sync::Arc<dyn nrese_sparql::ServiceClient>) {
        let _ = self.settings.services.set(nrese_sparql::Services(client));
    }

    /// Who answers `SERVICE` calls, if a client is installed.
    pub fn services(&self) -> Option<nrese_sparql::Services> {
        self.settings.services.get().cloned()
    }

    /// Applies and commits `command` without validation gates.
    pub fn apply(&self, command: &MutationCommand) -> StoreResult<MutationCommitReport> {
        self.invalidate_reasoning()?;
        let mut tx = self.engine.transaction();
        let report = command.apply(
            &mut tx,
            &CancellationToken::new(),
            self.config.union_default_graph,
            self.services(),
        )?;
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

    /// Whether the inferred stack is `program`'s closure as this store computes it.
    pub fn reasoning_is_current(&self, program: impl Into<nrese_reasoner::RuleProgram>) -> bool {
        let program = program.into();
        self.reasoning_state()
            .is_some_and(|state| state.is_current_with(&program, self.config.hide_unnamed_classes))
    }

    /// The closure's size with equality replicated and over representatives, for
    /// `program` on the asserted data ([`crate::reasoning::equality_report`]).
    pub fn equality_report(&self, program: impl Into<nrese_reasoner::RuleProgram>) -> String {
        let tx = self.engine.transaction();
        let program = crate::reasoning::Program::new(&program.into(), &|term| tx.intern(term));
        crate::reasoning::equality_report(&program, tx.base())
    }

    /// Replaces the inferred stack with `program`'s closure over the asserted data, as one
    /// revision (see [`crate::reasoning`]). For after bulk loads, at startup and after a
    /// change of rules; commits keep it current afterwards.
    pub fn rematerialise(
        &self,
        program: impl Into<nrese_reasoner::RuleProgram>,
    ) -> StoreResult<crate::reasoning::MaterialisationReport> {
        self.rematerialise_until(program, nrese_reasoner::v2::eval::NEVER)
    }

    /// [`Self::rematerialise`], stopped when `stop` fires: then nothing changes and
    /// [`StoreError::MaterialisationCancelled`](crate::StoreError) is returned.
    pub fn rematerialise_until(
        &self,
        program: impl Into<nrese_reasoner::RuleProgram>,
        stop: nrese_reasoner::v2::eval::Stop<'_>,
    ) -> StoreResult<crate::reasoning::MaterialisationReport> {
        let rules = &program.into();
        let started = std::time::Instant::now();
        let rematerialisation = self.engine.rematerialisation();
        let asserted = rematerialisation
            .base()
            .len_in(nrese_engine::ReadModel::Asserted);
        let program = crate::reasoning::Program::new(rules, &|term| rematerialisation.intern(term))
            .hiding_unnamed_classes(self.config.hide_unnamed_classes);
        let closure = crate::reasoning::materialise_until(&program, rematerialisation.base(), stop)
            .map_err(|_| crate::StoreError::MaterialisationCancelled)?;
        let inferred = closure.inferred.len() as u64;
        let base = rematerialisation.base();
        let reported = crate::reasoning::MaterialisationReport::default()
            .with_diagnostics(&closure.diagnostics, &|id| {
                crate::reasoning::decoded(base.decode(nrese_engine::TermId::from_raw(id)), id)
            });
        crate::reasoning::log_diagnostics(
            &reported.diagnostics,
            reported.diagnostics_total,
            "materialisation skipped an ontology axiom",
        );
        let summary = rematerialisation.finish(closure.inferred)?;
        nrese_engine::memory::release_all();
        self.record_reasoning(crate::ReasoningState::of_with(
            rules,
            self.config.hide_unnamed_classes,
            closure.violations.len(),
        ))?;
        if !closure.violations.is_empty() {
            tracing::error!(
                ruleset = %rules.name(),
                violations = closure.violations.len(),
                "the data is inconsistent: the store is in quarantine until it is repaired"
            );
        }
        tracing::info!(
            ruleset = %rules.name(),
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
        let report = crate::reasoning::MaterialisationReport {
            ruleset: rules.name(),
            revision: summary.revision,
            asserted,
            inferred,
            inferred_inserted: summary.inferred_inserted,
            inferred_deleted: summary.inferred_deleted,
            violations: closure.violations.len(),
            rounds: closure.rounds,
            elapsed: started.elapsed(),
            ..reported
        };
        *self
            .last_materialisation
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(report.clone());
        Ok(report)
    }

    /// The latest full materialisation's report, if one ran since the store opened.
    pub fn last_materialisation(&self) -> Option<crate::reasoning::MaterialisationReport> {
        self.last_materialisation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
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

/// The read model of RDF4J's `infer` parameter.
fn read_model(infer: bool) -> crate::ReadModel {
    match infer {
        true => crate::ReadModel::Materialised,
        false => crate::ReadModel::Asserted,
    }
}
