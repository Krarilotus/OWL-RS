use std::fmt;
use std::path::{Path, PathBuf};

use nrese_engine::{Engine, EngineConfig, ReadModel, TermId};
use nrese_sparql::CancellationToken;

use crate::backup::{DatasetBackupArtifact, DatasetBackupFormat, DatasetExport, export_dataset};
use crate::bulk_load::{BulkLoadReport, BulkLoadRequest, bulk_load};
use crate::config::{StoreConfig, StoreMode};
use crate::error::StoreResult;
use crate::graph_store::{
    GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport, GraphWriteRequest,
};
use crate::graph_store_executor::{graph_exists, write_graph};
use crate::loader::preload_ontology;
use crate::mutation::{MutationCommand, MutationCommitReport};
use crate::query::{SerializedQueryResult, SparqlQueryRequest};
use crate::query_executor::{PreparedQuery, explain_prepared, plan_prepared, run_query};
use crate::stats::{StoreStats, collect_stats};
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};

/// The dataset and the operations on it. Reads run on an engine snapshot and never wait for
/// writers. Writes go through [`MutationPipeline`](crate::MutationPipeline), which gates
/// them (access, reasoning, consistency, SHACL); the store has no public write that
/// commits around it. Bulk loads, rematerialisation and a replica's log are operations of
/// their own.
#[derive(Clone)]
pub struct StoreService {
    config: StoreConfig,
    /// [`StoreConfig::process_memory_bytes`] as resolved at open (0: no limit).
    process_memory: u64,
    engine: Engine,
    preloaded_ontology: Option<PathBuf>,
    /// Whether the reasoning state file may exist (see [`Self::reasoning_state`]); saves a
    /// file-system call per write once it is gone.
    marker: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The reasoning state recorded this process (also for in-memory stores, which have no
    /// file).
    materialised: std::sync::Arc<std::sync::Mutex<Option<crate::ReasoningState>>>,
    /// The certificate's epoch, a sequence lock over its changes ([`Self::certificate_epoch`]):
    /// odd while one is in progress (under the writer slot, [`CertificateTransition`]), even
    /// once it is settled in memory and on disk.
    certificate_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// When queries are rewritten for OWL 2 QL ([`StoreConfig::ql_rewriting`], or the
    /// repository's own, [`Self::set_ql_rewriting`]).
    ql_mode: std::sync::Arc<std::sync::RwLock<crate::QlRewritingMode>>,
    /// The latest full materialisation's report (startup, load, ruleset change).
    last_materialisation:
        std::sync::Arc<std::sync::Mutex<Option<crate::reasoning::MaterialisationReport>>>,
    /// What every query gets from the store: the default graph's meaning and the budget
    /// all running queries share.
    settings: crate::query_executor::StoreSettings,
    /// Namespace prefixes for clients ([`crate::namespaces`]).
    namespaces: std::sync::Arc<crate::namespaces::Namespaces>,
    /// Statements bulk loads couldn't read ([`crate::quarantine`]).
    quarantine: std::sync::Arc<crate::quarantine::Quarantine>,
    /// Client transactions ([`crate::sessions`]).
    sessions: std::sync::Arc<crate::sessions::Sessions>,
    /// The queries running now ([`crate::running`]).
    running: std::sync::Arc<crate::running::RunningQueries>,
    /// Inferred statements under graph access ([`crate::support`]).
    supports: std::sync::Arc<crate::support::Supports>,
    /// The `owl2-dl` mode's state ([`crate::dl`]).
    dl: std::sync::Arc<crate::dl::Dl>,
    /// Why the store can't serve any more until the server restarts, if it can't
    /// ([`Self::unavailable`]): its state is neither the one before a failed change nor the
    /// one after it. Set once.
    unavailable: std::sync::Arc<std::sync::OnceLock<String>>,
}

/// The file recording what the inferred stack is exact for ([`crate::reasoning_state`]).
const REASONING_MARKER: &str = "reasoning.state";

/// A change of the reasoning certificate in progress
/// ([`StoreService::certificate_epoch`]): made odd by its start, the epoch is even again when
/// this drops, the change settled or failed, on every way out (`?` and unwinding included).
struct CertificateTransition<'a>(&'a std::sync::atomic::AtomicU64);

impl Drop for CertificateTransition<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The memory stop of one operation ([`StoreService::memory_stop`]).
pub(crate) struct MemoryStop(Option<nrese_exec::memory::MemoryWatch>);

impl MemoryStop {
    pub(crate) fn exceeded(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(nrese_exec::memory::MemoryWatch::exceeded)
    }

    /// What the operation may still take under this watch now
    /// ([`nrese_exec::memory::MemoryWatch::room`]): its room for the reasoner
    /// ([`nrese_reasoner::eval::OperationStop::within`]), which admits by the least of it
    /// and the process's. Without a watch, no bound of its own.
    pub(crate) fn room(&self) -> nrese_exec::memory::Reading {
        self.0
            .as_ref()
            .map_or(Ok(u64::MAX), nrese_exec::memory::MemoryWatch::room)
    }

    /// The error of the operation `interrupted` ended, if memory ended it: this watch's
    /// stop (past its limit, or the memory unread), else by the cause the operation itself
    /// kept: the process limit's stop, or an allocation its room couldn't admit (by the
    /// least of this watch's and the process's limits, [`Self::room`]), named with the
    /// least of those limits set. `None`: its caller stopped it.
    pub(crate) fn error(
        &self,
        interrupted: nrese_reasoner::delta::Interrupted,
    ) -> Option<crate::StoreError> {
        use nrese_exec::memory::MemoryStopCause;
        let limit = || {
            [
                self.0.as_ref().map(nrese_exec::memory::MemoryWatch::limit),
                nrese_exec::memory::process_limit(),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(0)
        };
        let watch = self
            .0
            .as_ref()
            .and_then(|watch| Some((watch.limit(), watch.cause()?)))
            .or_else(|| Some((limit(), interrupted.memory?)));
        match watch {
            Some((limit, MemoryStopCause::Unread)) => {
                Some(crate::StoreError::MemoryUnreadable { limit })
            }
            Some((limit, MemoryStopCause::OverLimit)) => {
                Some(crate::StoreError::ProcessMemoryLimit { limit })
            }
            None => None,
        }
    }
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
    pub fn new(mut config: StoreConfig) -> StoreResult<Self> {
        let process_memory = config.process_memory_bytes.resolve()?;
        config.process_memory_bytes = crate::ProcessMemory::Bytes(process_memory);
        let runtime = std::sync::Arc::new(crate::Runtime::new(
            process_memory,
            config.total_query_memory_bytes,
        ));
        Self::with_runtime(config, runtime)
    }

    /// Opens a store on resources shared with other stores. A catalog uses this for
    /// every repository; embedded applications can supply the same owner explicitly.
    /// The runtime's budgets apply, not `config.total_query_memory_bytes` or, for what is
    /// charged, `config.process_memory_bytes` (which stays this store's watch).
    pub fn with_runtime(
        mut config: StoreConfig,
        runtime: std::sync::Arc<crate::Runtime>,
    ) -> StoreResult<Self> {
        config.validate()?;
        let process_memory = config.process_memory_bytes.resolve()?;
        config.process_memory_bytes = crate::ProcessMemory::Bytes(process_memory);
        nrese_engine::set_index_encoding(config.index_encoding);
        nrese_engine::set_vocabulary_encoding(config.vocabulary);
        // Only switched off: on leaves what the process was started with (an operator's
        // `MIMALLOC_ALLOW_THP=0` switches them off the same way).
        if !config.huge_pages {
            nrese_exec::memory::set_transparent_huge_pages(false);
        }
        nrese_engine::memory::set_release_between_rounds(config.reasoning_release);
        let engine = match config.mode {
            StoreMode::InMemory => Engine::new(EngineConfig::default())?,
            StoreMode::OnDisk => {
                let defaults = EngineConfig::default();
                let engine_config = EngineConfig {
                    durability: nrese_engine::DurabilityConfig {
                        verify_on_open: config.verify_on_open,
                        map_checkpoints: config.map_checkpoints,
                        checkpoint_after_wal_bytes: config.checkpoint_after_wal_bytes,
                        bulk_load_memory: (config.bulk_load_memory_bytes > 0)
                            .then_some(config.bulk_load_memory_bytes),
                        wal_archive: config.wal_archive,
                        wal_damage: config.wal_damage,
                        ..defaults.durability
                    },
                    ..defaults
                };
                Engine::open(&config.data_dir, engine_config)?
            }
        };
        // The canonical views versions keep (canonical equality answers) draw on the
        // memory the queries do.
        engine.set_canonical_view_budget(std::sync::Arc::new(
            nrese_exec::Budget::unlimited().within(runtime.query_memory()),
        ));
        // So do the corpora reader-scoped searches keep, where they are on.
        if config.search_scope == crate::SearchScope::Reader {
            start_reader_corpora(
                &engine,
                &runtime,
                config.reader_corpus_bytes,
                config.reader_corpus_tuning,
            );
        }
        let before = engine.snapshot().revision();
        // At the revision the engine recovered, before anything commits (the ontology's
        // preload may): its readmissions are in effect only where the store holds theirs.
        let quarantine = crate::quarantine::Quarantine::open(
            (config.mode == StoreMode::OnDisk).then_some(config.data_dir.as_path()),
            before,
        )?;
        let preloaded_ontology = preload_ontology(&engine, &config)?;
        let ql_mode = config.ql_rewriting;
        let settings = crate::query_executor::StoreSettings {
            union_default_graph: config.union_default_graph,
            geosparql_stated_only: config.geosparql_stated_only,
            runtime,
            services: std::sync::Arc::default(),
            equality_closed: std::sync::Arc::default(),
            equality_canonical: config.equality_canonical_answers,
            equality_early_expansion: config.equality_early_expansion,
            result_cache: (config.query_cache_bytes > 0).then(|| {
                std::sync::Arc::new(nrese_sparql::ResultCache::new(config.query_cache_bytes))
            }),
            ql: std::sync::Arc::default(),
            search_scope: std::sync::Arc::new(std::sync::RwLock::new(config.search_scope)),
            reader_corpus_bytes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(
                config.reader_corpus_bytes,
            )),
        };
        let unavailable = std::sync::Arc::<std::sync::OnceLock<String>>::default();
        let namespaces = crate::namespaces::Namespaces::open(
            (config.mode == StoreMode::OnDisk).then(|| config.data_dir.clone()),
            std::sync::Arc::clone(&unavailable),
        )?;
        // The support graph sets and restricted views draw on the memory the queries do,
        // as the canonical views.
        let supports = crate::support::Supports::new(std::sync::Arc::new(
            nrese_exec::Budget::unlimited().within(settings.runtime.query_memory()),
        ));
        // So does what client transactions keep between requests.
        let sessions = crate::sessions::Sessions::within(
            std::sync::Arc::new(
                nrese_exec::Budget::unlimited().within(settings.runtime.query_memory()),
            ),
            crate::sessions::SESSION_IDLE,
        );
        let service = Self {
            process_memory,
            settings,
            namespaces: std::sync::Arc::new(namespaces),
            quarantine: std::sync::Arc::new(quarantine),
            sessions: std::sync::Arc::new(sessions),
            running: std::sync::Arc::default(),
            supports: std::sync::Arc::new(supports),
            dl: std::sync::Arc::default(),
            config,
            engine,
            preloaded_ontology,
            marker: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            unavailable,
            materialised: std::sync::Arc::default(),
            certificate_epoch: std::sync::Arc::default(),
            ql_mode: std::sync::Arc::new(std::sync::RwLock::new(ql_mode)),
            last_materialisation: std::sync::Arc::default(),
        };
        // The planner's statistics are the store's, shared by its requests: an envelope of
        // their own within the catalog's budget, as classification's.
        service
            .engine
            .charge_statistics_to(std::sync::Arc::new(service.envelope(None).part()));
        // So are the owl2-dl bounds' U1 and read views (SYN-61), the same way.
        service
            .dl
            .bounds
            .set_kept(std::sync::Arc::new(service.envelope(None).part()));
        // Under the writer slot, as every change of the certificate (Q06 remainder, invariant C).
        let tx = service.engine.transaction();
        let writer = tx.writer().expect("a transaction holds the writer slot");
        if service.engine.snapshot().revision() != before {
            service.invalidate_held(writer)?;
        }
        if let Some(state) = service.reasoning_state() {
            service.note_equality(writer, &state);
            // Kept in memory too: reads ask for it per query ([`Self::status_without_dl`]).
            *service
                .materialised
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(state);
        }
        drop(tx);
        service.pin_configured_queries()?;
        Ok(service)
    }

