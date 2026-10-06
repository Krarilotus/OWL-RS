use std::fmt;
use std::path::{Path, PathBuf};

use nrese_engine::{Engine, EngineConfig, ReadModel, TermId};
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
use crate::query_executor::{PreparedQuery, explain_prepared, plan_prepared, run_query};
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
    /// Namespace prefixes for clients ([`crate::namespaces`]).
    namespaces: std::sync::Arc<crate::namespaces::Namespaces>,
    /// Client transactions ([`crate::sessions`]).
    sessions: std::sync::Arc<crate::sessions::Sessions>,
    /// The queries running now ([`crate::running`]).
    running: std::sync::Arc<crate::running::RunningQueries>,
    /// Inferred statements under graph access ([`crate::support`]).
    supports: std::sync::Arc<crate::support::Supports>,
    /// The `owl2-dl` mode's state ([`crate::dl`]).
    dl: std::sync::Arc<crate::dl::Dl>,
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
        nrese_engine::set_index_encoding(config.index_encoding);
        nrese_engine::set_vocabulary_encoding(config.vocabulary);
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
                        wal_archive: config.wal_archive,
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
            geosparql_stated_only: config.geosparql_stated_only,
            query_memory: (config.total_query_memory_bytes > 0)
                .then(|| nrese_sparql::SharedBudget::new(config.total_query_memory_bytes)),
            services: std::sync::Arc::default(),
            equality_closed: std::sync::Arc::default(),
            equality_canonical: config.equality_canonical_answers,
            equality_early_expansion: config.equality_early_expansion,
        };
        let namespaces = crate::namespaces::Namespaces::open(
            (config.mode == StoreMode::OnDisk).then(|| config.data_dir.clone()),
        );
        let service = Self {
            query_cache,
            settings,
            namespaces: std::sync::Arc::new(namespaces),
            sessions: std::sync::Arc::default(),
            running: std::sync::Arc::default(),
            supports: std::sync::Arc::default(),
            dl: std::sync::Arc::default(),
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
            // Kept in memory too: reads ask for it per query ([`Self::status_without_dl`]).
            *service
                .materialised
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(state);
        }
        Ok(service)
    }

    /// Whether the recorded reasoning closes the data under `owl:sameAs` (a ruleset with
    /// the equality rules): queries may then rely on it.
    ///
    /// A stack kept over representatives (`reasoner.equality = "compact"`) is read
    /// expanded: the engine learns `owl:sameAs` ([`nrese_engine::Engine::set_equality`]).
    fn note_equality(&self, state: &crate::ReasoningState) {
        let closed = nrese_reasoner::RuleProgram::closes_equality(&state.ruleset);
        self.settings
            .equality_closed
            .store(closed, std::sync::atomic::Ordering::Release);
        let same_as = (closed && state.compact_equality)
            .then(|| {
                self.engine.snapshot().lookup(
                    nrese_rdf::NamedNodeRef::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
                        .into(),
                )
            })
            .flatten();
        self.engine.set_equality(same_as);
    }

    /// The store's namespace prefixes ([`crate::namespaces`]).
    pub fn namespaces(&self) -> &crate::namespaces::Namespaces {
        &self.namespaces
    }

    /// The store's client transactions ([`crate::sessions`]).
    pub fn sessions(&self) -> &crate::sessions::Sessions {
        &self.sessions
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
                Err(error) => {
                    // The file may still be there: the next invalidation tries again (the
                    // review of 3 October 2026, A4).
                    self.marker.store(true, Ordering::Release);
                    return Err(crate::error::StoreError::Io(error));
                }
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
        self.engine.set_equality(None);
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

    /// Every statement in `format` (a backup); for a scope that reads every graph.
    pub fn export_dataset(
        &self,
        scope: &crate::ReadScope,
        format: DatasetBackupFormat,
    ) -> StoreResult<DatasetBackupArtifact> {
        scope.require_all("an export")?;
        export_dataset(&self.engine.snapshot(), format)
    }

    /// What this store's updates are evaluated with.
    fn update_context<'a>(
        &self,
        cancellation: &'a CancellationToken,
    ) -> crate::mutation::command::UpdateContext<'a> {
        crate::mutation::command::UpdateContext {
            cancellation,
            union_default_graph: self.config.union_default_graph,
            geosparql_stated_only: self.config.geosparql_stated_only,
            services: self.services(),
            namespaces: self.namespaces.all(),
        }
    }

    /// `request` parsed for this store: a prefix it uses without declaring it means what the
    /// store's namespaces bind it to ([`PreparedQuery::parse_with`]).
    /// Results that may hold RDF 1.2 terms (the query makes or matches them, or the store
    /// holds triple terms) announce it ([`PreparedQuery::announce_rdf12`]).
    pub fn prepare_query(&self, request: &SparqlQueryRequest) -> StoreResult<PreparedQuery> {
        let mut prepared = PreparedQuery::parse_with(request, Some(&self.namespaces.all()))?;
        if prepared.uses_rdf12()
            || self
                .engine
                .snapshot()
                .holds_triple_terms(prepared.read_model())
        {
            prepared.announce_rdf12();
        }
        Ok(prepared)
    }

    /// The latest snapshot as a reader with `access` sees it (`None`: every graph): with
    /// `inferred = "supported"`, only the inferred statements its graphs support
    /// ([`crate::support`]).
    pub fn read_snapshot(
        &self,
        access: Option<&std::sync::Arc<nrese_sparql::GraphAccess>>,
    ) -> nrese_engine::Snapshot {
        self.view_of(&self.engine.snapshot(), access)
    }

    /// [`Self::read_snapshot`] of `snapshot`, a committed revision.
    fn view_of(
        &self,
        snapshot: &nrese_engine::Snapshot,
        access: Option<&std::sync::Arc<nrese_sparql::GraphAccess>>,
    ) -> nrese_engine::Snapshot {
        match access {
            Some(access) if by_support(access) => {
                self.supports
                    .view(snapshot, access, self.support_compilation())
            }
            _ => snapshot.clone(),
        }
    }

    fn support_compilation(&self) -> crate::support::Compilation {
        crate::support::Compilation {
            hide_unnamed_classes: self.config.hide_unnamed_classes,
            by_representatives: self.config.equality_by_representatives,
            compact: self.config.equality_compact,
            cap: self.config.support_sets,
        }
    }

    /// Computes the support graph sets of the latest revision on a thread of their own,
    /// so the first read under `inferred = "supported"` doesn't wait for them (at start,
    /// when the policy turns to it; the store does it itself after a rematerialisation
    /// while readers use them). Returns at once.
    pub fn prepare_support_sets(&self) {
        self.supports
            .prepare(self.engine.snapshot(), self.support_compilation());
    }

    /// Inferred statements under graph access ([`crate::support`]): the mutation pipeline
    /// reports its commits to it.
    pub(crate) fn supports(&self) -> &crate::support::Supports {
        &self.supports
    }

    /// How the support graph sets of [`Self::read_snapshot`] were obtained so far.
    pub fn support_statistics(&self) -> crate::support::SupportStatistics {
        self.supports.statistics()
    }

    /// The rules the inferred stack is maintained with (reasoner v2; `None`: none), for
    /// the support graph sets of [`Self::read_snapshot`]. The mutation pipeline registers
    /// its own.
    pub fn use_reasoning_rules(&self, rules: Option<nrese_reasoner::RuleProgram>) {
        self.supports.use_rules(rules);
    }

    /// Switches the `owl2-dl` mode's work on or off ([`crate::dl`]): the mutation
    /// pipeline running the reasoner's `owl2-dl` mode registers it, and whether user rules
    /// run with it.
    pub fn use_dl(&self, active: bool, user_rules: bool) {
        self.dl.set_active(active, user_rules);
    }

    /// The `owl2-dl` mode's state.
    pub fn dl(&self) -> &crate::dl::Dl {
        &self.dl
    }

    /// The `owl2-dl` mode's bounds at the latest revision ([`crate::dl::bounds`]): the
    /// upper bound U1 built afresh if it doesn't describe that revision.
    pub fn dl_bounds(&self) -> crate::dl::BoundsReport {
        crate::dl::bounds::report(self)
    }

    /// U1's facts beyond L, as maintained or (`afresh`) evaluated anew; for the
    /// differential tests of its maintenance.
    #[doc(hidden)]
    pub fn dl_upper_facts(&self, afresh: bool) -> Option<Vec<[String; 3]>> {
        crate::dl::bounds::upper_facts(self, afresh)
    }

    pub fn execute_query(
        &self,
        request: &SparqlQueryRequest,
    ) -> StoreResult<SerializedQueryResult> {
        let prepared = self.prepare_query(request)?;
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
        self.run_query_reporting(prepared, cancellation, out)
            .map(|_| ())
    }

    /// [`Self::run_query`], with whether the answers are complete where a reasoning path
    /// can leave some out (`None` where none can): under `owl2-dl`, every query that reads
    /// the inferred statements goes through the bounds ([`crate::dl`]); its answers are
    /// computed before the first byte is written.
    pub fn run_query_reporting(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
    ) -> StoreResult<Option<nrese_sparql::Completeness>> {
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        let mut dl_status = self.status_without_dl(prepared);
        if let Some(mode) = self.dl_mode(prepared) {
            match crate::dl::query::answer(self, prepared, cancellation, mode)? {
                crate::dl::query::Outcome::Answers(answers, status) => {
                    crate::query_executor::write_answers(prepared, answers, out)?;
                    return Ok(Some(status));
                }
                // Over the lower bound as every query runs, its status known already.
                crate::dl::query::Outcome::Stream(status, None) => dl_status = Some(status),
                // L adds memberships to the snapshot: streamed over its view, uncached.
                crate::dl::query::Outcome::Stream(status, Some(lower)) => {
                    run_query(&lower, prepared, &self.settings, cancellation, out)?;
                    return Ok(Some(status));
                }
            }
        }
        let snapshot = self.read_snapshot(prepared.access());
        let settings = &self.settings;
        if !self.query_cache.enabled() || prepared.volatile() {
            return run_query(&snapshot, prepared, settings, cancellation, out).map(|()| dl_status);
        }
        let key = crate::query_cache::CacheKey {
            request: prepared.cache_request(),
            revision: snapshot.revision(),
        };
        let mut out = out;
        if let Some(bytes) = self.query_cache.get(&key) {
            out.write_all(&bytes)?;
            return Ok(dl_status);
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
        Ok(dl_status)
    }

    /// Outside `owl2-dl`, the status of answers that read a ruleset's closure (`None`
    /// without reasoning, or for asserted statements only): sound, and complete only for
    /// what the ruleset derives, never certain answers under OWL 2 DL. Known before the
    /// query runs.
    pub fn status_without_dl(
        &self,
        prepared: &PreparedQuery,
    ) -> Option<nrese_sparql::Completeness> {
        if self.dl.active() || prepared.read_model() == ReadModel::Asserted {
            return None;
        }
        let ruleset = self
            .materialised
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()?
            .ruleset
            .clone();
        Some(nrese_sparql::Completeness::sound_only(format!(
            "answers over the {ruleset} closure: what its rules derive, not every certain \
             answer under OWL 2 DL (reasoner.mode = \"owl2-dl\" gives those)"
        )))
    }

    /// Whether [`Self::run_query_reporting`] computes `prepared`'s answers in full before
    /// writing them (under `owl2-dl`, to complete them through the bounds).
    pub fn reports_completeness(&self, prepared: &PreparedQuery) -> bool {
        self.dl_mode(prepared).is_some()
    }

    /// Under `owl2-dl`, the answers a query reading the inferred statements asks for;
    /// `None` for any other query (or mode).
    fn dl_mode(&self, prepared: &PreparedQuery) -> Option<crate::DlAnswers> {
        (self.dl.active() && prepared.read_model() == ReadModel::Materialised)
            .then(|| prepared.dl_answers().unwrap_or(self.config.dl.answers))
    }

    /// What every query gets from the store.
    pub(crate) fn query_settings(&self) -> &crate::query_executor::StoreSettings {
        &self.settings
    }

    /// Runs a prepared query on the latest snapshot to completion and reports how it ran:
    /// the executor, each operator with estimated and actual rows, and times.
    pub fn explain_query(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::Explanation> {
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        let mut explanation = explain_prepared(
            &self.read_snapshot(prepared.access()),
            prepared,
            &self.settings,
            cancellation,
        )?;
        if let Some(mode) = self.dl_mode(prepared) {
            // The status as the query would answer, never failing for it.
            let mode = match mode {
                crate::DlAnswers::Exact => crate::DlAnswers::CertainWhereComplete,
                other => other,
            };
            explanation.completeness = Some(
                match crate::dl::query::answer(self, prepared, cancellation, mode)? {
                    crate::dl::query::Outcome::Answers(_, status)
                    | crate::dl::query::Outcome::Stream(status, _) => status,
                },
            );
        }
        Ok(explanation)
    }

    /// The plan `prepared` would run as, each node with its estimated rows, without
    /// running it (EXPLAIN without ANALYZE).
    pub fn plan_query(&self, prepared: &PreparedQuery) -> StoreResult<crate::PlannedQuery> {
        let mut planned = plan_prepared(
            &self.read_snapshot(prepared.access()),
            prepared,
            &self.settings,
        )?;
        if self.dl_mode(prepared).is_some() {
            planned.completeness = Some(crate::dl::query::plan_status(self, prepared));
        }
        Ok(planned)
    }

    /// Bytes of intermediate results the running queries hold now, the most they held at
    /// once, and the limit; `None` without a limit
    /// ([`StoreConfig::total_query_memory_bytes`]).
    pub fn query_memory(&self) -> Option<(usize, usize, usize)> {
        let budget = self.settings.query_memory.as_ref()?;
        Some((budget.used(), budget.peak(), budget.limit()))
    }

    /// A query over every graph (the server's own work, tests).
    pub fn execute_query_str(&self, query: &str) -> StoreResult<SerializedQueryResult> {
        self.execute_query(&SparqlQueryRequest::new(query, crate::ReadScope::All))
    }

    /// A graph (Graph Store Protocol `GET`) as the read sees it: a graph its scope doesn't
    /// read doesn't exist for it.
    pub fn execute_graph_read(
        &self,
        read: &crate::ReadContext<'_>,
        request: &GraphReadRequest,
    ) -> StoreResult<GraphReadResult> {
        if let Some(access) = read.scope.access() {
            let graph = request.target.graph_name()?;
            if !access.allows_graph(&graph) {
                return Ok(GraphReadResult {
                    media_type: request.format.media_type(),
                    payload: Vec::new(),
                    exists: false,
                });
            }
        }
        execute_graph_read(
            &self.read_snapshot(read.scope.access()),
            request,
            read.model(),
            &read.cancel,
        )
    }

    /// Runs `read` on the data as it would be after `pending` (an RDF4J transaction's
    /// operations), which are applied to a speculative transaction: never committed, so no
    /// writer slot is taken.
    /// In a session, the result is kept for its next read on the same data
    /// ([`crate::sessions`]); `scope` is the reader's, by which the operations were scoped.
    fn with_pending<T>(
        &self,
        pending: &crate::StatementsRequest,
        scope: &crate::ReadScope,
        cancellation: &CancellationToken,
        read: impl FnOnce(&nrese_engine::Snapshot) -> StoreResult<T>,
    ) -> StoreResult<T> {
        if let Some(view) = self.sessions.view(&self.engine.snapshot(), scope, pending) {
            return read(&view);
        }
        // Never committed: no writer slot, so other clients commit meanwhile. The operations
        // read as the reader may; what they change is checked at the commit (the view is
        // the reader's own, and shows the graphs it may read only).
        let mut tx = self.engine.speculative();
        let context = self.update_context(cancellation);
        let requester = crate::Requester::new(scope.clone(), crate::WriteScope::All);
        crate::statements::apply_statements(&mut tx, pending, &requester, &mut |tx, request| {
            crate::mutation::command::apply_sparql_update(tx, request, &requester, &context)
        })?;
        let view = match scope.access() {
            Some(access) if by_support(access) => {
                // The operations aren't reasoned over: the base's inferred statements, as
                // far as the reader sees them and the operations left them.
                let base = self.view_of(tx.base(), Some(access));
                let kept: Vec<nrese_engine::EncodedQuad> = base
                    .quads_for_pattern_in(ReadModel::Inferred, &nrese_engine::QuadPattern::all())
                    .filter(|quad| view_has(&tx, quad))
                    .collect();
                tx.pending_snapshot()
                    .with_inferred_subset(nrese_engine::InferredSubset::Only(kept))
            }
            _ => tx.pending_snapshot(),
        };
        self.sessions
            .keep_view(tx.base().clone(), scope, pending, view.clone());
        read(&view)
    }

    /// [`run_query`](Self::run_query) on the data as `pending` would leave it.
    pub fn run_query_pending(
        &self,
        pending: &crate::StatementsRequest,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
    ) -> StoreResult<()> {
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        let scope = crate::ReadScope::of(prepared.access().cloned());
        self.with_pending(pending, &scope, cancellation, |snapshot| {
            run_query(snapshot, prepared, &self.settings, cancellation, out)
        })
    }

    /// The queries running now ([`crate::running`]).
    pub fn running_queries(&self) -> &crate::running::RunningQueries {
        &self.running
    }

    /// Every graph with statements and its number of asserted statements, the default
    /// graph first (as `None`), then the named graphs `access` lets the requester read
    /// (every one with `None`), in IRI order.
    pub fn graph_sizes(
        &self,
        scope: &crate::ReadScope,
    ) -> Vec<(Option<nrese_rdf::NamedOrBlankNode>, u64)> {
        let access = scope.access().map(|access| &**access);
        use nrese_engine::{GraphSelector, QuadPattern};
        let snapshot = self.engine.snapshot();
        let counts: Vec<(TermId, u64)> = snapshot
            .group_counts_in(
                ReadModel::Asserted,
                &QuadPattern::all(),
                nrese_engine::quad::Permutation::Gspo,
            )
            .unwrap_or_else(|| {
                std::iter::once(TermId::DEFAULT_GRAPH)
                    .chain(snapshot.named_graphs())
                    .map(|graph| {
                        let pattern = QuadPattern {
                            graph: GraphSelector::Exact(graph),
                            ..QuadPattern::all()
                        };
                        (graph, snapshot.count_in(ReadModel::Asserted, &pattern))
                    })
                    .collect()
            });
        let mut sizes: Vec<(Option<nrese_rdf::NamedOrBlankNode>, u64)> = Vec::new();
        for (graph, count) in counts {
            if count == 0 {
                continue;
            }
            if graph == TermId::DEFAULT_GRAPH {
                if access.is_none_or(|access| access.default_graph) {
                    sizes.insert(0, (None, count));
                }
                continue;
            }
            let name = match snapshot.decode(graph) {
                Some(nrese_rdf::Term::NamedNode(node)) => {
                    if access.is_some_and(|access| !access.allows(node.as_str())) {
                        continue;
                    }
                    nrese_rdf::NamedOrBlankNode::NamedNode(node)
                }
                Some(nrese_rdf::Term::BlankNode(node)) if access.is_none() => {
                    nrese_rdf::NamedOrBlankNode::BlankNode(node)
                }
                _ => continue,
            };
            sizes.push((Some(name), count));
        }
        let named = usize::from(sizes.first().is_some_and(|(graph, _)| graph.is_none()));
        sizes[named..].sort_by_cached_key(|(graph, _)| graph.as_ref().map(ToString::to_string));
        sizes
    }

    /// The statements matching `pattern` (RDF4J's `GET /statements`) as `read` sees them.
    pub fn statements(
        &self,
        read: &crate::ReadContext<'_>,
        pattern: &crate::StatementPattern,
    ) -> StoreResult<Vec<nrese_rdf::Quad>> {
        self.on_data(read, |snapshot| {
            crate::statements::read_statements(snapshot, read.model(), pattern, &read.scope)
        })
    }

    /// [`Self::statements`] written to `out` in `format` as they are read (no list of them
    /// is kept: a whole repository streams in constant memory). Returns how many.
    pub fn write_statements(
        &self,
        read: &crate::ReadContext<'_>,
        pattern: &crate::StatementPattern,
        format: crate::GraphResultFormat,
        out: impl std::io::Write,
    ) -> StoreResult<u64> {
        self.on_data(read, |snapshot| {
            crate::statements::write_statements(
                snapshot,
                read.model(),
                pattern,
                &read.scope,
                format,
                &read.cancel,
                out,
            )
        })
    }

    /// How many statements match `pattern` (RDF4J's `/size`) as `read` sees them.
    pub fn count(
        &self,
        read: &crate::ReadContext<'_>,
        pattern: &crate::StatementPattern,
    ) -> StoreResult<u64> {
        self.on_data(read, |snapshot| {
            Ok(crate::statements::count_statements(
                snapshot,
                read.model(),
                pattern,
                &read.scope,
            ))
        })
    }

    /// Runs `read` on the latest snapshot, or on the data as the context's pending changes
    /// would leave it.
    fn on_data<T>(
        &self,
        context: &crate::ReadContext<'_>,
        read: impl FnOnce(&nrese_engine::Snapshot) -> StoreResult<T>,
    ) -> StoreResult<T> {
        match context.pending {
            None => read(&self.read_snapshot(context.scope.access())),
            Some(pending) => self.with_pending(pending, &context.scope, &context.cancel, read),
        }
    }

    /// Writes an image backup of the latest snapshot into `dir` ([`crate::image_backup`]).
    pub fn backup_image(&self, dir: &std::path::Path) -> StoreResult<crate::ImageManifest> {
        crate::image_backup::backup_image(&self.engine, dir)
    }

    /// A read replica's feed: the committed records after revision `after`, at most about
    /// `max_bytes` ([`nrese_engine::Engine::log_since`]). An error for a log that no
    /// longer holds them (`EngineError::LogTruncated`): the replica starts from an image.
    pub fn replication_log(
        &self,
        after: u64,
        max_bytes: usize,
    ) -> StoreResult<nrese_engine::LogBatch> {
        self.engine
            .log_since(after, max_bytes)
            .map_err(crate::StoreError::Engine)
    }

    /// Applies a primary's records on this store, a read replica: each becomes a commit
    /// with the primary's revision ([`nrese_engine::Engine::apply_log`]). Returns the
    /// revision reached. Support graph sets start afresh (the records don't say what they
    /// touched in the sets' terms); nothing is reasoned here, the records carry the
    /// inferences.
    pub fn apply_replication_log(&self, frames: &[u8]) -> StoreResult<u64> {
        let revision = self
            .engine
            .apply_log(frames)
            .map_err(crate::StoreError::Engine)?;
        self.supports().forget();
        Ok(revision)
    }

    /// At most `limit` resources whose labels' or local names' words begin with the words
    /// of `typed`, best first ([`crate::autocomplete`]); for a scope that reads every graph.
    pub fn autocomplete(
        &self,
        scope: &crate::ReadScope,
        typed: &str,
        limit: usize,
        infer: bool,
    ) -> StoreResult<Vec<crate::autocomplete::Suggestion>> {
        scope.require_all("autocompletion")?;
        Ok(crate::autocomplete::autocomplete(
            &self.engine.snapshot(),
            read_model(infer),
            typed,
            limit,
        ))
    }

    /// The named graphs `scope` reads (RDF4J's `/contexts`).
    pub fn contexts(&self, scope: &crate::ReadScope) -> Vec<nrese_rdf::Term> {
        let mut graphs = crate::statements::contexts(&self.engine.snapshot());
        if let Some(access) = scope.access() {
            graphs.retain(|graph| match graph {
                nrese_rdf::Term::NamedNode(node) => access.allows(node.as_str()),
                _ => false,
            });
        }
        graphs
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

    /// Applies and commits `command` for `requester` without validation gates.
    pub fn apply(
        &self,
        command: &MutationCommand,
        requester: &crate::Requester,
    ) -> StoreResult<MutationCommitReport> {
        self.invalidate_reasoning()?;
        let mut tx = self.engine.transaction();
        let cancellation = CancellationToken::new();
        let context = self.update_context(&cancellation);
        let report = command.apply(&mut tx, requester, &context)?;
        let summary = tx.commit()?;
        Ok(report.committed(summary.revision))
    }

    // The writes below are the server's own and tests': every graph, no gates. Requests go
    // through the mutation pipeline with their requester.

    pub fn execute_update(
        &self,
        request: &SparqlUpdateRequest,
    ) -> StoreResult<UpdateExecutionReport> {
        match self.apply(
            &MutationCommand::Update(request.clone()),
            &crate::Requester::all(),
        )? {
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
        match self.apply(
            &MutationCommand::GraphWrite(request.clone()),
            &crate::Requester::all(),
        )? {
            MutationCommitReport::GraphWrite(report) => Ok(report),
            other => unreachable!("graph write produced {other:?}"),
        }
    }

    pub fn execute_graph_delete(&self, target: &GraphTarget) -> StoreResult<GraphDeleteReport> {
        match self.apply(
            &MutationCommand::GraphDelete(target.clone()),
            &crate::Requester::all(),
        )? {
            MutationCommitReport::GraphDelete(report) => Ok(report),
            other => unreachable!("graph delete produced {other:?}"),
        }
    }

    /// Loads RDF files through the engine's bulk path: parallel parsing and interning, one
    /// sort, one revision, no validation gates. For initial loads and full restores; use
    /// the mutation pipeline for regular writes.
    pub fn bulk_load(&self, request: &BulkLoadRequest) -> StoreResult<BulkLoadReport> {
        self.bulk_load_with(request, &crate::LoadProgress::default())
    }

    /// [`Self::bulk_load`], reporting to `progress` and stopping when it is cancelled.
    pub fn bulk_load_with(
        &self,
        request: &BulkLoadRequest,
        progress: &crate::LoadProgress,
    ) -> StoreResult<BulkLoadReport> {
        self.invalidate_reasoning()?;
        bulk_load(&self.engine, request, progress)
    }

    /// Builds the query planner's statistics of the store as it is now and saves them
    /// beside its checkpoint ([`nrese_engine::Engine::prepare_statistics`]): the last step
    /// of a load, so that queries after a start neither wait for them nor share the
    /// machine with their build.
    pub fn prepare_statistics(&self) {
        self.engine.prepare_statistics();
    }

    /// Whether the inferred stack is `program`'s closure as this store computes it.
    pub fn reasoning_is_current(&self, program: impl Into<nrese_reasoner::RuleProgram>) -> bool {
        let program = program.into();
        self.reasoning_state().is_some_and(|state| {
            state.is_current_with(
                &program,
                self.config.hide_unnamed_classes,
                self.config.equality_compact,
            )
        })
    }

    /// Why the statement `subject predicate object` holds under `program`: a derivation of
    /// it from asserted statements, the statement first ([`crate::reasoning::explain_fact`]).
    /// `None` if it doesn't hold (or no derivation was found within the budget).
    ///
    /// Within `scope`: the statement must be one the requester sees (an inferred statement
    /// where the scope shows inferences, an asserted one in a readable graph), else the
    /// answer is `None` as for a statement that doesn't hold (no existence oracle for
    /// hidden statements); asserted premises in no readable graph are `hidden` steps.
    pub fn explain_statement(
        &self,
        program: impl Into<nrese_reasoner::RuleProgram>,
        scope: &crate::ReadScope,
        subject: nrese_rdf::TermRef<'_>,
        predicate: nrese_rdf::TermRef<'_>,
        object: nrese_rdf::TermRef<'_>,
    ) -> Option<Vec<crate::reasoning::InferenceStep>> {
        let snapshot = self.engine.snapshot();
        let program = self.bound_program(&snapshot, program.into());
        let fact = Self::fact_of(&snapshot, [subject, predicate, object])?;
        let crate::ReadScope::Graphs(access) = scope else {
            return crate::reasoning::explain_fact(&program, &snapshot, fact, None);
        };
        let readable = |fact: [u64; 3]| readable_asserted(&snapshot, access, fact);
        let steps = crate::reasoning::explain_fact(&program, &snapshot, fact, Some(&readable))?;
        let visible = match steps.first().map(|step| step.origin) {
            Some("inferred") => self.inferred_visible(&snapshot, access, fact),
            Some("asserted") => true,
            _ => false,
        };
        visible.then_some(steps)
    }

    /// The justifications of the statement `subject predicate object` under `program`:
    /// minimal sets of asserted statements it follows from, as `mode` asks
    /// ([`crate::reasoning::justify_fact`]). `None` if it doesn't hold. Within `scope` as
    /// [`Self::explain_statement`]: a statement the requester doesn't see is `None`, and
    /// asserted statements in no readable graph are `hidden`.
    pub fn justify_statement(
        &self,
        program: impl Into<nrese_reasoner::RuleProgram>,
        scope: &crate::ReadScope,
        statement: [nrese_rdf::TermRef<'_>; 3],
        mode: crate::reasoning::JustificationMode,
    ) -> Option<crate::reasoning::JustificationAnswer> {
        let snapshot = self.engine.snapshot();
        let program = self.bound_program(&snapshot, program.into());
        let fact = Self::fact_of(&snapshot, statement)?;
        let crate::ReadScope::Graphs(access) = scope else {
            return crate::reasoning::justify_fact(&program, &snapshot, fact, mode, None);
        };
        let readable = |fact: [u64; 3]| readable_asserted(&snapshot, access, fact);
        let visible = readable(fact) || self.inferred_visible(&snapshot, access, fact);
        if !visible {
            return None;
        }
        crate::reasoning::justify_fact(&program, &snapshot, fact, mode, Some(&readable))
    }

    /// `program` with its constants by the snapshot's ids, without the writer: a constant
    /// the store doesn't hold gets an id no statement uses (its rules can't fire).
    fn bound_program(
        &self,
        snapshot: &nrese_engine::Snapshot,
        program: nrese_reasoner::RuleProgram,
    ) -> crate::reasoning::Program {
        let unknown = std::cell::Cell::new(u64::MAX >> 1);
        crate::reasoning::Program::compile(&program, &|term| {
            snapshot.lookup(term).unwrap_or_else(|| {
                unknown.set(unknown.get() - 1);
                nrese_engine::TermId::from_raw(unknown.get())
            })
        })
        .hiding_unnamed_classes(self.config.hide_unnamed_classes)
    }

    /// A statement by the snapshot's ids; `None` if a term isn't in the store.
    fn fact_of(
        snapshot: &nrese_engine::Snapshot,
        [s, p, o]: [nrese_rdf::TermRef<'_>; 3],
    ) -> Option<[u64; 3]> {
        Some([
            snapshot.lookup(s)?.raw(),
            snapshot.lookup(p)?.raw(),
            snapshot.lookup(o)?.raw(),
        ])
    }

    /// Whether the requester sees `fact` as an inferred statement.
    fn inferred_visible(
        &self,
        snapshot: &nrese_engine::Snapshot,
        access: &std::sync::Arc<nrese_sparql::GraphAccess>,
        fact: [u64; 3],
    ) -> bool {
        if by_support(access) {
            let [s, p, o] = fact.map(nrese_engine::TermId::from_raw);
            self.view_of(snapshot, Some(access)).contains_in(
                ReadModel::Inferred,
                &nrese_engine::EncodedQuad::new(s, p, o, nrese_engine::TermId::DEFAULT_GRAPH),
            )
        } else {
            access.inferred
        }
    }

    /// The closure's size with equality replicated and over representatives, for
    /// `program` on the asserted data ([`crate::reasoning::equality_report`]).
    /// A watch over the process's memory limit; `None` without a limit.
    pub(crate) fn memory_watch(&self) -> Option<nrese_exec::memory::MemoryWatch> {
        let limit = self.config.process_memory_bytes;
        (limit > 0).then(|| nrese_exec::memory::MemoryWatch::new(limit))
    }

    pub fn equality_report(&self, program: impl Into<nrese_reasoner::RuleProgram>) -> String {
        let tx = self.engine.transaction();
        let program = crate::reasoning::Program::compile(&program.into(), &|term| tx.intern(term));
        crate::reasoning::equality_report(&program, tx.base())
    }

    /// Replaces the inferred stack with `program`'s closure over the asserted data, as one
    /// revision (see [`crate::reasoning`]). For after bulk loads, at startup and after a
    /// change of rules; commits keep it current afterwards.
    pub fn rematerialise(
        &self,
        program: impl Into<nrese_reasoner::RuleProgram>,
    ) -> StoreResult<crate::reasoning::MaterialisationReport> {
        self.rematerialise_until(program, nrese_reasoner::eval::NEVER)
    }

    /// [`Self::rematerialise`], stopped when `stop` fires: then nothing changes and
    /// [`StoreError::MaterialisationCancelled`](crate::StoreError) is returned. It also
    /// stops when the process passes its memory limit
    /// ([`crate::StoreConfig::process_memory_bytes`]), with
    /// [`StoreError::ProcessMemoryLimit`](crate::StoreError).
    pub fn rematerialise_until(
        &self,
        program: impl Into<nrese_reasoner::RuleProgram>,
        stop: nrese_reasoner::eval::Stop<'_>,
    ) -> StoreResult<crate::reasoning::MaterialisationReport> {
        let rules = &program.into();
        let started = std::time::Instant::now();
        let rematerialisation = self.engine.rematerialisation();
        let asserted = rematerialisation
            .base()
            .len_in(nrese_engine::ReadModel::Asserted);
        let program =
            crate::reasoning::Program::compile(rules, &|term| rematerialisation.intern(term))
                .hiding_unnamed_classes(self.config.hide_unnamed_classes)
                .by_representatives(self.config.equality_by_representatives)
                .storing_representatives(self.config.equality_compact);
        let watch = self.memory_watch();
        let stop = || stop() || watch.as_ref().is_some_and(|watch| watch.exceeded());
        let stopped = || match &watch {
            Some(watch) if watch.exceeded() => crate::StoreError::ProcessMemoryLimit {
                limit: watch.limit(),
            },
            _ => crate::StoreError::MaterialisationCancelled,
        };
        let closure =
            crate::reasoning::materialise_until(&program, rematerialisation.base(), &stop)
                .map_err(|_| stopped())?;
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
        // The last word under the writer slot: a caller whose claim ended meanwhile (a
        // retired pipeline, a cancelled request) changes nothing.
        if stop() {
            return Err(stopped());
        }
        let summary = rematerialisation.finish(closure.inferred)?;
        nrese_engine::memory::release_all();
        self.record_reasoning(crate::ReasoningState::of_with(
            rules,
            self.config.hide_unnamed_classes,
            self.config.equality_compact,
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
        // A rematerialisation isn't reported to the sets as commits are: readers that
        // use them would wait for a computation.
        if self.supports.in_use() {
            self.prepare_support_sets();
        }
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
        match self.apply(
            &MutationCommand::Restore(request.clone()),
            &crate::Requester::all(),
        )? {
            MutationCommitReport::Restore(report) => Ok(report),
            other => unreachable!("restore produced {other:?}"),
        }
    }
}

/// Whether a reader with `access` sees the inferred statements by their support graph
/// sets ([`crate::support`]).
fn by_support(access: &nrese_sparql::GraphAccess) -> bool {
    access.inferred && access.inferred_by_support
}

/// Whether the pending state of `tx` holds the inferred `quad`.
fn view_has(tx: &nrese_engine::Transaction<'_>, quad: &nrese_engine::EncodedQuad) -> bool {
    tx.contains_in(ReadModel::Inferred, quad)
}

/// The read model of RDF4J's `infer` parameter.
fn read_model(infer: bool) -> crate::ReadModel {
    match infer {
        true => crate::ReadModel::Materialised,
        false => crate::ReadModel::Asserted,
    }
}

/// Whether `fact` is asserted in a graph `access` may read.
fn readable_asserted(
    snapshot: &nrese_engine::Snapshot,
    access: &nrese_sparql::GraphAccess,
    [s, p, o]: [u64; 3],
) -> bool {
    let id = nrese_engine::TermId::from_raw;
    let pattern = nrese_engine::QuadPattern {
        subject: Some(id(s)),
        predicate: Some(id(p)),
        object: Some(id(o)),
        graph: nrese_engine::GraphSelector::Any,
    };
    snapshot
        .quads_for_pattern_in(nrese_engine::ReadModel::Asserted, &pattern)
        .any(|quad| {
            let graph = if quad.graph.is_default_graph() {
                nrese_rdf::GraphName::DefaultGraph
            } else {
                match snapshot.decode(quad.graph) {
                    Some(nrese_rdf::Term::NamedNode(n)) => nrese_rdf::GraphName::NamedNode(n),
                    _ => return false,
                }
            };
            access.allows_graph(&graph)
        })
}