    /// Whether the recorded reasoning closes the data under `owl:sameAs` (a ruleset with
    /// the equality rules): queries may then rely on it.
    ///
    /// A stack kept over representatives (`reasoner.equality = "compact"`) is read
    /// expanded: the engine learns `owl:sameAs` ([`nrese_engine::Engine::set_equality`]).
    fn note_equality(&self, writer: nrese_engine::WriterHeld<'_>, state: &crate::ReasoningState) {
        self.note_ql(writer, state);
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

    /// Switches the QL rewriting on for a closure it can rewrite over (docs/design/
    /// ql-rewriting.md §4): `owl2-ql` and `owl2-rl` make the data H-complete, `owl2-rl`
    /// with its list rules; the others don't close inverses or the domains of restrictions.
    /// Under `owl2-dl` the bounds give those answers ([`crate::dl`]): no rewriting.
    fn note_ql(&self, _writer: nrese_engine::WriterHeld<'_>, state: &crate::ReasoningState) {
        use nrese_sparql::ql::{Closure, QlRewriting};
        let closure = match state.ruleset.as_str() {
            "owl2-rl" => Some(Closure { lists: true }),
            "owl2-ql" => Some(Closure { lists: false }),
            _ => None,
        };
        let closure =
            closure.filter(|_| self.ql_rewriting().applies_to(&state.ruleset) && !self.dl.active());
        let mut ql = self.settings.ql.write().unwrap_or_else(|p| p.into_inner());
        // The same closure keeps its compiled schema.
        if ql.as_ref().map(|q| q.closure()) != closure {
            *ql = closure.map(|closure| std::sync::Arc::new(QlRewriting::new(closure)));
        }
    }

    /// When queries are rewritten for OWL 2 QL.
    pub fn ql_rewriting(&self) -> crate::QlRewritingMode {
        *self.ql_mode.read().unwrap_or_else(|p| p.into_inner())
    }

    /// What searches rank and select among ([`crate::StoreConfig::search_scope`]).
    pub fn search_scope(&self) -> crate::SearchScope {
        self.settings.search_scope()
    }

    /// Changes what searches rank and select among (a repository's setting): queries and
    /// updates that start from now on read it; running ones keep theirs. Their cached
    /// results stay apart ([`nrese_sparql::ResultCache`] keys a reader's scope).
    pub fn set_search_scope(&self, scope: crate::SearchScope) {
        let mut current = self
            .settings
            .search_scope
            .write()
            .unwrap_or_else(|p| p.into_inner());
        // Turned on: the corpora the engine keeps for it draw on the queries' memory.
        if scope == crate::SearchScope::Reader && *current != scope {
            self.start_reader_corpora();
        }
        *current = scope;
    }

    /// Changes the bytes a `reader` search scope's corpora may take (a repository's
    /// setting; [`crate::StoreConfig::reader_corpus_bytes`]): searches from now on keep
    /// theirs within it, and the corpora kept so far are let go. Under `global` it is read
    /// when `reader` is turned on.
    pub fn set_reader_corpus_bytes(&self, bytes: usize) {
        // Under the scope's lock: a scope turned on meanwhile reads the new bytes.
        let current = self
            .settings
            .search_scope
            .write()
            .unwrap_or_else(|p| p.into_inner());
        let before = self
            .settings
            .reader_corpus_bytes
            .swap(bytes, std::sync::atomic::Ordering::Relaxed);
        if before != bytes && *current == crate::SearchScope::Reader {
            self.start_reader_corpora();
        }
    }

    /// The bytes a `reader` search scope's corpora may take now.
    pub fn reader_corpus_bytes(&self) -> usize {
        self.settings
            .reader_corpus_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Gives the engine's corpora their budget (the setting's bytes within the queries'
    /// memory) and their tuning: reader-scoped searches from now on use them.
    fn start_reader_corpora(&self) {
        start_reader_corpora(
            &self.engine,
            &self.settings.runtime,
            self.reader_corpus_bytes(),
            self.config.reader_corpus_tuning,
        );
    }

    /// Changes when queries are rewritten for OWL 2 QL (a repository's setting), at once.
    /// Under the writer slot: the certificate it reads can't be cleared meanwhile.
    pub fn set_ql_rewriting(&self, mode: crate::QlRewritingMode) {
        let tx = self.engine.transaction();
        *self.ql_mode.write().unwrap_or_else(|p| p.into_inner()) = mode;
        if let Some(state) = self.reasoning_state() {
            #[cfg(test)]
            hooks::run(&hooks::AFTER_QL_READ);
            self.note_ql(
                tx.writer().expect("a transaction holds the writer slot"),
                &state,
            );
        }
    }

    /// The store's namespace prefixes ([`crate::namespaces`]).
    pub fn namespaces(&self) -> &crate::namespaces::Namespaces {
        &self.namespaces
    }

    /// Why the store can't serve until the server restarts, if it can't: a change failed
    /// and its state couldn't be brought back to a known one (the reason says which file
    /// and how to repair it). Its server answers every request to it with 503.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.get().map(String::as_str)
    }

    /// The store's [`Self::unavailable`], shared: a server reads it on every request
    /// without taking the store.
    pub fn unavailable_flag(&self) -> std::sync::Arc<std::sync::OnceLock<String>> {
        std::sync::Arc::clone(&self.unavailable)
    }

    /// Makes the store unavailable until a restart, for `why` (the first reason stays).
    pub(crate) fn make_unavailable(&self, why: String) {
        tracing::error!(%why, "store unavailable until the server restarts");
        let _ = self.unavailable.set(why);
    }

    /// The statements bulk loads couldn't read ([`crate::quarantine`]).
    pub fn quarantine(&self) -> &crate::quarantine::Quarantine {
        &self.quarantine
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

    /// Whether the certificate covers `program` for a holder of the writer slot: no write can
    /// change it, or the data it covers, until the holder lets the slot go. A writer that maintains
    /// the closure checks it here, under its own transaction (review r1, F1).
    pub(crate) fn reasoning_is_current_held(
        &self,
        _writer: nrese_engine::WriterHeld<'_>,
        program: &nrese_reasoner::RuleProgram,
    ) -> bool {
        self.reasoning_is_current(program)
    }

    /// The certificate's epoch, a sequence lock over its changes. Odd: a change is in progress,
    /// and what a reader sees of the certificate (in memory or on disk) may be the old one; it
    /// takes nothing then. Even and the same before and after a reader's reads: no change
    /// began meanwhile, so the certificate it checked covers every snapshot those reads took
    /// (every commit in between maintained the closure). Each change runs its whole course,
    /// marker included, inside one [`CertificateTransition`], before the commit it precedes.
    pub(crate) fn certificate_epoch(&self) -> u64 {
        self.certificate_epoch
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Starts a change of the certificate, under the writer slot: the epoch is odd until the
    /// returned transition drops, on every way out of the change.
    fn certificate_transition(
        &self,
        _writer: nrese_engine::WriterHeld<'_>,
    ) -> CertificateTransition<'_> {
        let before = self
            .certificate_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        debug_assert!(
            before.is_multiple_of(2),
            "one change of the certificate at a time"
        );
        CertificateTransition(&self.certificate_epoch)
    }

    /// Forgets what the inferred stack is exact for, taking the writer slot to do so: no
    /// rematerialisation certifies it meanwhile. Never call it under the slot (it isn't
    /// reentrant): a holder calls [`Self::invalidate_held`].
    pub fn invalidate_reasoning(&self) -> StoreResult<()> {
        let tx = self.engine.transaction();
        self.invalidate_held(tx.writer().expect("a transaction holds the writer slot"))
    }

    /// [`Self::invalidate_reasoning`] by a holder of the writer slot, before a commit that
    /// doesn't maintain the inferences: the certificate changes only between commits (the Q06
    /// remainder's invariant C), so no rematerialisation can certify data this write changes.
    pub(crate) fn invalidate_held(&self, writer: nrese_engine::WriterHeld<'_>) -> StoreResult<()> {
        use std::sync::atomic::Ordering;
        // Before the commit it precedes, until the marker is settled: no reader takes the old
        // certificate for the data this write changes (fix check r1 on review r1, F1).
        let _transition = self.certificate_transition(writer);
        #[cfg(test)]
        hooks::run(&hooks::INSIDE_INVALIDATION);
        self.settings
            .equality_closed
            .store(false, Ordering::Release);
        *self.settings.ql.write().unwrap_or_else(|p| p.into_inner()) = None;
        *self.materialised.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if !self.marker.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        if let Some(path) = self.marker_path() {
            // The removal durable too (DUR-3): a marker back after a power loss would claim the
            // inferred stack exact for reasoning it no longer reflects. Gone already (an
            // earlier try whose sync failed) still needs the sync ([`crate::metadata::remove`]).
            match crate::metadata::remove(&path) {
                Ok(()) => {}
                Err(error) => {
                    // The file may still be there, or its removal not durable: the next
                    // invalidation tries again (the review of 3 October 2026, A4).
                    self.marker.store(true, Ordering::Release);
                    return Err(crate::error::StoreError::Io(error));
                }
            }
        }
        Ok(())
    }

    /// Certifies `state` for the inferred stack just published, still under the writer slot
    /// of its publication ([`nrese_engine::Finished`]): no other write comes between them.
    fn record_reasoning(
        &self,
        writer: nrese_engine::WriterHeld<'_>,
        state: crate::ReasoningState,
    ) -> StoreResult<()> {
        let _transition = self.certificate_transition(writer);
        self.note_equality(writer, &state);
        let text = state.to_text();
        *self.materialised.lock().unwrap_or_else(|p| p.into_inner()) = Some(state);
        let Some(path) = self.marker_path() else {
            return Ok(());
        };
        // "A marker may be in place", before anything is written (fix check r1, R2-F2): a
        // publication that fails after its rename leaves the marker under its name, and the
        // next invalidation must remove it, or refuse, not skip it.
        self.marker
            .store(true, std::sync::atomic::Ordering::Release);
        // Synced and durably named, the store's path included (DUR-3; fix check DEFER-F1): read
        // at open, a marker lost or short after a power loss would misstate what the inferred
        // stack is exact for.
        crate::metadata::write(&path, text.as_bytes())?;
        Ok(())
    }

    /// Empties the inferred stack (reasoning switched off), forgetting the marker.
    pub fn clear_inferred(&self) -> StoreResult<u64> {
        let rematerialisation = self.engine.rematerialisation();
        self.invalidate_held(rematerialisation.writer())?;
        self.engine.set_equality(None);
        if rematerialisation.base().len_in(ReadModel::Inferred) == 0 {
            return Ok(0);
        }
        Ok(rematerialisation.finish(Vec::new())?.inferred_deleted)
    }

    /// Waits until the engine's background maintenance has nothing requested or running
    /// ([`Engine::settle`]): for measurements of what the store holds.
    #[doc(hidden)]
    pub fn settle(&self) {
        self.engine.settle();
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    /// The process memory limit this store watches ([`StoreConfig::process_memory_bytes`]
    /// as resolved when it opened); 0: none.
    pub fn process_memory_bytes(&self) -> u64 {
        self.process_memory
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

    /// The corpora a `reader` search scope keeps: their bytes against their budget, and how
    /// many were walked or derived; `started` false while none was made.
    pub fn text_corpus_stats(&self) -> nrese_engine::TextCorpusStats {
        self.engine.snapshot().text_corpus_stats()
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

    /// An export of the latest snapshot, to be written as it is read
    /// ([`DatasetExport::write`]); for a scope that reads every graph.
    pub fn dataset_export(
        &self,
        scope: &crate::ReadScope,
        format: DatasetBackupFormat,
    ) -> StoreResult<DatasetExport> {
        scope.require_all("an export")?;
        Ok(DatasetExport::of(self.engine.snapshot(), format))
    }

    /// What this store's updates are evaluated with, drawing on `memory`: the request's
    /// envelope, or for a transaction's operations replayed for a read, the reading query's.
    pub(crate) fn update_context<'a>(
        &'a self,
        memory: nrese_exec::Envelope,
        cancellation: &'a CancellationToken,
    ) -> crate::mutation::command::UpdateContext<'a> {
        crate::mutation::command::UpdateContext {
            memory,
            cancellation,
            union_default_graph: self.config.union_default_graph,
            geosparql_stated_only: self.config.geosparql_stated_only,
            search_scope: self.search_scope(),
            services: self.services(),
            namespaces: self.namespaces.all(),
            supports: std::sync::Arc::clone(&self.supports),
            compilation: self.support_compilation(),
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
    /// `inferred = "supported"`, only the inferred statements its graphs derive
    /// ([`crate::support`]). What making that view reads and builds is charged to the
    /// queries' memory; `Err` if it has no room (never a partial view).
    pub fn read_snapshot(
        &self,
        access: Option<&std::sync::Arc<nrese_sparql::GraphAccess>>,
    ) -> StoreResult<nrese_engine::Snapshot> {
        self.view_of(
            &self.engine.snapshot(),
            access,
            &self.settings.envelope(None),
            None,
        )
    }

    /// [`Self::read_snapshot`] for a request, its view's working memory charged to the
    /// request's envelope `memory`; cancelling the request (its time running out) stops
    /// the view's computation with the request's cancellation error.
    pub(crate) fn read_snapshot_within(
        &self,
        access: Option<&std::sync::Arc<nrese_sparql::GraphAccess>>,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
    ) -> StoreResult<nrese_engine::Snapshot> {
        self.view_of(&self.engine.snapshot(), access, memory, Some(cancellation))
    }

    /// [`Self::read_snapshot`] for a statement or graph read (RDF4J, the Graph Store): its
    /// view stops with the read's cancellation. Such a read has no envelope of its own, so
    /// its view's working memory is the queries', as [`Self::read_snapshot`]'s is.
    fn read_snapshot_for(
        &self,
        read: &crate::ReadContext<'_>,
    ) -> StoreResult<nrese_engine::Snapshot> {
        self.read_snapshot_within(
            read.scope.access(),
            &self.settings.envelope(None),
            &read.cancel,
        )
    }

    /// [`Self::read_snapshot_within`] of `snapshot`, a committed revision
    /// ([`crate::support::Reading::committed`]). The unrestricted path (every graph, or no
    /// inferences, or every inference over every named graph) is the snapshot itself: no
    /// support state is read.
    fn view_of(
        &self,
        snapshot: &nrese_engine::Snapshot,
        access: Option<&std::sync::Arc<nrese_sparql::GraphAccess>>,
        memory: &nrese_exec::Envelope,
        cancellation: Option<&CancellationToken>,
    ) -> StoreResult<nrese_engine::Snapshot> {
        match self.reading(access, memory, cancellation) {
            Some(reading) => Ok(reading.committed(snapshot)?),
            None => Ok(snapshot.clone()),
        }
    }

    /// What a read by a reader with `access` sees ([`crate::support::Reading::of`]).
    fn reading(
        &self,
        access: Option<&std::sync::Arc<nrese_sparql::GraphAccess>>,
        memory: &nrese_exec::Envelope,
        cancellation: Option<&CancellationToken>,
    ) -> Option<crate::support::Reading> {
        crate::support::Reading::of(
            &self.supports,
            self.support_compilation(),
            &crate::ReadScope::of(access.cloned()),
            memory,
            cancellation,
        )
        .1
    }

    pub(crate) fn support_compilation(&self) -> crate::support::Compilation {
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
        self.supports.prepare(
            self.engine.snapshot(),
            self.support_compilation(),
            self.settings.envelope(None),
        );
    }

    /// Inferred statements under graph access ([`crate::support`]): the mutation pipeline
    /// reports its commits to it.
    pub(crate) fn supports(&self) -> &std::sync::Arc<crate::support::Supports> {
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
        // Under the writer slot, as every change of the certificate's QL schema.
        let tx = self.engine.transaction();
        self.dl.set_active(active, user_rules);
        if active {
            // The bounds give the answers through existentials (see `note_ql`).
            *self.settings.ql.write().unwrap_or_else(|p| p.into_inner()) = None;
        } else if let Some(state) = self.reasoning_state() {
            // Off: the rewriting the closure and the setting call for, whatever came first
            // (a change from `owl2-dl` to `owl2-rl` sets its QL rewriting before its write
            // path, which switches the bounds off, exists).
            self.note_ql(
                tx.writer().expect("a transaction holds the writer slot"),
                &state,
            );
        }
    }

    /// Marks the store as a read replica: it follows a primary's log, whose dictionary
    /// its own must continue, so the `owl2-dl` mode interns no term here (it reads the
    /// terms the primary's records bring).
    pub fn mark_replica(&self) {
        self.dl.set_replica();
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
        let mut completeness = None;
        let cancellation = CancellationToken::new();
        let (_, ql) = self.run_reporting(&prepared, &cancellation, &mut payload, |status| {
            completeness = status
        })?;
        Ok(SerializedQueryResult {
            kind: prepared.kind(),
            media_type: prepared.media_type(),
            payload,
            ql,
            completeness,
        })
    }

    /// Hits, misses and size of the result cache ([`nrese_sparql::cache`]); all zero
    /// without one.
    pub fn query_cache_stats(&self) -> nrese_sparql::ResultCacheStats {
        self.settings
            .result_cache
            .as_ref()
            .map(|cache| cache.stats())
            .unwrap_or_default()
    }

    /// Runs `request` and pins its result in the result cache under `name`: it is kept
    /// while the store doesn't change and pinned again when the query runs after a change
    /// (replacing what `name` pinned before). An error if the cache is off, the query
    /// can't be cached (`RAND`, `NOW`, `UUID`, `STRUUID`, `BNODE`, `SERVICE`), or its
    /// result doesn't fit in the budget beside the other pinned ones.
    pub fn pin_query(
        &self,
        name: &str,
        request: &SparqlQueryRequest,
        cancellation: &CancellationToken,
    ) -> StoreResult<nrese_sparql::PinnedResult> {
        let mut prepared = self.prepare_query(request)?;
        prepared.pin_as(name);
        self.run_query(&prepared, cancellation, std::io::sink())?;
        self.pinned_queries()
            .into_iter()
            .find(|pin| pin.name == name)
            .ok_or_else(|| {
                crate::StoreError::Configuration(format!("the result of '{name}' was not pinned"))
            })
    }

    /// Unpins `name`'s result (it stays cached like any other); `false` if nothing is
    /// pinned under that name.
    pub fn unpin_query(&self, name: &str) -> bool {
        self.settings
            .result_cache
            .as_ref()
            .is_some_and(|cache| cache.unpin(name))
    }

    /// The pinned queries, by name; a result computed before the latest commit isn't
    /// held (the cache drops it when the next query runs).
    pub fn pinned_queries(&self) -> Vec<nrese_sparql::PinnedResult> {
        let Some(cache) = &self.settings.result_cache else {
            return Vec::new();
        };
        let revision = self.engine.snapshot().revision();
        cache
            .pins()
            .into_iter()
            .map(|pin| match pin.revision {
                Some(held) if held < revision => nrese_sparql::PinnedResult {
                    revision: None,
                    rows: 0,
                    bytes: 0,
                    ..pin
                },
                _ => pin,
            })
            .collect()
    }

    /// Drops every cached result that isn't pinned.
    pub fn clear_query_cache(&self) {
        if let Some(cache) = &self.settings.result_cache {
            cache.clear();
        }
    }

    /// Pins the queries of [`StoreConfig::pinned_queries`], each under its file name
    /// without the extension.
    fn pin_configured_queries(&self) -> StoreResult<()> {
        for path in &self.config.pinned_queries {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    crate::StoreError::Configuration(format!(
                        "pinned query {} has no file name",
                        path.display()
                    ))
                })?;
            let text = std::fs::read_to_string(path).map_err(|error| {
                crate::StoreError::Configuration(format!(
                    "pinned query {}: {error}",
                    path.display()
                ))
            })?;
            let request = SparqlQueryRequest::new(text, crate::ReadScope::All);
            self.pin_query(name, &request, &CancellationToken::new())
                .map_err(|error| {
                    crate::StoreError::Configuration(format!(
                        "pinned query {}: {error}",
                        path.display()
                    ))
                })?;
        }
        Ok(())
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
        self.run_query_reporting(prepared, cancellation, out, |_| {})
    }

    /// [`Self::run_query`], handing `report` whether the answers are complete, where a
    /// reasoning path can leave some out (`None` where none can), before any answer is
    /// written: a transport sends it ahead of them. Under `owl2-dl` the bounds' status
    /// ([`crate::dl`]; the answers are then completed before the first byte), else the
    /// OWL 2 QL rewriting's where it applies, else a ruleset closure's
    /// ([`Self::status_without_dl`]). One status, [`nrese_sparql::Completeness`].
    /// A reader of some graphs who sees every inference is told `not-certified` where the
    /// rewriting ran: its assessment reads the schema of every graph.
    pub fn run_query_reporting(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>),
    ) -> StoreResult<()> {
        self.run_reporting(prepared, cancellation, out, report)
            .map(|_| ())
    }

    /// [`Self::run_query`] with the answers' status and, under `owl2-dl`, what decided it
    /// (the paths, the candidates proved and refuted; empty outside the mode); `None`
    /// where no reasoning path can leave answers out. For tests and the lab.
    #[doc(hidden)]
    pub fn run_query_dl(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
    ) -> StoreResult<Option<(nrese_sparql::Completeness, crate::dl::DlDetail)>> {
        let mut status = None;
        let (detail, _) = self.run_reporting(prepared, cancellation, out, |s| status = s)?;
        Ok(status.map(|s| (s, detail.unwrap_or_default())))
    }

    /// Every query run but a transaction's pending one: its refusal as its reader may be
    /// told it ([`for_its_reader`]; the answers are the reader's own).
    fn run_reporting(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>),
    ) -> StoreResult<(
        Option<crate::dl::DlDetail>,
        Option<nrese_sparql::ql::QlReport>,
    )> {
        let run = self.run_reporting_whole(prepared, cancellation, out, report);
        for_its_reader(prepared, crate::EntryPoint::Run, run, |_| {})
    }

    fn run_reporting_whole(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>),
    ) -> StoreResult<(
        Option<crate::dl::DlDetail>,
        Option<nrese_sparql::ql::QlReport>,
    )> {
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        // The request's one envelope: every evaluation, retained result and DL task of
        // the request draws from it.
        let memory = self.settings.query_envelope(prepared);
        if let Some(mode) = self.dl_mode(prepared) {
            use crate::dl::query::{Outcome, Read};
            let dl = format!("owl2-dl {mode:?}");
            let detail =
                match crate::dl::query::answer(self, prepared, cancellation, &memory, mode)? {
                    Outcome::Answers(answers, status, detail) => {
                        report(Some(status));
                        crate::query_executor::write_answers(prepared, answers, cancellation, out)?;
                        detail
                    }
                    // Streamed over the snapshot the status was decided on (L's view where
                    // L adds memberships, kept per revision, whose identity is its own),
                    // never a later revision's; or, for a status that holds whatever the
                    // revision (sound only), over the latest.
                    Outcome::Stream(status, read, detail) => {
                        let snapshot = match read {
                            Read::Lower(snapshot) | Read::At(snapshot) => snapshot,
                            Read::Latest => {
                                self.read_snapshot_within(prepared.access(), &memory, cancellation)?
                            }
                        };
                        let context = format!("{dl} {status:?}");
                        run_query(
                            &snapshot,
                            prepared,
                            &self.settings,
                            cancellation,
                            &memory,
                            out,
                            |_| {
                                report(Some(status));
                                context
                            },
                        )?;
                        detail
                    }
                };
            return Ok((Some(detail), None));
        }
        let snapshot = self.read_snapshot_within(prepared.access(), &memory, cancellation)?;
        let ql = run_query(
            &snapshot,
            prepared,
            &self.settings,
            cancellation,
            &memory,
            out,
            |ql| {
                let status = ql
                    .map(|ql| ql.completeness.clone())
                    .or_else(|| self.status_without_dl(prepared));
                // The cache's context is the whole assessment, not what the reader is told.
                let context = format!("{status:?}");
                report(status_for_its_reader(prepared, status, ql.is_some()));
                context
            },
        )?;
        Ok((None, ql.filter(|_| !withholds_certification(prepared))))
    }

    /// The status of answers that don't go through the DL bounds; known before the query
    /// runs. `None` only where no reasoning path can leave answers out: asserted statements
    /// alone, or no reasoning. Over a ruleset's closure: sound, and complete only for what
    /// the ruleset derives, never certain answers under OWL 2 DL. Under `owl2-dl`, a read
    /// that isn't of every statement (the inferred ones alone, `onto:implicit`) reads the
    /// rules' closure without the bounds ([`Self::dl_mode`]): sound only, never complete
    /// (SYN-72 slice 2 r1 F1).
    pub fn status_without_dl(
        &self,
        prepared: &PreparedQuery,
    ) -> Option<nrese_sparql::Completeness> {
        if prepared.read_model() == ReadModel::Asserted {
            return None;
        }
        if self.dl.active() {
            let mut status = nrese_sparql::Completeness::under(nrese_sparql::Regime::Owl2Dl);
            status.incomplete(
                "dl",
                "a read of the inferred statements alone: the rules' closure, without the DL \
                 bounds; certain answers under OWL 2 DL may be missing"
                    .to_owned(),
            );
            return Some(status);
        }
        let ruleset = self
            .materialised
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()?
            .ruleset
            .clone();
        let mut status =
            nrese_sparql::Completeness::under(nrese_sparql::Regime::of_ruleset(&ruleset));
        status.incomplete(
            "rules",
            format!(
                "answers over the {ruleset} closure: what its rules derive, complete for \
                 the ontologies of their profile, which isn't checked per query; not every \
                 certain answer under OWL 2 DL (reasoner.mode = \"owl2-dl\" gives those)"
            ),
        );
        Some(status)
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

    pub fn runtime(&self) -> &std::sync::Arc<crate::Runtime> {
        &self.settings.runtime
    }

    /// A request's memory envelope within the catalog's budget: `limit` bytes of its
    /// own, or none.
    pub(crate) fn envelope(&self, limit: Option<usize>) -> nrese_exec::Envelope {
        self.settings.envelope(limit)
    }

    /// Runs a prepared query on the latest snapshot to completion and reports how it ran:
    /// the executor, each operator with estimated and actual rows, and times. To a reader
    /// who may not read every graph, only what is its own ([`for_its_reader`]).
    pub fn explain_query(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::Explanation> {
        for_its_reader(
            prepared,
            crate::EntryPoint::Explain,
            self.explain_whole(prepared, cancellation),
            |explanation| {
                explanation.steps.clear();
                explanation.rewrites.clear();
                explanation.decoded = 0;
                certification_for_its_reader(
                    prepared,
                    &mut explanation.completeness,
                    &mut explanation.ql,
                );
            },
        )
    }

    fn explain_whole(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::Explanation> {
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        let memory = self.settings.query_envelope(prepared);
        let mut explanation = explain_prepared(
            &self.read_snapshot_within(prepared.access(), &memory, cancellation)?,
            prepared,
            &self.settings,
            cancellation,
            &memory,
        )?;
        if let Some(mode) = self.dl_mode(prepared) {
            // The status as the query would answer, never failing for it.
            let mode = match mode {
                crate::DlAnswers::Exact => crate::DlAnswers::CertainWhereComplete,
                other => other,
            };
            let (status, detail) =
                match crate::dl::query::answer(self, prepared, cancellation, &memory, mode)? {
                    crate::dl::query::Outcome::Answers(_, status, detail)
                    | crate::dl::query::Outcome::Stream(status, _, detail) => (status, detail),
                };
            explanation.completeness = Some(status);
            explanation.decided_by = detail.paths;
            explanation.candidates = Some(nrese_sparql::Candidates {
                proved: detail.proved,
                refuted: detail.refuted,
            });
        } else {
            explanation.completeness = explanation
                .ql
                .as_ref()
                .map(|ql| ql.completeness.clone())
                .or_else(|| self.status_without_dl(prepared));
        }
        Ok(explanation)
    }

    /// The plan `prepared` would run as, each node with its estimated rows, without
    /// running it (EXPLAIN without ANALYZE), within the query's memory envelope. Planning
    /// stops with [`nrese_sparql::QueryEvaluationError::Cancelled`] once `cancellation` is
    /// cancelled. To a reader who may not read every graph, only what is its own
    /// ([`for_its_reader`]).
    pub fn plan_query(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::PlannedQuery> {
        for_its_reader(
            prepared,
            crate::EntryPoint::Plan,
            self.plan_whole(prepared, cancellation),
            |planned| {
                planned.steps.clear();
                planned.rewrites.clear();
                certification_for_its_reader(prepared, &mut planned.completeness, &mut planned.ql);
            },
        )
    }

    fn plan_whole(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::PlannedQuery> {
        let memory = self.settings.query_envelope(prepared);
        let mut planned = plan_prepared(
            &self.read_snapshot_within(prepared.access(), &memory, cancellation)?,
            prepared,
            &self.settings,
            cancellation,
            &memory,
        )?;
        planned.completeness = match self.dl_mode(prepared) {
            Some(_) => Some(crate::dl::query::plan_status(self, prepared)),
            None => planned
                .ql
                .as_ref()
                .map(|ql| ql.completeness.clone())
                .or_else(|| self.status_without_dl(prepared)),
        };
        Ok(planned)
    }

    /// Bytes of intermediate results the running queries hold now, the most they held at
    /// once, and the limit; `None` without a limit
    /// ([`StoreConfig::total_query_memory_bytes`]).
    pub fn query_memory(&self) -> Option<(usize, usize, usize)> {
        let budget = self.settings.runtime.query_pool()?;
        Some((budget.used(), budget.peak(), budget.limit()))
    }

    /// `query` printed as standard SPARQL 1.1 for a store without reasoning that holds
    /// this store's asserted statements (its schema included): answering as this store does
    /// under `owl2-rl` with the QL rewriting on (docs/design/ql-rewriting.md §8), or why it
    /// can't be written exactly. For benchmarks of stores without reasoning.
    pub fn print_query(
        &self,
        query: &str,
        form: nrese_sparql::ql::PrintForm,
    ) -> StoreResult<nrese_sparql::ql::Printed> {
        let query = nrese_sparql::compat::parse_query(query, None)?;
        let snapshot = self.engine.snapshot();
        Ok(nrese_sparql::ql::print(&snapshot, &query, form)?)
    }

    /// A query over every graph (the server's own work, tests).
    pub fn execute_query_str(&self, query: &str) -> StoreResult<SerializedQueryResult> {
        self.execute_query(&SparqlQueryRequest::new(query, crate::ReadScope::All))
    }

    /// A graph (Graph Store Protocol `GET`) as the read sees it, in memory: for callers that
    /// want it so (tests, embedding); the server streams [`Self::write_graph`].
    pub fn execute_graph_read(
        &self,
        read: &crate::ReadContext<'_>,
        request: &GraphReadRequest,
    ) -> StoreResult<GraphReadResult> {
        let mut payload = Vec::new();
        let exists = self.write_graph(read, request, &mut payload)?;
        Ok(GraphReadResult {
            media_type: request.format.media_type(),
            payload,
            exists,
        })
    }

    /// A graph (Graph Store Protocol `GET`) as the read sees it, written to `out` as it is
    /// read; `false` (nothing written) for a graph that doesn't exist for the read, as a
    /// graph its scope doesn't read.
    pub fn write_graph(
        &self,
        read: &crate::ReadContext<'_>,
        request: &GraphReadRequest,
        out: impl std::io::Write,
    ) -> StoreResult<bool> {
        let whole = || -> StoreResult<bool> {
            if !self.readable(read, &request.target)? {
                return Ok(false);
            }
            let view = self.read_snapshot_for(read)?;
            write_graph(&view, request, read.model(), &read.cancel, out)
        };
        read.scope.told(crate::EntryPoint::GraphRead, whole())
    }

    /// Whether a graph exists as the read sees it (Graph Store Protocol `HEAD`), as
    /// [`Self::write_graph`] would answer, with nothing decoded.
    pub fn graph_exists(
        &self,
        read: &crate::ReadContext<'_>,
        target: &GraphTarget,
    ) -> StoreResult<bool> {
        let whole = || -> StoreResult<bool> {
            if !self.readable(read, target)? {
                return Ok(false);
            }
            graph_exists(&self.read_snapshot_for(read)?, target, read.model())
        };
        read.scope.told(crate::EntryPoint::GraphExists, whole())
    }

    /// Whether the read's scope reads `target`: a graph it doesn't read doesn't exist for it.
    fn readable(&self, read: &crate::ReadContext<'_>, target: &GraphTarget) -> StoreResult<bool> {
        Ok(match read.scope.access() {
            Some(access) => access.allows_graph(&target.graph_name()?),
            None => true,
        })
    }

    /// Runs `read` on the data as it would be after `pending` (an RDF4J transaction's
    /// operations), which are applied to a speculative transaction: never committed, so no
    /// writer slot is taken.
    /// In a session, the result is kept for its next read on the same data
    /// ([`crate::sessions`]); `scope` is the reader's, by which the operations were scoped.
    /// What the operations' replay and the reader's view read and build is charged to
    /// `memory`, the reading query's envelope ([`Self::read_snapshot_within`]).
    fn with_pending<T>(
        &self,
        pending: &crate::StatementsRequest,
        scope: &crate::ReadScope,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
        read: impl FnOnce(&nrese_engine::Snapshot) -> StoreResult<T>,
    ) -> StoreResult<T> {
        // The rules the view is decided under, read before it is made: one made under later
        // rules is kept as of these, never the reverse.
        let epoch = self.supports.epoch();
        if let Some(view) = self
            .sessions
            .view(&self.engine.snapshot(), scope, pending, epoch)
        {
            return read(&view);
        }
        // Never committed: no writer slot, so other clients commit meanwhile. The operations
        // read as the reader may; what they change is checked at the commit (the view is
        // the reader's own, and shows the graphs it may read only).
        // V1: private once V2 reads a private transaction through SPARQL.
        let mut tx = self.engine.speculative_shared();
        // What the operations' transfers reserve of the pending changes is the reading
        // query's too.
        tx.charge_pending_to(std::sync::Arc::new(memory.part()))?;
        let context = self.update_context(memory.clone(), cancellation);
        let requester = crate::Requester::new(scope.clone(), crate::WriteScope::All);
        crate::statements::apply_statements(&mut tx, pending, &requester, &mut |tx, request| {
            crate::mutation::command::apply_sparql_update(tx, request, &requester, &context)
        })?;
        let view = match self.reading(scope.access(), memory, Some(cancellation)) {
            Some(reading) => reading.pending(&tx)?,
            None => tx.pending_snapshot(),
        };
        self.sessions.keep_view(
            tx.base().clone(),
            scope,
            pending,
            view.clone(),
            epoch,
            self.supports.epoch(),
        );
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
        self.run_query_pending_reporting(pending, prepared, cancellation, out, |_| {})
    }

    /// [`Self::run_query_pending`], handing `report` the answers' status as
    /// [`Self::run_query_reporting`] does. A transaction reads the latest revision: with
    /// no operations pending, exactly as a query outside it (under `owl2-dl`, through that
    /// revision's bounds). Pending operations aren't reasoned over until the commit: the
    /// status says so, sound only, and not sound where they may delete statements whose
    /// inferences remain.
    pub fn run_query_pending_reporting(
        &self,
        pending: &crate::StatementsRequest,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>),
    ) -> StoreResult<()> {
        if pending.ops.is_empty() {
            return self.run_query_reporting(prepared, cancellation, out, report);
        }
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        let scope = crate::ReadScope::of(prepared.access().cloned());
        let in_dl = self.dl_mode(prepared).is_some();
        let deletes = pending
            .ops
            .iter()
            .any(|op| !matches!(op, crate::StatementOp::Add { .. }));
        let memory = self.settings.query_envelope(prepared);
        let run = self.with_pending(pending, &scope, &memory, cancellation, |snapshot| {
            run_query(
                snapshot,
                prepared,
                &self.settings,
                cancellation,
                &memory,
                out,
                |ql| {
                    let mut status = match in_dl {
                        true => {
                            let mut status =
                                nrese_sparql::Completeness::under(nrese_sparql::Regime::Owl2Dl);
                            status.incomplete(
                        "dl",
                        "a read inside a transaction: its pending operations aren't reasoned \
                         over before the commit (the committed closure, no DL bounds)"
                            .to_owned(),
                    );
                            Some(status)
                        }
                        false => ql
                            .map(|ql| ql.completeness.clone())
                            .or_else(|| self.status_without_dl(prepared)),
                    };
                    if deletes && let Some(status) = &mut status {
                        status.unsound(
                            "transaction",
                            "the pending operations may delete statements whose inferences remain \
                     until the commit"
                                .to_owned(),
                        );
                    }
                    // A transaction's pending state: never answered from the cache.
                    report(status_for_its_reader(prepared, status, ql.is_some()));
                    String::new()
                },
            )
            .map(|_| ())
        });
        for_its_reader(prepared, crate::EntryPoint::RunPending, run, |_| {})
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
                        (
                            graph,
                            snapshot.count_in(ReadModel::Asserted, &pattern).at_least(),
                        )
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
        self.on_data(crate::EntryPoint::Statements, read, |snapshot| {
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
        self.on_data(crate::EntryPoint::Statements, read, |snapshot| {
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
        self.on_data(crate::EntryPoint::Size, read, |snapshot| {
            Ok(crate::statements::count_statements(
                snapshot,
                read.model(),
                pattern,
                &read.scope,
            ))
        })
    }

    /// Runs `read` on the latest snapshot, or on the data as the context's pending changes
    /// would leave it, for `entry`: what it returns as its reader may be told it.
    fn on_data<T>(
        &self,
        entry: crate::EntryPoint,
        context: &crate::ReadContext<'_>,
        read: impl FnOnce(&nrese_engine::Snapshot) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let whole = match context.pending {
            None => self.read_snapshot_for(context).and_then(|view| read(&view)),
            Some(pending) => self.with_pending(
                pending,
                &context.scope,
                &self.settings.envelope(None),
                &context.cancel,
                read,
            ),
        };
        context.scope.told(entry, whole)
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
    ///
    /// The answer says what it carries (`expected`): one that doesn't hold exactly that is
    /// refused with nothing applied. `stop` ends it between records; what was published
    /// before stays ([`nrese_engine::Applied`]).
    pub fn apply_replication_log(
        &self,
        frames: &[u8],
        expected: &nrese_engine::LogExpectation,
        stop: &nrese_exec::stop::StopSource,
    ) -> StoreResult<nrese_engine::Applied> {
        let before = self.current_revision();
        let applied = self.engine.apply_log(frames, expected, stop);
        // Whatever was published, a whole answer or the records before a stop or a
        // failure, the support sets start afresh.
        if self.current_revision() != before {
            self.supports().forget();
        }
        applied.map_err(crate::StoreError::Engine)
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
        // A reader's corpora draw from the request's envelope, as a query's do.
        let search_scope = self.search_scope();
        let budget = (search_scope == crate::SearchScope::Reader)
            .then(|| std::sync::Arc::new(self.envelope(None).part()));
        Ok(crate::autocomplete::autocomplete(
            &self.engine.snapshot(),
            read_model(infer),
            typed,
            limit,
            search_scope,
            budget.as_ref(),
        )?)
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

    /// Applies and commits `command` for `requester` without validation gates: only for the
    /// store's own writes (its system graphs, scratch stores) and its unit tests. Every
    /// other write goes through [`crate::MutationPipeline`].
    pub(crate) fn apply_ungated(
        &self,
        command: &MutationCommand,
        requester: &crate::Requester,
    ) -> StoreResult<MutationCommitReport> {
        let mut tx = self.engine.transaction();
        self.invalidate_held(tx.writer().expect("a transaction holds the writer slot"))?;
        let cancellation = CancellationToken::new();
        let context = self.update_context(self.envelope(None), &cancellation);
        let report = command.apply(&mut tx, requester, &context)?;
        let summary = tx.commit()?;
        Ok(report.committed(summary.revision))
    }

    // The store's own writes, every graph, no gates (see `apply`). Everything else goes
    // through the mutation pipeline, which has the same methods, gated.

    pub(crate) fn update_ungated(
        &self,
        request: &SparqlUpdateRequest,
    ) -> StoreResult<UpdateExecutionReport> {
        match self.apply_ungated(
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

    #[cfg(test)]
    pub(crate) fn update_str_ungated(&self, update: &str) -> StoreResult<UpdateExecutionReport> {
        self.update_ungated(&SparqlUpdateRequest::new(update))
    }

    pub(crate) fn graph_write_ungated(
        &self,
        request: &GraphWriteRequest,
    ) -> StoreResult<GraphWriteReport> {
        match self.apply_ungated(
            &MutationCommand::GraphWrite(request.clone()),
            &crate::Requester::all(),
        )? {
            MutationCommitReport::GraphWrite(report) => Ok(report),
            other => unreachable!("graph write produced {other:?}"),
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
        let invalid = request.invalid.unwrap_or(self.config.invalid_statements);
        // Cleared under the load's writer slot, before it commits (invariant C).
        let invalidate = |writer: nrese_engine::WriterHeld<'_>| self.invalidate_held(writer);
        bulk_load(
            &self.engine,
            request,
            progress,
            invalid,
            &self.quarantine,
            &invalidate,
        )
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
                || crate::reasoning_state::nil_equated(&self.engine.snapshot()),
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
    ) -> StoreResult<Option<Vec<crate::reasoning::InferenceStep>>> {
        let whole =
            self.explain_statement_whole(program.into(), scope, [subject, predicate, object]);
        scope.told(crate::EntryPoint::ExplainStatement, whole)
    }

    fn explain_statement_whole(
        &self,
        program: nrese_reasoner::RuleProgram,
        scope: &crate::ReadScope,
        statement: [nrese_rdf::TermRef<'_>; 3],
    ) -> StoreResult<Option<Vec<crate::reasoning::InferenceStep>>> {
        let snapshot = self.engine.snapshot();
        let program = self.bound_program(&snapshot, program);
        let Some(fact) = Self::fact_of(&snapshot, statement) else {
            return Ok(None);
        };
        let crate::ReadScope::Graphs(access) = scope else {
            return Ok(crate::reasoning::explain_fact(
                &program, &snapshot, fact, None,
            ));
        };
        let readable = |fact: [u64; 3]| {
            crate::support::asserted(&snapshot, access.as_ref(), fact)
                == crate::support::Asserted::InReach
        };
        let Some(steps) =
            crate::reasoning::explain_fact(&program, &snapshot, fact, Some(&readable))
        else {
            return Ok(None);
        };
        let visible = match steps.first().map(|step| step.origin) {
            Some("inferred") => self.inferred_visible(&snapshot, access, fact)?,
            Some("asserted") => true,
            _ => false,
        };
        Ok(visible.then_some(steps))
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
    ) -> StoreResult<Option<crate::reasoning::JustificationAnswer>> {
        let whole = self.justify_statement_whole(program.into(), scope, statement, mode);
        scope.told(crate::EntryPoint::JustifyStatement, whole)
    }

    fn justify_statement_whole(
        &self,
        program: nrese_reasoner::RuleProgram,
        scope: &crate::ReadScope,
        statement: [nrese_rdf::TermRef<'_>; 3],
        mode: crate::reasoning::JustificationMode,
    ) -> StoreResult<Option<crate::reasoning::JustificationAnswer>> {
        let snapshot = self.engine.snapshot();
        let program = self.bound_program(&snapshot, program);
        let Some(fact) = Self::fact_of(&snapshot, statement) else {
            return Ok(None);
        };
        let crate::ReadScope::Graphs(access) = scope else {
            return Ok(crate::reasoning::justify_fact(
                &program, &snapshot, fact, mode, None,
            ));
        };
        let readable = |fact: [u64; 3]| {
            crate::support::asserted(&snapshot, access.as_ref(), fact)
                == crate::support::Asserted::InReach
        };
        let visible = readable(fact) || self.inferred_visible(&snapshot, access, fact)?;
        if !visible {
            return Ok(None);
        }
        Ok(crate::reasoning::justify_fact(
            &program,
            &snapshot,
            fact,
            mode,
            Some(&readable),
        ))
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

    /// Whether the requester sees `fact` as an inferred statement; `Err` if its view
    /// couldn't be made.
    fn inferred_visible(
        &self,
        snapshot: &nrese_engine::Snapshot,
        access: &std::sync::Arc<nrese_sparql::GraphAccess>,
        fact: [u64; 3],
    ) -> StoreResult<bool> {
        match self.reading(Some(access), &self.settings.envelope(None), None) {
            Some(reading) if access.inferred_by_support => {
                let [s, p, o] = fact.map(nrese_engine::TermId::from_raw);
                Ok(reading.committed(snapshot)?.contains_in(
                    ReadModel::Inferred,
                    &nrese_engine::EncodedQuad::new(s, p, o, nrese_engine::TermId::DEFAULT_GRAPH),
                ))
            }
            _ => Ok(access.inferred),
        }
    }

    /// An operation's stop under this store's process-memory policy; zero disables it.
    /// Opening a store never changes the process owner's global safety fallback.
    pub(crate) fn memory_stop(&self) -> MemoryStop {
        let limit = self.process_memory;
        MemoryStop((limit > 0).then(|| nrese_exec::memory::MemoryWatch::new(limit)))
    }

    /// A part of the process's budget ([`crate::Runtime::memory`]) of its own, for what
    /// a store operation outside any request charges (a rematerialisation's consistency
    /// violations): a refusal marks this part only. Without a process budget it is
    /// unlimited by setting, bounded only by [`Self::memory_stop`].
    pub(crate) fn operation_memory(&self) -> std::sync::Arc<nrese_exec::Budget> {
        std::sync::Arc::new(nrese_exec::Budget::unlimited().within(self.runtime().memory()))
    }

    /// The closure's size with equality replicated and over representatives, for
    /// `program` on the asserted data, or an explicit diagnostic if stopped.
    pub fn equality_report(&self, program: impl Into<nrese_reasoner::RuleProgram>) -> String {
        let memory = self.memory_stop();
        let (stop, room) = (|| memory.exceeded(), || memory.room());
        let operation = nrese_reasoner::eval::OperationStop::new(&stop).within(&room);
        let tx = self.engine.transaction();
        let program = crate::reasoning::Program::compile(&program.into(), &|term| tx.intern(term));
        nrese_reasoner::engine::equality_report_with(&program, tx.base(), &operation)
            .unwrap_or_else(|interrupted| match memory.error(interrupted) {
                Some(unread @ crate::StoreError::MemoryUnreadable { .. }) => {
                    format!("equality: {unread}")
                }
                _ => "equality: stopped by the process memory limit".to_owned(),
            })
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
        let memory = self.memory_stop();
        let stop = || stop() || memory.exceeded();
        let room = || memory.room();
        let stopped = |interrupted| {
            memory
                .error(interrupted)
                .unwrap_or(crate::StoreError::MaterialisationCancelled)
        };
        let operation = nrese_reasoner::eval::OperationStop::new(&stop).within(&room);
        let closure = crate::reasoning::materialise_with(
            &program,
            rematerialisation.base(),
            &operation,
            &self.operation_memory(),
        )
        .map_err(stopped)?;
        // 0 exactly when consistent; a lower bound where the list stopped at its budget.
        let violations = closure.violations.at_least();
        let violations_at_least = !closure.violations.complete();
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
            return Err(stopped(nrese_reasoner::delta::Interrupted::default()));
        }
        // The certificate of the closure about to be replaced goes first, durably: a stop or
        // a failed certificate between the publication and the new certificate must not leave
        // the old one describing the new closure after a start (review r1, F3).
        self.invalidate_held(rematerialisation.writer())?;
        // Where asked, what the reasoner freed goes back before the inferred stack, the
        // heap's peak, is built (as between the reasoner's rounds).
        nrese_engine::memory::release_between_rounds();
        nrese_exec::heap::phase("store: inferred stack");
        // Published with the writer slot still held: certified before any other write.
        let finished = rematerialisation.finish_held(closure.inferred)?;
        let summary = finished.summary();
        nrese_exec::heap::phase("store: done");
        nrese_engine::memory::release_all();
        #[cfg(test)]
        hooks::run(&hooks::AFTER_PUBLICATION);
        // From here the new inferences are published: a failure says so.
        self.record_reasoning(
            finished.writer(),
            crate::ReasoningState::of_with(
                rules,
                self.config.hide_unnamed_classes,
                self.config.equality_compact,
                violations,
            ),
        )
        .map_err(|error| crate::StoreError::InferencesNotRecorded(Box::new(error)))?;
        drop(finished);
        if violations > 0 {
            tracing::error!(
                ruleset = %rules.name(),
                violations,
                violations_at_least,
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
            violations,
            violations_at_least,
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
            violations,
            violations_at_least,
            rounds: closure.rounds,
            elapsed: started.elapsed(),
            phases: closure.phases,
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
}

/// Gives `engine`'s corpora of reader-scoped searches ([`crate::SearchScope::Reader`]) a
/// budget of `bytes` within the memory the queries draw on, and `tuning`.
fn start_reader_corpora(
    engine: &Engine,
    runtime: &crate::Runtime,
    bytes: usize,
    tuning: nrese_engine::CorpusTuning,
) {
    engine.set_text_corpus_budget(std::sync::Arc::new(
        nrese_exec::Budget::new(bytes).within(runtime.query_memory()),
    ));
    engine.set_corpus_tuning(tuning);
}

/// The read model of RDF4J's `infer` parameter.
fn read_model(infer: bool) -> crate::ReadModel {
    match infer {
        true => crate::ReadModel::Materialised,
        false => crate::ReadModel::Asserted,
    }
}

/// The status of its answers that `prepared`'s reader is told (round 13, R13-diag). A
/// reader of some graphs who sees every inference (`inferred = "visible"`) is told
/// `not-certified` wherever the OWL 2 QL rewriting ran: its assessment reads the schema of
/// every graph. Whether the rewriting runs depends on the query and the configuration, not
/// on the schema, so the token tells nothing of hidden data.
fn status_for_its_reader(
    prepared: &PreparedQuery,
    status: Option<nrese_sparql::Completeness>,
    rewritten: bool,
) -> Option<nrese_sparql::Completeness> {
    match rewritten && withholds_certification(prepared) {
        true => Some(nrese_sparql::Completeness::not_certified()),
        false => status,
    }
}

/// [`status_for_its_reader`] for an answer that carries the rewriting's report too, as an
/// EXPLAIN does: the status as its reader is told it, and no report where that is withheld.
fn certification_for_its_reader(
    prepared: &PreparedQuery,
    completeness: &mut Option<nrese_sparql::Completeness>,
    ql: &mut Option<nrese_sparql::ql::QlReport>,
) {
    *completeness = status_for_its_reader(prepared, completeness.take(), ql.is_some());
    if withholds_certification(prepared) {
        *ql = None;
    }
}

/// Whether `prepared`'s reader is told no assessment of the rewriting, neither its status
/// nor its report ([`status_for_its_reader`]).
fn withholds_certification(prepared: &PreparedQuery) -> bool {
    prepared
        .access()
        .is_some_and(|access| access.inferred && !access.inferred_by_support)
}

/// A run or an EXPLAIN of `prepared` as its reader may be told it (round 13, R13-diag). A
/// reader of every graph is told everything. Another is told only what comes from its query
/// and its answer: `redact` takes an EXPLAIN's operators (their estimates, the order and the
/// operators those chose, their rows, the dictionary work they report, their times) and its
/// rewrites (an estimate decides some, as eager aggregation), all read from statistics and
/// scans of every graph, and its QL rewriting's status and report where they would read
/// the schema of every graph ([`certification_for_its_reader`]); and a memory refusal goes
/// through the scope's boundary ([`crate::ReadScope::told`]) as `entry`'s.
fn for_its_reader<T>(
    prepared: &PreparedQuery,
    entry: crate::EntryPoint,
    whole: StoreResult<T>,
    redact: impl FnOnce(&mut T),
) -> StoreResult<T> {
    let scope = crate::ReadScope::of(prepared.access().cloned());
    let mut whole = scope.told(entry, whole)?;
    if !scope.reads_everything() {
        redact(&mut whole);
    }
    Ok(whole)
}

/// Test seams in the reasoning certificate's lifecycle: each runs a test's closure once, on the
/// thread that reaches it.
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    use std::thread::LocalKey;

    pub(crate) type Hook = RefCell<Option<Box<dyn FnOnce()>>>;

    thread_local! {
        /// A bulk load has cleared the reasoning certificate and is about to load.
        pub(crate) static AFTER_INVALIDATION: Hook = const { RefCell::new(None) };
        /// A rematerialisation has published its inferences and is about to certify them.
        pub(crate) static AFTER_PUBLICATION: Hook = const { RefCell::new(None) };
        /// A change of the QL rewriting setting has read the certificate and is about to
        /// install the rewriting it calls for.
        pub(crate) static AFTER_QL_READ: Hook = const { RefCell::new(None) };
        /// A pipeline write has passed its preflight certificate check and is about to take
        /// its transaction.
        pub(crate) static AFTER_PREFLIGHT: Hook = const { RefCell::new(None) };
        /// An entailment check has found the certificate current and is about to read the
        /// closure.
        pub(crate) static AFTER_CERTIFIED: Hook = const { RefCell::new(None) };
        /// An invalidation of the certificate has begun its change of the epoch, and the old
        /// certificate is still in place.
        pub(crate) static INSIDE_INVALIDATION: Hook = const { RefCell::new(None) };
    }

    pub(crate) fn run(hook: &'static LocalKey<Hook>) {
        if let Some(hook) = hook.with_borrow_mut(Option::take) {
            hook();
        }
    }
}

/// The reasoning certificate says what the inferred stack is exact for (`reasoning_state`,
/// `reasoning_is_current`, `consistency`). It must never claim a ruleset current for data that
/// ruleset's closure wasn't computed over: queries and the pipeline rely on it (the pipeline
/// then skips its rematerialisation, `MutationPipeline::apply`). Each witness races a bulk load
/// that adds `C subClassOf D` against an RDFS rematerialisation over `a a C`, at one point of
/// the race, and then asks: if RDFS is certified current, `a a D` must hold. Where the writer
/// slot is free at that point the other operation completes there; where it is held, it waits
/// for the slot (the test waits for it up to `WAIT`, then lets the first one go on), and every
/// order then reaches the same answer.
#[cfg(test)]
mod certificate_tests {
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use super::{StoreService, hooks};
    use crate::StoreConfig;

    const WAIT: std::time::Duration = std::time::Duration::from_secs(2);
    const A_C: &str = "<http://example.com/a> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
                       <http://example.com/C> .\n";
    const C_D: &str = "<http://example.com/C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> \
                       <http://example.com/D> .\n";

    fn rdfs() -> nrese_reasoner::RuleProgram {
        nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Rdfs)
            .materialised_program()
            .expect("RDFS reasons")
    }

    fn load(store: &StoreService, file: &std::path::Path) {
        store
            .bulk_load(&crate::BulkLoadRequest {
                files: vec![file.to_path_buf()],
                replace: false,
                graph: crate::GraphTarget::DefaultGraph,
                invalid: None,
            })
            .unwrap();
    }

    /// Makes `hook` run `other` on a thread of its own, waiting for it up to `WAIT`; the
    /// returned slot holds that thread, to be joined once the operation that reached the hook
    /// is done.
    fn interleave(
        hook: &'static std::thread::LocalKey<hooks::Hook>,
        other: impl FnOnce() + Send + 'static,
    ) -> Arc<Mutex<Option<JoinHandle<()>>>> {
        let joined = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&joined);
        hook.set(Some(Box::new(move || {
            let (done, finished) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                other();
                let _ = done.send(());
            });
            let _ = finished.recv_timeout(WAIT);
            *slot.lock().unwrap() = Some(thread);
        })));
        joined
    }

    fn join(joined: &Mutex<Option<JoinHandle<()>>>) {
        let thread = joined.lock().unwrap().take().expect("the hook ran");
        thread.join().expect("the interleaved operation succeeds");
    }

    /// What is wrong with the certificate: RDFS claimed current without `a a D`.
    fn stale(store: &StoreService, when: &str, wrong: &mut Vec<String>) {
        let answer = store
            .execute_query_str("ASK { <http://example.com/a> a <http://example.com/D> }")
            .unwrap();
        let derived = String::from_utf8_lossy(&answer.payload).contains("true");
        let current = store.reasoning_is_current(rdfs());
        eprintln!("{when}: RDFS certified current {current}, a a D {derived}");
        if current && !derived {
            wrong.push(format!(
                "{when}: RDFS certified current over data it wasn't computed for"
            ));
        }
    }

    fn files(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let (a, b) = (dir.join("a.nt"), dir.join("b.nt"));
        std::fs::write(&a, A_C).unwrap();
        std::fs::write(&b, C_D).unwrap();
        (a, b)
    }

    /// C1: a rematerialisation between a bulk load's clearing of the certificate and its load.
    /// Before the fix the load cleared the certificate before it took the writer slot: the
    /// rematerialisation then certified RDFS, and the load committed `C subClassOf D` under it.
    #[test]
    fn a_bulk_load_raced_by_a_rematerialisation_leaves_no_stale_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = files(dir.path());
        let store_dir = dir.path().join("store");
        let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
        load(&store, &a);
        let racer = store.clone();
        let joined = interleave(&hooks::AFTER_INVALIDATION, move || {
            racer.rematerialise(rdfs()).unwrap();
        });
        load(&store, &b);
        join(&joined);
        let mut wrong = Vec::new();
        stale(&store, "in process", &mut wrong);
        drop(store);
        let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
        stale(&store, "after a reopen", &mut wrong);
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The QL schema is part of the certificate (sol-loop r1, S3): a bulk load between a QL
    /// setting's read of the certificate and its installing the rewriting. The rewriting
    /// relies on the `owl2-rl` closure; once the load leaves no certificate, none may be
    /// installed. Before the fix the setter read the certificate without the writer slot,
    /// the load cleared it and committed, and the setter installed the rewriting after.
    #[test]
    fn a_ql_setting_raced_by_a_bulk_load_installs_no_rewriting_without_a_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = files(dir.path());
        let store = StoreService::new(StoreConfig::on_disk(dir.path().join("store"))).unwrap();
        load(&store, &a);
        let rl = nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl)
            .materialised_program()
            .expect("OWL 2 RL reasons");
        store.rematerialise(rl).unwrap();
        let racer = store.clone();
        let joined = interleave(&hooks::AFTER_QL_READ, move || load(&racer, &b));
        store.set_ql_rewriting(crate::QlRewritingMode::On);
        join(&joined);
        let rewriting = store.query_settings().ql().is_some();
        let certified = store.reasoning_state().is_some();
        eprintln!("QL rewriting {rewriting}, certificate {certified}");
        assert!(
            !rewriting || certified,
            "QL rewriting installed over data no certificate covers"
        );
    }

    fn rl() -> nrese_reasoner::RuleProgram {
        nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl)
            .materialised_program()
            .expect("OWL 2 RL reasons")
    }

    fn holds(store: &StoreService, pattern: &str) -> bool {
        let answer = store
            .execute_query_str(&format!("ASK {{ {pattern} }}"))
            .unwrap();
        String::from_utf8_lossy(&answer.payload).contains("true")
    }

    /// A reasoning write leaves the closure of the data it commits over (review r1, F1): a bulk
    /// load between a write's preflight check of the certificate and its transaction commits
    /// `C subClassOf D` and clears the certificate. The write (`a a C` again, which adds
    /// nothing) must not maintain from the stale closure: afterwards `a a D` holds. Before the
    /// fix the pipeline trusted its preflight check, committed over the load, and `a a D` was
    /// missing under its RDFS reasoning.
    #[test]
    fn a_reasoning_write_raced_by_a_bulk_load_commits_over_a_complete_closure() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = files(dir.path());
        let store = StoreService::new(StoreConfig::on_disk(dir.path().join("store"))).unwrap();
        load(&store, &a);
        let pipeline = crate::MutationPipeline::new(
            Arc::new(store.clone()),
            Arc::new(nrese_reasoner::ReasonerService::new(
                nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Rdfs),
            )),
        );
        let write = "INSERT DATA { <http://example.com/a> \
                     <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/C> }";
        pipeline.execute_update_str(write).unwrap();
        let racer = store.clone();
        let joined = interleave(&hooks::AFTER_PREFLIGHT, move || load(&racer, &b));
        pipeline.execute_update_str(write).unwrap();
        join(&joined);
        let derived = holds(&store, "<http://example.com/a> a <http://example.com/D>");
        eprintln!("after the write: a a D {derived}");
        assert!(
            derived,
            "the write committed over a closure the load made stale"
        );
    }

    /// An entailment answers from one certified closure (review r1, F1, the read side): a bulk
    /// replace with the same statements (it clears the inferred stack and the certificate)
    /// between the check of the certificate and the read of the closure. The data entails
    /// `a a D` before and after; the answer must not be `NotEntailed` (refusing as not current
    /// is right). Before the fix the check passed and the read found the cleared stack.
    #[test]
    fn an_entailment_raced_by_a_bulk_replace_answers_from_a_certified_closure() {
        let dir = tempfile::tempdir().unwrap();
        let both = dir.path().join("both.nt");
        std::fs::write(&both, format!("{A_C}{C_D}")).unwrap();
        let store = StoreService::new(StoreConfig::on_disk(dir.path().join("store"))).unwrap();
        load(&store, &both);
        store.rematerialise(rdfs()).unwrap();
        let racer = store.clone();
        let joined = interleave(&hooks::AFTER_CERTIFIED, move || {
            racer
                .bulk_load(&crate::BulkLoadRequest {
                    files: vec![both],
                    replace: true,
                    graph: crate::GraphTarget::DefaultGraph,
                    invalid: None,
                })
                .unwrap();
        });
        let node =
            |name: &str| nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/{name}"));
        let conclusion = [nrese_rdf::Triple::new(
            node("a"),
            nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            node("D"),
        )];
        let answer = store.entails(rdfs(), &conclusion);
        join(&joined);
        eprintln!("the answer: {answer:?}");
        assert!(
            !matches!(answer, Ok(crate::Entailment::NotEntailed)),
            "NotEntailed for a conclusion the data entails"
        );
    }

    /// An entailment that starts inside an invalidation answers from a certified closure
    /// (fix check r1 on review r1, F1's read side): a bulk replace with the same statements has
    /// begun clearing the certificate, the old one still in place, when an entailment starts
    /// and finds it current; the replace then clears it, commits an empty inferred stack, and
    /// only then does the entailment read the closure. The data entails `a a D` before and
    /// after; the answer must not be `NotEntailed`. Before the fix the reader's epoch was taken
    /// after the invalidation's one increment, so it was the same after its reads, and the
    /// answer from the cleared stack stood.
    #[test]
    fn an_entailment_started_inside_an_invalidation_answers_from_a_certified_closure() {
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let both = dir.path().join("both.nt");
        std::fs::write(&both, format!("{A_C}{C_D}")).unwrap();
        let store = StoreService::new(StoreConfig::on_disk(dir.path().join("store"))).unwrap();
        load(&store, &both);
        store.rematerialise(rdfs()).unwrap();
        let (certified, has_certified) = mpsc::channel();
        let (go, gone) = mpsc::channel::<()>();
        let reader = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&reader);
        let racer = store.clone();
        hooks::INSIDE_INVALIDATION.set(Some(Box::new(move || {
            let thread = std::thread::spawn(move || {
                hooks::AFTER_CERTIFIED.set(Some(Box::new(move || {
                    let _ = certified.send(());
                    let _ = gone.recv_timeout(WAIT);
                })));
                let node = |name: &str| {
                    nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/{name}"))
                };
                racer.entails(
                    rdfs(),
                    &[nrese_rdf::Triple::new(
                        node("a"),
                        nrese_rdf::NamedNode::new_unchecked(
                            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                        ),
                        node("D"),
                    )],
                )
            });
            let _ = has_certified.recv_timeout(WAIT);
            *slot.lock().unwrap() = Some(thread);
        })));
        store
            .bulk_load(&crate::BulkLoadRequest {
                files: vec![both],
                replace: true,
                graph: crate::GraphTarget::DefaultGraph,
                invalid: None,
            })
            .unwrap();
        let _ = go.send(());
        let thread = reader
            .lock()
            .unwrap()
            .take()
            .expect("the invalidation ran its hook");
        let answer = thread.join().expect("the entailment returns");
        eprintln!("the answer: {answer:?}");
        assert!(
            !matches!(answer, Ok(crate::Entailment::NotEntailed)),
            "NotEntailed for a conclusion the data entails"
        );
    }

    /// Spec (Q06 fix check r2): an open certificate transition refuses without waiting;
    /// once settled and certified, the RDFS subclass conclusion still holds.
    #[test]
    fn an_entailment_refuses_an_open_certificate_transition_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let both = dir.path().join("both.nt");
        std::fs::write(&both, format!("{A_C}{C_D}")).unwrap();
        let store = StoreService::new(StoreConfig::on_disk(dir.path().join("store"))).unwrap();
        load(&store, &both);
        store.rematerialise(rdfs()).unwrap();
        let conclusion = [nrese_rdf::Triple::new(
            nrese_rdf::NamedNode::new_unchecked("http://example.com/a"),
            nrese_rdf::vocab::rdf::TYPE.into_owned(),
            nrese_rdf::NamedNode::new_unchecked("http://example.com/D"),
        )];
        let racer = store.clone();
        let during = conclusion.clone();
        hooks::INSIDE_INVALIDATION.set(Some(Box::new(move || {
            // Synchronous: the transition cannot settle until this call returns.
            // The fleet red's external timeout witnesses the old unbounded loop.
            assert!(matches!(
                racer.entails(rdfs(), &during),
                Err(crate::StoreError::Configuration(message))
                    if message == "the inferred statements aren't rdfs's closure: rematerialise first"
            ));
        })));
        store.invalidate_reasoning().unwrap();
        store.rematerialise(rdfs()).unwrap();
        assert_eq!(
            store.entails(rdfs(), &conclusion).unwrap(),
            crate::Entailment::Entailed
        );
    }

    /// Spec (Q06 fix check r2): a certificate changed during reads refuses even when
    /// the replacement is certified, instead of retrying to an answer.
    #[test]
    fn an_entailment_refuses_a_certificate_changed_during_its_reads() {
        let dir = tempfile::tempdir().unwrap();
        let both = dir.path().join("both.nt");
        std::fs::write(&both, format!("{A_C}{C_D}")).unwrap();
        let store = StoreService::new(StoreConfig::on_disk(dir.path().join("store"))).unwrap();
        load(&store, &both);
        store.rematerialise(rdfs()).unwrap();
        let racer = store.clone();
        hooks::AFTER_CERTIFIED.set(Some(Box::new(move || {
            racer.invalidate_reasoning().unwrap();
            racer.rematerialise(rdfs()).unwrap();
        })));
        let conclusion = [nrese_rdf::Triple::new(
            nrese_rdf::NamedNode::new_unchecked("http://example.com/a"),
            nrese_rdf::vocab::rdf::TYPE.into_owned(),
            nrese_rdf::NamedNode::new_unchecked("http://example.com/D"),
        )];
        assert!(matches!(
            store.entails(rdfs(), &conclusion),
            Err(crate::StoreError::Configuration(message))
                if message == "the inferred statements aren't rdfs's closure: rematerialise first"
        ));
        assert_eq!(
            store.entails(rdfs(), &conclusion).unwrap(),
            crate::Entailment::Entailed
        );
    }

    const INVERSE: &str = "<http://example.com/p> <http://www.w3.org/2002/07/owl#inverseOf> \
                           <http://example.com/q> .\n<http://example.com/a> <http://example.com/p> \
                           <http://example.com/b> .\n";
    const B_Q_A: &str = "<http://example.com/b> <http://example.com/q> <http://example.com/a>";

    /// An on-disk store in `dir` whose closure and certificate are OWL 2 RL's over `INVERSE`
    /// (`b q a` inferred).
    fn rl_certified(dir: &std::path::Path) -> std::path::PathBuf {
        let data = dir.join("inverse.nt");
        std::fs::write(&data, INVERSE).unwrap();
        let store_dir = dir.join("store");
        let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
        load(&store, &data);
        store.rematerialise(rl()).unwrap();
        assert!(holds(&store, B_Q_A), "OWL 2 RL infers b q a");
        store_dir
    }

    /// What is wrong after a start: OWL 2 RL certified over a closure without `b q a`.
    fn rl_certified_wrongly(store_dir: &std::path::Path, row: &str, wrong: &mut Vec<String>) {
        let store = StoreService::new(StoreConfig::on_disk(store_dir)).unwrap();
        let current = store.reasoning_is_current(rl());
        let inferred = holds(&store, B_Q_A);
        eprintln!("row {row}: OWL 2 RL certified current {current}, b q a {inferred}");
        if current && !inferred {
            wrong.push(format!("{row}: OWL 2 RL certified over the RDFS closure"));
        }
    }

    const F3_CHILD: &str = "NRESE_Q06_F3_CHILD";

    /// A replacement closure never reopens under the certificate of the one it replaced
    /// (review r1, F3): over an OWL 2 RL closure and certificate, an RDFS rematerialisation
    /// publishes and the process stops before its certificate (a child process of this test
    /// binary exits at the seam, unwinding nothing); in the other row the new certificate's
    /// write fails before its rename. After a start the store must not certify OWL 2 RL over
    /// the RDFS closure. Before the fix the old certificate stayed in place until the new one
    /// replaced it, and the start took it.
    #[test]
    fn a_replacement_closure_never_reopens_under_the_old_certificate() {
        if let Ok(store_dir) = std::env::var(F3_CHILD) {
            let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
            hooks::AFTER_PUBLICATION.set(Some(Box::new(|| {
                std::process::exit(86);
            })));
            let _ = store.rematerialise(rdfs());
            return;
        }
        let mut wrong = Vec::new();
        let dir = tempfile::tempdir().unwrap();
        let store_dir = rl_certified(dir.path());
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "service::certificate_tests::a_replacement_closure_never_reopens_under_the_old_certificate",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(F3_CHILD, &store_dir)
            .status()
            .unwrap();
        eprintln!("row stopped after publication: the child {status}");
        if status.code() != Some(86) {
            wrong.push(format!(
                "stopped after publication: the child didn't stop there: {status}"
            ));
        }
        rl_certified_wrongly(&store_dir, "stopped after publication", &mut wrong);
        let dir = tempfile::tempdir().unwrap();
        let store_dir = rl_certified(dir.path());
        {
            let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
            crate::metadata::tests::FAIL_NAMING_OF.set(Some(store_dir.clone()));
            let rematerialised = store.rematerialise(rdfs());
            crate::metadata::tests::FAIL_NAMING_OF.set(None);
            eprintln!(
                "row certificate write fails: {:?}",
                rematerialised.as_ref().err()
            );
        }
        rl_certified_wrongly(&store_dir, "certificate write fails", &mut wrong);
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// C2: a bulk load between a rematerialisation's publication and its certificate. Before
    /// the fix the rematerialisation released the writer slot when it published: the load then
    /// committed `C subClassOf D`, and the certificate came after it.
    #[test]
    fn a_rematerialisation_raced_by_a_bulk_load_leaves_no_stale_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = files(dir.path());
        let store_dir = dir.path().join("store");
        let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
        load(&store, &a);
        let racer = store.clone();
        let joined = interleave(&hooks::AFTER_PUBLICATION, move || load(&racer, &b));
        store.rematerialise(rdfs()).unwrap();
        join(&joined);
        let mut wrong = Vec::new();
        stale(&store, "in process", &mut wrong);
        drop(store);
        let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
        stale(&store, "after a reopen", &mut wrong);
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}

#[cfg(test)]
mod marker_tests {
    use super::StoreService;
    use crate::StoreConfig;
    // The reasoning marker is written as the store's metadata: its directory sync, after the
    // rename, fails through metadata's fault (`metadata::tests::FAIL_DIR_SYNC`).
    use crate::metadata::tests::FAIL_DIR_SYNC as FAIL_MARKER_SYNC;

    /// A reasoning marker whose publication failed after its rename is still cleared by the
    /// next clear of the inferences (DUR-3 fix check r1, R2-F2). A rematerialisation under RDFS
    /// of `a a C . C subClassOf D` publishes its inferences, renames its marker into place, and
    /// the marker's directory sync then fails: `InferencesNotRecorded`. The inferences are
    /// cleared; the store must no longer claim them current, then or after a reopen, and a later
    /// RDFS rematerialisation derives `a a D`. Before the fix the marker stayed while the store
    /// took it for gone, so the clear left it: after it, the store claimed RDFS current with no
    /// inferences (a wrong `NotEntailed` for `a a D`).
    #[test]
    fn a_marker_whose_placement_failed_is_still_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data.nt");
        std::fs::write(
            &data,
            "<http://example.com/a> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
             <http://example.com/C> .\n<http://example.com/C> \
             <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example.com/D> .\n",
        )
        .unwrap();
        let store_dir = dir.path().join("store");
        let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
        // A bulk load invalidates the reasoning: the store takes no marker to be in place.
        store
            .bulk_load(&crate::BulkLoadRequest {
                files: vec![data],
                replace: false,
                graph: crate::GraphTarget::DefaultGraph,
                invalid: None,
            })
            .unwrap();
        let program = nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Rdfs)
            .materialised_program()
            .expect("RDFS reasons");
        FAIL_MARKER_SYNC.set(true);
        let refused = store.rematerialise(&program);
        FAIL_MARKER_SYNC.set(false);
        assert!(
            matches!(refused, Err(crate::StoreError::InferencesNotRecorded(_))),
            "{refused:?}"
        );
        store.clear_inferred().unwrap();
        let mut wrong = Vec::new();
        if store.reasoning_state().is_some() {
            wrong.push("after the clear, the inferences are claimed current");
        }
        drop(store);
        let store = StoreService::new(StoreConfig::on_disk(&store_dir)).unwrap();
        if store.reasoning_state().is_some() {
            wrong.push("after a reopen, the inferences are claimed current");
        }
        store.rematerialise(&program).unwrap();
        let answer = store
            .execute_query_str("ASK { <http://example.com/a> a <http://example.com/D> }")
            .unwrap();
        if !String::from_utf8_lossy(&answer.payload).contains("true") {
            wrong.push("a later RDFS rematerialisation doesn't derive a a D");
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}

#[cfg(test)]
mod memory_stop_tests {
    use super::MemoryStop;
    use crate::StoreError;
    use nrese_exec::memory::{MemoryStopCause, MemoryWatch, Reading, Unread};
    use nrese_reasoner::delta::Interrupted;

    fn failed() -> Reading {
        Err(Unread::Failed)
    }

    /// DC-07: an operation whose store watch can't read the process's memory stops, and its
    /// error says the memory was unreadable, with the limit it couldn't enforce.
    #[test]
    fn a_store_watch_that_cant_read_stops_as_memory_unreadable() {
        let stop = MemoryStop(Some(MemoryWatch::with_reader(u64::MAX, failed)));
        assert!(stop.exceeded());
        assert!(matches!(
            stop.error(Interrupted::default()),
            Some(StoreError::MemoryUnreadable { limit: u64::MAX })
        ));
    }

    /// DC-07 S2 (Sol, round 1): an operation the process limit stopped is named by the cause
    /// it kept when it stopped, not by a later reading: unread, or past the limit; one its
    /// caller stopped is no memory error.
    #[test]
    fn a_process_limit_stop_is_named_by_the_operations_cause() {
        let unwatched = MemoryStop(None);
        let error = |memory| unwatched.error(Interrupted { memory });
        assert!(matches!(
            error(Some(MemoryStopCause::Unread)),
            Some(StoreError::MemoryUnreadable { .. })
        ));
        assert!(matches!(
            error(Some(MemoryStopCause::OverLimit)),
            Some(StoreError::ProcessMemoryLimit { .. })
        ));
        assert!(error(None).is_none());
    }
}
