use std::fmt;
use std::path::{Path, PathBuf};

use nrese_engine::{Engine, EngineConfig, EngineError, ReadModel, Snapshot, TermId};
use nrese_exec::stop::StopSource;
use nrese_sparql::CancellationToken;

use crate::backup::{DatasetBackupArtifact, DatasetBackupFormat, DatasetExport, export_dataset};
use crate::bulk_load::{BulkLoadReport, BulkLoadRequest, bulk_load};
use crate::config::{StoreConfig, StoreMode};
use crate::dl::query::{Revision, answer, certify_closure, plan_status};
use crate::error::StoreResult;
use crate::graph_store::{
    GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport, GraphWriteRequest,
};
use crate::graph_store_executor::{graph_exists, write_graph};
use crate::loader::preload_ontology;
use crate::mutation::disposition::{Disposition, Route};
use crate::mutation::{MutationCommand, MutationCommitReport};
use crate::query::{SerializedQueryResult, SparqlQueryRequest};
use crate::query_executor::{PreparedQuery, explain_prepared, plan_prepared, run_query};
use crate::stats::{StoreStats, collect_stats};
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};

/// A status header's text as its transport sends it, kept with a cached answer
/// ([`StoreService::run_query_reporting`]).
pub type StatusHeader = Option<std::sync::Arc<str>>;

/// One run of a prepared query, opened by the store that runs it ([`StoreService::open_run`]),
/// with the memory envelope everything of the run draws on: its evaluations, retained results
/// and DL tasks, and what its transport holds of its output (an answer held whole,
/// [`Self::memory`]). Each run has an envelope of its own and executes once: two runs of one
/// prepared query, at once or one after another, share neither a limit nor a refusal, and
/// nothing else hands a query an envelope.
pub struct QueryRun {
    store: StoreService,
    prepared: PreparedQuery,
    memory: nrese_exec::Envelope,
}

impl QueryRun {
    /// The run's envelope, for what its transport holds of its output.
    pub fn memory(&self) -> &nrese_exec::Envelope {
        &self.memory
    }

    /// [`StoreService::run_query_reporting`], as this run.
    pub fn run_reporting(
        self,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>, StatusHeader) -> Option<String>,
    ) -> StoreResult<()> {
        self.store
            .run_reporting(&self.prepared, &self.memory, cancellation, out, report)
            .map(|_| ())
    }

    /// [`StoreService::run_query_pending_reporting`], as this run.
    pub fn run_pending_reporting(
        self,
        pending: &crate::StatementsRequest,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>, StatusHeader) -> Option<String>,
    ) -> StoreResult<()> {
        let (prepared, memory) = (&self.prepared, &self.memory);
        self.store
            .run_pending_reporting(pending, prepared, memory, cancellation, out, report)
    }

    /// [`StoreService::explain_query`], as this run.
    pub fn explain(self, cancellation: &CancellationToken) -> StoreResult<crate::Explanation> {
        self.store
            .explain_in(&self.prepared, &self.memory, cancellation)
    }

    /// [`StoreService::plan_query`], as this run.
    pub fn plan(self, cancellation: &CancellationToken) -> StoreResult<crate::PlannedQuery> {
        self.store
            .plan_in(&self.prepared, &self.memory, cancellation)
    }
}

/// The dataset and the operations on it. Reads run on an engine snapshot and never wait for
/// writers. Writes go through [`MutationPipeline`](crate::MutationPipeline), which gates
/// them (access, reasoning, consistency, SHACL); the store has no public write that
/// commits around it. Bulk loads, rematerialisation and a replica's log are operations of
/// their own.
#[derive(Clone)]
pub struct StoreService {
    config: StoreConfig,
    /// [`StoreConfig::process_memory_bytes`], found at its first need
    /// ([`Self::process_memory_bytes`]).
    process_memory: crate::ProcessMemory,
    /// What reads the memory the process may use: [`nrese_exec::memory::capacity`], or a
    /// test's.
    capacity: fn() -> nrese_exec::memory::Reading,
    engine: Engine,
    preloaded_ontology: Option<PathBuf>,
    /// Whether the reasoning state file may exist (see [`Self::reasoning_state`]); saves a
    /// file-system call per write once it is gone.
    marker: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Shared by the store's clones: a clone with its own record would never see a commit's mark.
    pub(crate) lists: std::sync::Arc<crate::reasoning::ListsKept>,
    /// When queries are rewritten for OWL 2 QL ([`StoreConfig::ql_rewriting`], or the
    /// repository's own, [`Self::set_ql_rewriting`]).
    ql_mode: std::sync::Arc<std::sync::RwLock<crate::QlRewritingMode>>,
    /// What a classification answers outside OWL 2 DL ([`crate::DlConfig::outside_profile`],
    /// or the repository's own, [`Self::set_outside_profile`]).
    outside_profile: std::sync::Arc<std::sync::RwLock<crate::OutsideProfile>>,
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
    /// Why the published shapes count is still pending, if a refresh was deferred.
    pub(crate) shapes_deferred: std::sync::Arc<crate::shacl::ShapesDeferred>,
}

/// The file recording what the inferred stack is exact for ([`crate::reasoning_state`]).
pub(crate) const REASONING_MARKER: &str = "reasoning.state";

/// What the store knows about a version's reasoning, published with it in the engine's slot
/// ([`nrese_engine::Certificate`]). Readers access it through [`Certificate::record`].
pub(crate) struct Certificate {
    /// What the inferred stack is exact for, where [`Self::exact`].
    pub(crate) state: crate::ReasoningState,
    /// The state describes the stack of this version.
    pub(crate) exact: bool,
    /// The store vouches for the rows ([`StoreService::vouches`]): exact, not a replica, and
    /// made by the rules registered (or, with none registered, as the marker says).
    vouched: bool,
    /// The rules registered made the stack (`ReasoningState::made_by`): only then is `state`'s
    /// violation count the rules' word for this version.
    checked: bool,
    /// A rebuild in this process supplied the assessment, even without registered rules.
    rebuilt_here: bool,
    /// The rules assessed the whole closure.
    assessed: bool,
    /// A check found a clash and the store still holds it in quarantine.
    held: bool,
    /// Whether the rules equated a literal with another term in this version.
    pub(crate) literal: Literal,
    /// The QL rewriting its queries get, the same object a commit that maintains the closure
    /// notes its memberships in and vouches with.
    pub(crate) ql: Ql,
    /// [`nrese_sparql::QueryOptions::equality_closed`].
    pub(crate) equality_closed: bool,
    /// The `owl2-dl` mode's route, and whether user rules run with it ([`crate::dl::Dl`]).
    pub(crate) dl: bool,
    pub(crate) user_rules: bool,
    /// Its version's ill-typed literal ([`Self::ill_typed`]): unset until a need finds it, never
    /// at the open; a cell of its own, or one found for the same statements.
    pub(crate) ill_typed: std::sync::Mutex<IllTyped>,
}

/// A version's ill-typed literal, if any: `None` until a search ends.
pub(crate) type IllTyped = Option<Option<nrese_engine::TermId>>;

/// Whether the rules equated a literal with another term ([`Certificate::literal`]): reads of
/// typed values aren't `complete` unless it is `Clear` (`dl::query::withhold`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Literal {
    /// A rebuild or the matching registered rules establish that none were equated.
    Clear,
    /// A rebuild or a commit derived one.
    Set,
    /// Opened from a marker, under rules that may equate literals.
    Unknown,
}

/// What [`Certificate::record`] answers of a version: every verdict a reader gives of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Assessment {
    /// The rules' word: `Unknown` where no record vouched under the rules registered assessed
    /// the version, or on a replica.
    pub(crate) rules: crate::ConsistencyStatus,
    /// The rules' hold: the store isn't ready.
    pub(crate) held: bool,
    /// The rules assessed the whole version. Held and not assessed (a commit in quarantine
    /// before its revalidation, an invalidation): nothing may be answered by the rules.
    pub(crate) assessed: bool,
}

/// What a reader answering by the rules may take as the premise ([`Assessment::premise`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Premise {
    /// The rules' word, as `rules` says it.
    Word,
    /// Held without the rules' word, the whole version assessed (a bare open over a marker
    /// with violations): inconsistent.
    HeldInconsistent,
    /// Held and not assessed: no answer by the rules.
    Unassessed,
}

impl Assessment {
    /// What a reader answering by the rules takes as its premise.
    pub(crate) fn premise(&self) -> Premise {
        match (self.held, self.rules, self.assessed) {
            (true, crate::ConsistencyStatus::Unknown, false) => Premise::Unassessed,
            (true, crate::ConsistencyStatus::Unknown, true) => Premise::HeldInconsistent,
            _ => Premise::Word,
        }
    }
}

/// What a new exact record says beside its state ([`StoreService::certificate`]).
struct Facts {
    literal: Literal,
    held: bool,
    assessed: bool,
    checked: bool,
    rebuilt_here: bool,
}

/// What a read inside a transaction lacks ([`StoreService::assess`]), under `owl2-dl` with
/// [`CLOSURE`]; and where its operations may delete, [`DELETES`].
const PENDING: &str = "a read inside a transaction: its pending operations aren't reasoned over \
                       before the commit";
const CLOSURE: &str = " (the committed closure, no DL bounds)";
const NO_BOUNDS: &str = " (no DL bounds)";
const DELETES: &str = "the pending operations may delete statements whose inferences remain \
                       until the commit";

/// A certificate's QL rewriting, if it has one.
type Ql = Option<std::sync::Arc<nrese_sparql::ql::QlRewriting>>;

/// The whole-store explanation of an undecided OWL 2 QL verdict.
#[derive(Debug, serde::Serialize)]
pub struct UndecidedReport {
    pub gate: &'static str,
    pub disposition: &'static str,
    pub causes: [&'static str; 1],
    pub decided_by: &'static str,
}

impl Certificate {
    fn uses_owl2_ql(&self) -> bool {
        self.state.ruleset.split('+').next() == Some("owl2-ql")
    }

    /// The version's reasoning record. A pending snapshot carries its base's record.
    /// A withheld view has only the certificate for producing rows ([`Self::for_rows`]).
    pub(crate) fn record(snapshot: &Snapshot) -> Option<&Self> {
        snapshot.certificate()?.downcast_ref()
    }

    /// `snapshot`'s record where the store vouches for its rows; any other reads as none.
    pub(crate) fn of(snapshot: &Snapshot) -> Option<&Self> {
        Self::record(snapshot).filter(|c| c.vouched)
    }

    /// The one `snapshot`'s rows are produced under ([`Snapshot::certificate_for_rows`]),
    /// vouched.
    pub(crate) fn for_rows(snapshot: &Snapshot) -> Option<&Self> {
        let rows = snapshot.certificate_for_rows()?.downcast_ref::<Self>()?;
        rows.vouched.then_some(rows)
    }

    /// The version's verdict and hold. Replicas have no rules verdict or hold.
    pub(crate) fn assessment(snapshot: &Snapshot, replica: bool) -> Assessment {
        let Some(record) = Self::record(snapshot) else {
            let rules = crate::ConsistencyStatus::Unknown;
            return Assessment {
                rules,
                held: false,
                assessed: false,
            };
        };
        let word = !replica && record.exact && record.checked && record.assessed;
        Assessment {
            rules: match word {
                true => record.state.consistency(),
                false => crate::ConsistencyStatus::Unknown,
            },
            held: !replica && record.held,
            assessed: record.assessed,
        }
    }

    /// Whether `snapshot` was never reasoned over: no inferred statement, no certificate (not
    /// even one a withheld view keeps for its rows), and no rules in place (`rules`). Anything
    /// else without a certificate is a stack nothing vouches for.
    pub(crate) fn bare(snapshot: &Snapshot, rules: bool) -> bool {
        snapshot.len_in(ReadModel::Inferred) == 0
            && !rules
            && Self::record(snapshot).is_none()
            && Self::for_rows(snapshot).is_none()
    }

    /// Whether `snapshot` reads under `owl2-dl`, and whether user rules run with it: as its
    /// certificate for rows says, else as the mode `dl` is now (a read without one is told
    /// `not-certified` all the same).
    pub(crate) fn route(snapshot: &Snapshot, dl: &crate::dl::Dl) -> (bool, bool) {
        Self::for_rows(snapshot).map_or((dl.active(), dl.user_rules()), |c| (c.dl, c.user_rules))
    }

    /// The ill-typed literal `data`, this certificate's version, holds: found once, on the first
    /// need of a published snapshot, by one scan (`dl::query::ill_typed`; concurrent needs wait)
    /// that `stop` may end, `None` then, kept unset, so the need fails closed. A pending state
    /// reads the cell as it is. `Some(None)` outside `owl2-dl` and `owl2-ql`.
    pub(crate) fn ill_typed(&self, data: &Snapshot, stop: &dyn Fn() -> bool) -> IllTyped {
        if !crate::dl::query::keeps_ill_typed(self.dl, &self.state.ruleset) {
            return Some(None);
        }
        let mut cell = self.ill_typed.lock().unwrap_or_else(|p| p.into_inner());
        if cell.is_none() && data.shared_state().is_some() {
            *cell = crate::dl::query::ill_typed(data, stop);
        }
        *cell
    }
}

/// The memory stop of one operation ([`StoreService::memory_stop`]).
pub(crate) struct MemoryStop(Option<nrese_exec::memory::MemoryWatch>);

impl MemoryStop {
    /// The stop for a process memory limit as found ([`StoreService::process_memory_bytes`]):
    /// none for 0; one that reads as unread where the limit couldn't be found, so the
    /// operation has no room ([`Self::room`]) and stops ([`Self::exceeded`]): fail closed.
    fn of(limit: nrese_exec::memory::Reading) -> Self {
        use nrese_exec::memory::{MemoryWatch, Reading, Unread};
        fn unread() -> Reading {
            Err(Unread::Failed)
        }
        Self(match limit {
            Ok(0) => None,
            Ok(limit) => Some(MemoryWatch::new(limit)),
            Err(_) => Some(MemoryWatch::with_reader(0, unread)),
        })
    }

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
    pub fn new(config: StoreConfig) -> StoreResult<Self> {
        config
            .process_memory_bytes
            .supported(nrese_exec::memory::READS_MEMORY)?;
        let runtime = std::sync::Arc::new(crate::Runtime::for_process(
            config.process_memory_bytes,
            config.total_query_memory_bytes,
        ));
        Self::with_runtime(config, runtime)
    }

    /// Opens a store on resources shared with other stores. A catalog uses this for
    /// every repository; embedded applications can supply the same owner explicitly.
    /// The runtime's budgets apply, not `config.total_query_memory_bytes` or, for what is
    /// charged, `config.process_memory_bytes` (which stays this store's watch).
    pub fn with_runtime(
        config: StoreConfig,
        runtime: std::sync::Arc<crate::Runtime>,
    ) -> StoreResult<Self> {
        config.validate()?;
        let process_memory = config.process_memory_bytes;
        process_memory.supported(nrese_exec::memory::READS_MEMORY)?;
        // The process's settings, set once (D16-489): a later store's differ and are left.
        crate::process_settings::apply_process_settings(&config);
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
                        forget_on_cut: Some(REASONING_MARKER),
                        ..defaults.durability
                    },
                    ..defaults
                };
                Engine::open(&config.data_dir, engine_config)?
            }
        };
        engine.watch_graph(nrese_rdf::NamedNode::new_unchecked(
            config.shapes_graph.clone(),
        ))?;
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
        let outside_profile = config.dl.outside_profile;
        let settings = crate::query_executor::StoreSettings {
            union_default_graph: config.union_default_graph,
            geosparql_stated_only: config.geosparql_stated_only,
            runtime,
            services: std::sync::Arc::default(),
            equality_canonical: config.equality_canonical_answers,
            equality_early_expansion: config.equality_early_expansion,
            result_cache: (config.query_cache_bytes > 0).then(|| {
                std::sync::Arc::new(nrese_sparql::ResultCache::new(config.query_cache_bytes))
            }),
            search_scope: std::sync::Arc::new(std::sync::RwLock::new(config.search_scope)),
            reader_corpus_bytes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(
                config.reader_corpus_bytes,
            )),
        };
        let unavailable = std::sync::Arc::<std::sync::OnceLock<String>>::default();
        let namespaces = std::sync::Arc::new(crate::namespaces::Namespaces::open(
            (config.mode == StoreMode::OnDisk).then(|| config.data_dir.clone()),
            std::sync::Arc::clone(&unavailable),
        )?);
        // The support graph sets and restricted views draw on the memory the queries do,
        // as the canonical views.
        let supports = crate::support::Supports::new(std::sync::Arc::new(
            nrese_exec::Budget::unlimited().within(settings.runtime.query_memory()),
        ));
        // So does what client transactions keep between requests.
        let mut sessions = crate::sessions::Sessions::within(
            std::sync::Arc::new(
                nrese_exec::Budget::unlimited().within(settings.runtime.query_memory()),
            ),
            crate::sessions::SESSION_IDLE,
        );
        sessions.update_settings = Some((
            std::sync::Arc::clone(&namespaces),
            std::sync::Arc::clone(&settings.search_scope),
        ));
        let service = Self {
            process_memory,
            capacity: nrese_exec::memory::capacity,
            settings,
            namespaces,
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
            lists: Default::default(),
            ql_mode: std::sync::Arc::new(std::sync::RwLock::new(ql_mode)),
            outside_profile: std::sync::Arc::new(std::sync::RwLock::new(outside_profile)),
            last_materialisation: std::sync::Arc::default(),
            shapes_deferred: Default::default(),
        };
        // Every store a replica server opens is a replica's from its open on, the default and
        // the catalog's others alike: its `owl2-dl` mode interns no term (the default's
        // dictionary continues the primary's), and its refusals say the replica's sentence.
        if service.config.replica {
            service.dl.set_replica();
        }
        // The planner's statistics are the store's, shared by its requests: an envelope of
        // their own within the catalog's budget, exact (no request's credit: it lives as long
        // as the store).
        let kept = || nrese_exec::Envelope::exact(None, service.runtime().query_memory());
        service
            .engine
            .charge_statistics_to(std::sync::Arc::new(kept().part()));
        // So are the owl2-dl bounds' U1 and read views (SYN-61), the same way.
        service
            .dl
            .bounds
            .set_kept(std::sync::Arc::new(kept().part()));
        // Under the writer slot, as every change of the certificate (Q06 remainder, invariant C).
        let tx = service.engine.transaction();
        let writer = tx.writer().expect("a transaction holds the writer slot");
        if service.engine.snapshot().revision() != before {
            service.forget_marker(writer)?;
        }
        // The marker, read once, here: the open's record. The reading isn't on disk: the
        // certified stack's is installed before the store is anyone's, with its record. The
        // marker doesn't record whether the rules equated two literals.
        if let Some(state) = service.marker_state() {
            service.certify(&tx, state, Literal::Unknown);
        }
        drop(tx);
        service.refresh_kept_shapes(&service.engine.snapshot(), &StopSource::none());
        service.pin_configured_queries()?;
        Ok(service)
    }

    /// Installs the state and reading under the writer slot. Recertification carries the
    /// hold, assessment and reusable QL schema. Marker-only literal certainty requires
    /// matching registered rules that cannot equate literals.
    fn certify(
        &self,
        tx: &nrese_engine::Transaction<'_>,
        state: crate::ReasoningState,
        literal: Literal,
    ) {
        let writer = tx.writer().expect("a transaction holds the writer slot");
        let base = Certificate::record(tx.base()).filter(|c| c.exact);
        let hide = self.config.hide_unnamed_classes;
        let registered = self.supports.rules();
        let rebuilt_here = base.is_some_and(|c| c.rebuilt_here);
        let checked = registered
            .as_ref()
            .map_or(rebuilt_here, |p| state.made_by(p, hide));
        let cannot_equate = checked
            && registered.as_ref().is_some_and(|program| {
                !program.may_equate_literals() && state.ruleset == program.name()
            });
        let facts = Facts {
            literal: match literal {
                Literal::Unknown if cannot_equate => Literal::Clear,
                Literal::Clear if !rebuilt_here && !cannot_equate => Literal::Unknown,
                literal => literal,
            },
            held: base.map_or(state.violations > 0, |c| c.held),
            assessed: base.is_none_or(|c| c.assessed),
            checked,
            rebuilt_here,
        };
        let kept = base.and_then(|c| c.ql.clone());
        let reading = self.note_equality(writer, &state);
        tx.install(reading, Some(self.certificate(state, kept, facts)));
    }

    /// The current record made again under the settings now (a registration, the QL setting,
    /// the mode), `edit` applied to its state: nothing where no record describes the stack.
    fn recertify(
        &self,
        tx: &nrese_engine::Transaction<'_>,
        edit: impl FnOnce(&mut crate::ReasoningState),
    ) {
        let Some(base) = Certificate::record(tx.base()).filter(|c| c.exact) else {
            return;
        };
        let (mut state, literal) = (base.state.clone(), base.literal);
        edit(&mut state);
        self.certify(tx, state, literal);
    }

    /// The record a maintained commit `tx` publishes, from its base's: the same stack's state,
    /// QL schema and settings; its literal mark `Set` where the commit derived a literal
    /// equality; the gate's ill-typed witness, or its own search under the memory stop where
    /// the gate didn't search and the base's witness is known; assessed where the base isn't
    /// held (a commit in quarantine checks only its own facts: the hold carried, not assessed).
    /// `None` where no record describes the base's stack.
    pub(crate) fn successor(
        &self,
        tx: &nrese_engine::Transaction<'_>,
        literal_equality: bool,
        witness: Option<IllTyped>,
    ) -> Option<nrese_engine::Certificate> {
        let base = Certificate::record(tx.base()).filter(|c| c.exact)?;
        let cell = *base.ill_typed.lock().unwrap_or_else(|p| p.into_inner());
        let ill_typed = if crate::dl::query::keeps_ill_typed(base.dl, &base.state.ruleset) {
            witness.unwrap_or_else(|| {
                let memory = self.memory_stop();
                cell.and_then(|cell| {
                    crate::dl::query::ill_typed_after(tx, cell, &|| memory.exceeded())
                })
            })
        } else {
            cell
        };
        Some(std::sync::Arc::new(Certificate {
            state: base.state.clone(),
            ql: base.ql.clone(),
            equality_closed: base.equality_closed,
            dl: base.dl,
            user_rules: base.user_rules,
            ill_typed: std::sync::Mutex::new(ill_typed),
            exact: true,
            vouched: base.vouched,
            checked: base.checked,
            rebuilt_here: base.rebuilt_here,
            assessed: !base.held,
            held: base.held,
            literal: if literal_equality {
                Literal::Set
            } else {
                base.literal
            },
        }))
    }

    /// The record a publication that doesn't maintain the inferences carries, by a holder of
    /// the writer slot: no row vouched for, no verdict, the current record's hold kept; `None`
    /// where there is no hold to keep.
    fn withheld(&self, _writer: nrese_engine::WriterHeld<'_>) -> Option<nrese_engine::Certificate> {
        let current = self.engine.snapshot();
        let base = Certificate::record(&current).filter(|c| c.held)?;
        Some(std::sync::Arc::new(Certificate {
            state: base.state.clone(),
            ql: None,
            equality_closed: false,
            dl: base.dl,
            user_rules: base.user_rules,
            ill_typed: Default::default(),
            exact: false,
            vouched: false,
            checked: false,
            rebuilt_here: false,
            assessed: false,
            held: true,
            literal: Literal::Unknown,
        }))
    }

    /// Whether the store vouches for the stack `state` records: never on a replica (its reads
    /// are told `not-certified`, [`crate::StoreConfig::replica`]), and only where the rules
    /// registered made it ([`Self::use_reasoning_rules`]); with none registered, as its marker
    /// says, its label naming the marker's rules. Any other stack is read as stored and told
    /// `not-certified` until it is rebuilt. A reader of some graphs who sees inferences is
    /// refused with no rules registered all the same: the view of what its graphs support
    /// needs rules to be made at all.
    fn vouches(&self, state: &crate::ReasoningState) -> bool {
        let hide = self.config.hide_unnamed_classes;
        !self.config.replica && self.supports.vouches_for(state, hide)
    }

    /// `state`'s record of an exact stack under the store's settings now, with `facts`; `kept`,
    /// the QL schema to keep if its closure is the one `state` calls for. Whether the recorded
    /// reasoning closes the data under `owl:sameAs` (a ruleset with the equality rules):
    /// queries may then rely on it. Its ill-typed literal isn't looked for until a need asks
    /// ([`Certificate::ill_typed`]).
    fn certificate(
        &self,
        state: crate::ReasoningState,
        kept: Ql,
        facts: Facts,
    ) -> nrese_engine::Certificate {
        std::sync::Arc::new(Certificate {
            ql: self.ql_for(&state, kept),
            equality_closed: nrese_reasoner::RuleProgram::closes_equality(&state.ruleset),
            dl: self.dl.active(),
            user_rules: self.dl.user_rules(),
            vouched: self.vouches(&state),
            state,
            ill_typed: Default::default(),
            exact: true,
            checked: facts.checked,
            rebuilt_here: facts.rebuilt_here,
            assessed: facts.assessed,
            held: facts.held,
            literal: facts.literal,
        })
    }

    /// How the certified stack is read: a stack kept over representatives
    /// (`reasoner.equality = "compact"`) expanded by `owl:sameAs`, else as stored. Its
    /// publication installed that already; the open installs it ([`Self::certify`]).
    fn note_equality(
        &self,
        _writer: nrese_engine::WriterHeld<'_>,
        state: &crate::ReasoningState,
    ) -> Option<nrese_engine::TermId> {
        let closed = nrese_reasoner::RuleProgram::closes_equality(&state.ruleset);
        (closed && state.compact_equality)
            .then(|| {
                self.engine.snapshot().lookup(
                    nrese_rdf::NamedNodeRef::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
                        .into(),
                )
            })
            .flatten()
    }

    /// Switches the QL rewriting on for a closure it can rewrite over (docs/design/
    /// ql-rewriting.md §4): `owl2-ql` and `owl2-rl` make the data H-complete, `owl2-rl`
    /// with its list rules; the others don't close inverses or the domains of restrictions.
    /// Under `owl2-dl` the bounds give those answers ([`crate::dl`]): no rewriting.
    fn ql_for(&self, state: &crate::ReasoningState, kept: Ql) -> Ql {
        use nrese_sparql::ql::{Closure, QlRewriting};
        let closure = match state.ruleset.as_str() {
            "owl2-rl" => Some(Closure { lists: true }),
            "owl2-ql" => Some(Closure { lists: false }),
            _ => None,
        };
        let closure =
            closure.filter(|_| self.ql_rewriting().applies_to(&state.ruleset) && !self.dl.active());
        // The same closure keeps its compiled schema.
        kept.filter(|q| Some(q.closure()) == closure)
            .or_else(|| closure.map(|closure| std::sync::Arc::new(QlRewriting::new(closure))))
    }

    /// When queries are rewritten for OWL 2 QL.
    pub fn ql_rewriting(&self) -> crate::QlRewritingMode {
        *self.ql_mode.read().unwrap_or_else(|p| p.into_inner())
    }

    /// What a classification answers on an input outside OWL 2 DL.
    pub fn outside_profile(&self) -> crate::OutsideProfile {
        *self
            .outside_profile
            .read()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Changes what a classification answers outside OWL 2 DL (a repository's setting): the
    /// classifications that start from now on read it.
    pub fn set_outside_profile(&self, profile: crate::OutsideProfile) {
        *self
            .outside_profile
            .write()
            .unwrap_or_else(|p| p.into_inner()) = profile;
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
        #[cfg(test)]
        hooks::run(&hooks::AFTER_QL_READ);
        self.recertify(&tx, |_| {});
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

    /// The marker's state ([`crate::ReasoningState::from_text`]); read by the open only.
    fn marker_state(&self) -> Option<crate::ReasoningState> {
        let text = std::fs::read_to_string(self.marker_path()?).ok()?;
        crate::ReasoningState::from_text(&text)
    }

    /// What the inferred stack is exact for, as the current version's record says
    /// ([`Certificate::exact`]); `None` if unknown (never materialised, or a write that didn't
    /// maintain the inferences), and for a commit in quarantine until its revalidation's
    /// rebuild assesses the version (held, not assessed): the next commit rebuilds first.
    pub fn reasoning_state(&self) -> Option<crate::ReasoningState> {
        let snapshot = self.engine.snapshot();
        Self::reasoning_state_at(&snapshot).cloned()
    }

    fn reasoning_state_at(snapshot: &Snapshot) -> Option<&crate::ReasoningState> {
        Certificate::record(snapshot)
            .filter(|c| c.exact && c.assessed)
            .map(|c| &c.state)
    }

    /// Whether the data is consistent, starting from the current version's rules assessment
    /// ([`Certificate::assessment`]; see [`crate::reasoning_state`] for the quarantine this
    /// reports). `Unknown` on a replica, and where no record the rules registered checked
    /// assessed the version. Under `owl2-dl` or `owl2-ql`, also for no clash where the version
    /// holds an ill-typed literal, which OWL 2 doesn't admit, or the search for one, on this
    /// first need, stopped ([`Certificate::ill_typed`]). A clean OWL 2 QL assessment
    /// remains undecided until a consistency check can decide it.
    pub fn consistency(&self) -> crate::ConsistencyStatus {
        self.consistency_at(&self.engine.snapshot())
    }

    /// [`Self::consistency`] of `snapshot`'s version.
    pub(crate) fn consistency_at(&self, snapshot: &Snapshot) -> crate::ConsistencyStatus {
        use crate::ConsistencyStatus::{Consistent, Undecided, Unknown};
        let status = Certificate::assessment(snapshot, self.config.replica).rules;
        let Some(certificate) = Certificate::of(snapshot).filter(|_| status == Consistent) else {
            return status;
        };
        let memory = self.memory_stop();
        match certificate.ill_typed(snapshot, &|| memory.exceeded()) {
            Some(None) if certificate.uses_owl2_ql() => Undecided {
                violations: 0,
                why: crate::WhyUndecided::NoQlDecider,
            },
            Some(None) => status,
            _ => Unknown,
        }
    }

    /// The whole-store verdict and the disposition of an undecided OWL 2 QL assessment.
    pub fn undecided_report(&self) -> (crate::ConsistencyStatus, Option<UndecidedReport>) {
        use crate::{ConsistencyStatus::Undecided, WhyUndecided::NoQlDecider};
        let status = self.consistency();
        let report = match status {
            Undecided {
                why: NoQlDecider, ..
            } => Some(UndecidedReport {
                gate: "consistency",
                disposition: self
                    .dispose(NoQlDecider, Route::Published)
                    .published_status(),
                causes: ["no-decider"],
                decided_by: "owl2-dl",
            }),
            _ => None,
        };
        (status, report)
    }

    /// Whether the store is in quarantine, readable and repairable but not ready: the current
    /// version's record holds a rules clash, or the consistency policy quarantines its
    /// undecided OWL 2 QL verdict.
    pub fn is_quarantined(&self) -> bool {
        use crate::{ConsistencyStatus::Undecided, WhyUndecided::NoQlDecider};
        let snapshot = self.engine.snapshot();
        Certificate::assessment(&snapshot, self.config.replica).held
            || (Certificate::of(&snapshot).is_some_and(Certificate::uses_owl2_ql)
                && matches!(
                    self.consistency_at(&snapshot),
                    Undecided {
                        why: NoQlDecider,
                        ..
                    }
                )
                && self.dispose(NoQlDecider, Route::Published) == Disposition::Quarantined)
    }

    /// Whether the certificate covers `program` for a holder of the writer slot: no write can
    /// change it, or the data it covers, until the holder lets the slot go. A writer that maintains
    /// the closure checks it here, under its own transaction.
    pub(crate) fn reasoning_is_current_held(
        &self,
        _writer: nrese_engine::WriterHeld<'_>,
        program: &nrese_reasoner::RuleProgram,
    ) -> bool {
        self.reasoning_is_current(program)
    }

    /// Forgets what the inferred stack is exact for, taking the writer slot to do so: no
    /// rematerialisation certifies it meanwhile. Never call it under the slot (it isn't
    /// reentrant): a holder calls `Self::invalidate_held`.
    pub fn invalidate_reasoning(&self) -> StoreResult<()> {
        let tx = self.engine.transaction();
        let writer = tx.writer().expect("a transaction holds the writer slot");
        // The stack stays, read as it was; the version vouches for nothing, its hold kept.
        let reading = Certificate::record(tx.base())
            .filter(|c| c.exact)
            .and_then(|c| self.note_equality(writer, &c.state));
        let withheld = self.invalidate_held(writer)?;
        tx.install(reading, withheld);
        Ok(())
    }

    /// [`Self::invalidate_reasoning`] by a holder of the writer slot, before a publication that
    /// doesn't maintain the inferences: the marker goes first ([`Self::forget_marker`]), then
    /// the record that publication is to carry ([`Self::withheld`]) is returned. The record
    /// changes only with a publication, so no reader takes the old one for changed data.
    pub(crate) fn invalidate_held(
        &self,
        writer: nrese_engine::WriterHeld<'_>,
    ) -> StoreResult<Option<nrese_engine::Certificate>> {
        #[cfg(test)]
        hooks::run(&hooks::INSIDE_INVALIDATION);
        self.forget_marker(writer)?;
        Ok(self.withheld(writer))
    }

    /// Removes the marker durably, by a holder of the writer slot: a removal that fails fails
    /// the write before it changes anything, and leaves the current record over the data it
    /// was computed for.
    pub(crate) fn forget_marker(&self, _writer: nrese_engine::WriterHeld<'_>) -> StoreResult<()> {
        use std::sync::atomic::Ordering;
        if self.marker.load(Ordering::Acquire)
            && let Some(path) = self.marker_path()
        {
            // The removal durable too (DUR-3): a marker back after a power loss would claim the
            // inferred stack exact for reasoning it no longer reflects. Gone already (an
            // earlier try whose sync failed) still needs the sync ([`crate::metadata::remove`]).
            // Failed, the file may still be there, or its removal not durable: the next
            // invalidation tries again (the review of 3 October 2026, A4).
            crate::metadata::remove(&path).map_err(crate::error::StoreError::Io)?;
        }
        self.marker.store(false, Ordering::Release);
        Ok(())
    }

    /// Writes the marker for the inferred stack just published with `state`'s record, still
    /// under the writer slot of its publication ([`nrese_engine::Finished`]): no other write
    /// comes between them.
    fn record_reasoning(
        &self,
        _writer: nrese_engine::WriterHeld<'_>,
        state: &crate::ReasoningState,
    ) -> StoreResult<()> {
        let text = state.to_text();
        let Some(path) = self.marker_path() else {
            return Ok(());
        };
        // "A marker may be in place", before anything is written: a
        // publication that fails after its rename leaves the marker under its name, and the
        // next invalidation must remove it, or refuse, not skip it.
        self.marker
            .store(true, std::sync::atomic::Ordering::Release);
        // Synced and durably named, the store's path included (DUR-3): read
        // at open, a marker lost or short after a power loss would misstate what the inferred
        // stack is exact for.
        crate::metadata::write(&path, text.as_bytes())?;
        Ok(())
    }

    /// Empties the inferred stack (reasoning switched off), forgetting the marker. The rules
    /// no longer run, so their hold goes with the record.
    pub fn clear_inferred(&self) -> StoreResult<u64> {
        let rematerialisation = self.engine.rematerialisation();
        self.forget_marker(rematerialisation.writer())?;
        #[cfg(test)]
        hooks::run(&hooks::BEFORE_CLEAR_PUBLICATION);
        // An empty stack is read as stored, installed even where it was empty already.
        let finished = rematerialisation.finish_held(Vec::new(), None, None)?;
        Ok(finished.summary().inferred_deleted)
    }

    /// Waits until the engine's background maintenance has nothing requested or running
    /// ([`Engine::settle`]), or reports that the worker ended: for measurements of what the
    /// store holds.
    #[doc(hidden)]
    pub fn settle(&self) -> StoreResult<()> {
        self.engine.settle().map_err(Into::into)
    }

    /// The background maintenance worker's recorded error, once it has ended.
    pub fn maintenance_error(&self) -> Option<EngineError> {
        self.engine.maintenance_error()
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    /// The process memory limit this store watches ([`StoreConfig::process_memory_bytes`]);
    /// 0: none, read nothing. Else found at this first need, from the memory the process may
    /// use, read once per process ([`nrese_exec::memory::capacity`]; a later ask, or another
    /// store's, reads nothing). Opening a store needs it only where the open evaluates
    /// queries ([`StoreConfig::pinned_queries`]), which charge the process's budget. Unread where that memory can't be read: what asked can't be
    /// held to the limit, and stops (fail closed).
    pub fn process_memory_bytes(&self) -> nrese_exec::memory::Reading {
        self.process_memory.bytes_with(self.capacity)
    }

    /// This store reading the memory the process may use with `capacity`.
    #[cfg(test)]
    pub(crate) fn with_capacity(mut self, capacity: fn() -> nrese_exec::memory::Reading) -> Self {
        self.capacity = capacity;
        self
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
    /// Results that may hold RDF 1.2 terms announce it: the query makes or matches them
    /// ([`PreparedQuery::announce_rdf12`]), or a graph the reader reads holds a triple term.
    /// For a reader of every graph that is decided here, on the latest snapshot; for a reader
    /// of some graphs, by its first run on what it reads.
    pub fn prepare_query(&self, request: &SparqlQueryRequest) -> StoreResult<PreparedQuery> {
        let mut prepared = PreparedQuery::parse_with(request, Some(&self.namespaces.all()))?;
        if PreparedQuery::uses_rdf12(prepared.query()) {
            prepared.announce_rdf12();
        }
        if prepared.access().is_none() {
            prepared.announce_on(&self.engine.snapshot());
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
        let read = self.view_of(&self.engine.snapshot(), access, memory, Some(cancellation));
        #[cfg(test)]
        hooks::run(&hooks::AFTER_DECIDED);
        read
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

    /// The envelope a statement read's replay of a transaction's pending changes is charged
    /// to: the one its transport holds its answer in, where it holds it
    /// ([`crate::ReadContext::within`]: a reader of every graph's, over HTTP/1.0); else none
    /// of its own, so the queries', as [`Self::read_snapshot`]'s is.
    fn read_memory(&self, read: &crate::ReadContext<'_>) -> nrese_exec::Envelope {
        read.memory
            .clone()
            .unwrap_or_else(|| self.settings.envelope(None))
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

    fn query_view(
        &self,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancel: &CancellationToken,
    ) -> StoreResult<Snapshot> {
        let snapshot = self.engine.snapshot();
        let Some(scope) = &prepared.inference_scope else {
            return self.view_of(&snapshot, prepared.access(), memory, Some(cancel));
        };
        let view = self
            .reading(Some(scope), memory, Some(cancel))
            .expect("the dataset's selector requests inferred statements")
            .scoped(
                &snapshot,
                prepared.read_model(),
                prepared.access().is_some(),
            )?;
        self.refuse_exact(prepared, &snapshot, || {
            self.status_outside_dl(prepared, None, Revision::Committed(&view))
        })?;
        Ok(view)
    }

    pub(crate) fn support_compilation(&self) -> crate::support::Compilation {
        crate::support::Compilation {
            hide_unnamed_classes: self.config.hide_unnamed_classes,
            by_representatives: self.config.equality_by_representatives,
            compact: self.config.equality_compact,
            cap: self.config.support_sets,
            replica: self.dl.replica(),
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
    /// its own. What the store vouches for follows them (`Self::vouches`): a stack other
    /// rules made is read as stored and told `not-certified` until it is rebuilt; with `None`
    /// after a registration it is told what its marker says again. Taken under the writer
    /// slot, as every change of the certificate: it waits for a write, a bulk load or a
    /// rematerialisation running meanwhile, and must not be called by a holder of the slot.
    pub fn use_reasoning_rules(&self, rules: Option<nrese_reasoner::RuleProgram>) {
        let tx = self.engine.transaction();
        // The record made under the new rules is installed before the slot goes back; a
        // reader judges each version by its own record.
        self.supports.use_rules(rules);
        #[cfg(test)]
        hooks::run(&hooks::AFTER_RULES_REGISTERED);
        self.recertify(&tx, |_| {});
    }

    /// Switches the `owl2-dl` mode's work on or off ([`crate::dl`]): the mutation
    /// pipeline running the reasoner's `owl2-dl` mode registers it, and whether user rules
    /// run with it.
    pub fn use_dl(&self, active: bool, user_rules: bool) {
        // The writer slot covers the mode change and installation of its record.
        let tx = self.engine.transaction();
        self.dl.set_active(active, user_rules);
        // The record with the mode's route: on, no QL rewriting (the bounds give the answers
        // through existentials, see `ql_for`); off, the rewriting the closure and the setting
        // call for, whatever came first. A state with violations: whether `owl2-dl` decides
        // them, derived under the mode now set (`ReasoningState::undecided`).
        let treatments = &self.config.dl.treatments;
        self.recertify(&tx, |state| {
            if state.violations > 0 {
                state.undecided = active
                    .then(|| self.dl.undecided(tx.base(), treatments))
                    .flatten();
            }
        });
    }

    /// The `owl2-dl` mode's state.
    pub fn dl(&self) -> &crate::dl::Dl {
        &self.dl
    }
    /// The `owl2-dl` mode's bounds at the latest revision (`crate::dl::bounds`): the
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
        let memory = self.settings.query_envelope(&prepared);
        let (_, ql) = self.run_reporting(
            &prepared,
            &memory,
            &cancellation,
            &mut payload,
            |status, _| {
                completeness = status;
                None
            },
        )?;
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
        self.run_query_reporting(prepared, cancellation, out, |_, _| None)
    }

    /// [`Self::run_query`], handing `report` whether the answers are complete, where a
    /// reasoning path can leave some out (`None` where none can), before any answer is
    /// written: a transport sends it ahead of them. Under `owl2-dl` the bounds' status
    /// ([`crate::dl`]; the answers are then completed before the first byte), else the
    /// OWL 2 QL rewriting's where it applies, else a ruleset closure's
    /// ([`Self::status_without_dl`]). One status, [`nrese_sparql::Completeness`].
    /// A reader of some graphs who sees every inference is told `not-certified` where the
    /// rewriting ran, and any reader of some graphs under `owl2-dl`: either assessment reads
    /// the schema of every graph. With the status, `report` is handed the header text kept
    /// with the answer where the result cache gives it (made by an earlier `report` for a
    /// request told the same status), and returns the text it made for an answer written
    /// now (`None`: none), which the store keeps with the answer where it offers that to
    /// the cache.
    pub fn run_query_reporting(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>, StatusHeader) -> Option<String>,
    ) -> StoreResult<()> {
        let memory = self.settings.query_envelope(prepared);
        self.run_reporting(prepared, &memory, cancellation, out, report)
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
        let memory = self.settings.query_envelope(prepared);
        let (detail, _) = self.run_reporting(prepared, &memory, cancellation, out, |s, _| {
            status = s;
            None
        })?;
        Ok(status.map(|s| (s, detail.unwrap_or_default())))
    }

    /// Every query run but a transaction's pending one, in its envelope `memory`: its refusal
    /// as its reader may be told it ([`for_its_reader`]; the answers are the reader's own).
    fn run_reporting(
        &self,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>, StatusHeader) -> Option<String>,
    ) -> StoreResult<(
        Option<crate::dl::DlDetail>,
        Option<nrese_sparql::ql::QlReport>,
    )> {
        let run = self.run_reporting_whole(prepared, memory, cancellation, out, report);
        let run = run.map_err(|error| refusal_for_its_reader(prepared, error));
        for_its_reader(prepared, crate::EntryPoint::Run, run, |_| {})
    }

    fn run_reporting_whole(
        &self,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>, StatusHeader) -> Option<String>,
    ) -> StoreResult<(
        Option<crate::dl::DlDetail>,
        Option<nrese_sparql::ql::QlReport>,
    )> {
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        // The run's one envelope: every evaluation, retained result and DL task of the run
        // draws from it.
        let memory = memory.clone();
        // One snapshot the route, the answers and their status are all of (T3, N5).
        let snapshot = self.query_view(prepared, &memory, cancellation)?;
        prepared.announce_on(&snapshot);
        if let Some(mode) = self.dl_mode(prepared, &snapshot) {
            use crate::dl::query::{Outcome, Read};
            let detail = match answer(self, &snapshot, prepared, cancellation, &memory, mode)? {
                Outcome::Answers(answers, status, detail) => {
                    report(self.told(prepared, Some(status), false), None);
                    crate::query_executor::write_answers(prepared, answers, cancellation, out)?;
                    detail
                }
                // Streamed over the snapshot the status was decided on (L's view where
                // L adds memberships, kept per revision, whose identity is its own),
                // never a later revision's.
                Outcome::Stream(status, read, detail) => {
                    let snapshot = match read {
                        Read::Lower(snapshot) | Read::At(snapshot) => snapshot,
                    };
                    let context = dl_context(mode, &status);
                    run_query(
                        &snapshot,
                        prepared,
                        &self.settings,
                        cancellation,
                        &memory,
                        out,
                        |_| {
                            let told = self.told(prepared, Some(status), false);
                            (context, move |kept| report(told, kept))
                        },
                    )?;
                    detail
                }
            };
            return Ok((Some(detail), None));
        }
        #[cfg(test)]
        hooks::run(&hooks::AFTER_DECIDED);
        let at = Revision::Committed(&snapshot);
        let status = || self.status_outside_dl(prepared, None, at);
        if prepared.inference_scope.is_none() {
            self.refuse_exact(prepared, &snapshot, status)?;
        }
        // Set where the status is decided, before the answer, a cached one too.
        let mut withheld = true;
        let ql = run_query(
            &snapshot,
            prepared,
            &self.settings,
            cancellation,
            &memory,
            out,
            |mut ql| {
                self.certify_ql(ql.as_deref_mut(), prepared, &snapshot);
                let ql = ql.as_deref();
                let status = self.status_outside_dl(prepared, ql, at);
                // The cache's context is the whole assessment, not what the reader is told.
                let context = context_outside_dl(status.as_ref(), ql.is_some());
                let told = self.told(prepared, status, ql.is_some());
                withheld = withholds_report(prepared, told.as_ref());
                (context, move |kept| report(told, kept))
            },
        )?;
        Ok((None, ql.filter(|_| !withheld)))
    }

    /// The status of answers outside the DL bounds: the QL rewriting's report where it ran,
    /// certified ([`Self::certify_ql`]: what neither it nor the closure answers, the list
    /// mark among it, composed onto it); else [`Self::status_without_dl`], marked for the
    /// data `at` the answers were read from.
    fn status_outside_dl(
        &self,
        prepared: &PreparedQuery,
        ql: Option<&nrese_sparql::ql::QlReport>,
        at: Revision<'_>,
    ) -> Option<nrese_sparql::Completeness> {
        if prepared.inference_scope.is_some() {
            return (!Certificate::bare(at.snapshot(), false))
                .then(nrese_sparql::Completeness::not_certified);
        }
        let route = match ql {
            Some(ql) => Some(ql.completeness.clone()),
            None => self.status_without_dl_at(prepared, at),
        };
        self.assess(prepared, at, route)
    }

    /// The one composition of a read's status (T3), on the snapshot `at` it read, after the
    /// status of its `route` (the closure's, the QL report's or the bounds'), each step only
    /// taking away. A read of a version no one vouched for, or of an unpublished one passed
    /// as committed, is `not-certified`: `None` only where the version carries no
    /// certificate, no reasoner was ever registered on the store (clearing the rules
    /// doesn't count) and the inferred stack is empty. Pending
    /// operations aren't reasoned over before the commit, and may delete what their
    /// inferences rest on. Under `owl2-dl` the question-only
    /// individuals ([`crate::dl::query::mark`]); then the negation rule again, over all that
    /// was taken away.
    pub(crate) fn assess(
        &self,
        prepared: &PreparedQuery,
        at: Revision<'_>,
        route: Option<nrese_sparql::Completeness>,
    ) -> Option<nrese_sparql::Completeness> {
        if prepared.read_model() == ReadModel::Asserted {
            return None;
        }
        let snapshot = at.snapshot();
        let unpublished = matches!(at, Revision::Committed(_)) && snapshot.shared_state().is_none();
        let Some(certificate) = Certificate::of(snapshot).filter(|_| !unpublished) else {
            let bare = Certificate::bare(snapshot, self.supports.has_rules());
            return (!bare).then(nrese_sparql::Completeness::not_certified);
        };
        let mut status = route?;
        let source = if certificate.dl { "dl" } else { "transaction" };
        if let Revision::Pending { deletes, .. } = at {
            // Under `owl2-dl` a pending read has no DL bounds whatever its dataset; it reads the
            // committed closure where it surely reads the default graph.
            let closure = match (
                certificate.dl,
                crate::dl::query::reads_store_closure(prepared),
            ) {
                (false, _) => "",
                (true, true) => CLOSURE,
                (true, false) => NO_BOUNDS,
            };
            status.incomplete(source, format!("{PENDING}{closure}"));
            if deletes {
                status.unsound("transaction", DELETES.to_owned());
            }
        }
        if certificate.dl {
            crate::dl::query::mark(self, prepared, at, &mut status);
        }
        let (reads, decided) = (prepared.reads(), matches!(at, Revision::Decided(_)));
        if matches!(at, Revision::Pending { .. }) {
            crate::dl::query::negation_rule(&mut status, reads, source);
        } else if certificate.dl {
            crate::dl::query::certify(&mut status, reads, "dl", decided, Some(at.snapshot()));
        }
        let rules = if certificate.dl { "dl" } else { "rules" };
        crate::dl::query::withhold(&mut status, reads, rules, (snapshot, certificate), self);
        Some(status)
    }

    /// The status of answers that don't go through the DL bounds, of the committed closure before
    /// the QL rewriting and a transaction's pending operations. `None` only where no reasoning path
    /// can leave answers out: asserted statements alone, or no reasoning. Over a ruleset's closure:
    /// sound, complete only for what it derives, never certain answers under OWL 2 DL. Under
    /// `owl2-dl`, the rules' closure without the bounds, the inferred statements alone or every
    /// statement (`Self::dl_mode`): sound only, unsound where a missing row can make a returned one
    /// wrong (`NOT EXISTS`, …: [`crate::dl::query::certify_closure`]). As `prepared`'s reader is
    /// told it (`status_for_its_reader`), but where reasoning is on a reader by support is told
    /// `not-certified` whatever its read tells: its status is its view's, known only as the read
    /// runs. The read's status is the authority; `not-certified` here says nothing of the data.
    pub fn status_without_dl(
        &self,
        prepared: &PreparedQuery,
    ) -> Option<nrese_sparql::Completeness> {
        let snapshot = self.engine.snapshot();
        let status = self.status_outside_dl(prepared, None, Revision::Committed(&snapshot));
        let reasoned = !Certificate::bare(&snapshot, self.supports.has_rules());
        if reasoned && prepared.access().is_some_and(|a| a.inferred_by_support) {
            return Some(nrese_sparql::Completeness::not_certified());
        }
        self.told(prepared, status, false)
    }

    /// [`status_for_its_reader`] under this store's equality.
    fn told(
        &self,
        prepared: &PreparedQuery,
        status: Option<nrese_sparql::Completeness>,
        rewritten: bool,
    ) -> Option<nrese_sparql::Completeness> {
        status_for_its_reader(prepared, status, rewritten, self.config.equality_compact)
    }

    /// [`Self::status_without_dl`] of a read of the data `at`.
    fn status_without_dl_at(
        &self,
        prepared: &PreparedQuery,
        at: Revision<'_>,
    ) -> Option<nrese_sparql::Completeness> {
        if prepared.read_model() == ReadModel::Asserted {
            return None;
        }
        let certificate = Certificate::of(at.snapshot())?;
        let omission_reason = prepared.dataset_omission_reason();
        if certificate.dl {
            // A read of every statement inside a transaction lacks only the pending
            // operations' reasoning (`Self::assess`); any other read, the DL bounds.
            let mut status = nrese_sparql::Completeness::under(nrese_sparql::Regime::Owl2Dl);
            let pending = matches!(at, Revision::Pending { .. });
            if let Some(reason) = omission_reason {
                status.incomplete("dl", reason.to_owned());
            } else if prepared.read_model() != ReadModel::Materialised || !pending {
                status.incomplete(
                    "dl",
                    "a read of the rules' closure alone, without the DL \
                     bounds; certain answers under OWL 2 DL may be missing"
                        .to_owned(),
                );
            }
            certify_closure(&mut status, prepared.reads(), "dl", Some(at.snapshot()));
            return Some(status);
        }
        let ruleset = certificate.state.ruleset.clone();
        let mut status =
            nrese_sparql::Completeness::under(nrese_sparql::Regime::of_ruleset(&ruleset));
        let reason = match omission_reason {
            Some(reason) => reason.to_owned(),
            None => format!(
                "answers over the {ruleset} closure: what its rules derive, complete for \
                 the ontologies of their profile, which isn't checked per query; not every \
                 certain answer under OWL 2 DL (reasoner.mode = \"owl2-dl\" gives those)"
            ),
        };
        status.incomplete("rules", reason);
        certify_closure(&mut status, prepared.reads(), "rules", Some(at.snapshot()));
        Some(status)
    }

    /// Under `owl2-dl`, the answers a query reading the inferred statements asks for;
    /// `None` for any other query (or mode).
    fn dl_mode(&self, prepared: &PreparedQuery, at: &Snapshot) -> Option<crate::DlAnswers> {
        (prepared.inference_scope.is_none()
            && Certificate::route(at, &self.dl).0
            && prepared.read_model() == ReadModel::Materialised)
            .then(|| prepared.dl_answers().unwrap_or(self.config.dl.answers))
    }

    /// Exact answers asked for under `owl2-dl` on a read past the bounds (the inferred
    /// statements alone, or a transaction's pending state), which isn't certified complete:
    /// refused before it runs by the rule the bounds' path follows
    /// ([`crate::dl::query::refuse_unless_complete`]), with the `status` the read would
    /// carry, built only when exact answers are asked for.
    fn refuse_exact(
        &self,
        prepared: &PreparedQuery,
        at: &Snapshot,
        status: impl FnOnce() -> Option<nrese_sparql::Completeness>,
    ) -> StoreResult<()> {
        let mode = prepared.dl_answers().unwrap_or(self.config.dl.answers);
        if !Certificate::route(at, &self.dl).0 || mode != crate::DlAnswers::Exact {
            return Ok(());
        }
        status().map_or(Ok(()), |status| {
            crate::dl::query::refuse_unless_complete(mode, &status, Vec::new)
        })
    }

    /// What every query gets from the store.
    pub(crate) fn query_settings(&self) -> &crate::query_executor::StoreSettings {
        &self.settings
    }

    pub fn runtime(&self) -> &std::sync::Arc<crate::Runtime> {
        &self.settings.runtime
    }

    /// A request's memory envelope within the catalog's budget: `limit` bytes of its
    /// own, or none: for a read a transport holds memory for without a query of its own (a
    /// query run's is [`Self::open_run`]).
    pub fn envelope(&self, limit: Option<usize>) -> nrese_exec::Envelope {
        self.settings.envelope(limit)
    }

    /// Opens one run of `prepared` on this store, with an envelope of its own: its memory
    /// limit within this catalog's budget, which the run draws on and so does what its
    /// transport holds of its output ([`QueryRun::memory`]).
    pub fn open_run(&self, prepared: &PreparedQuery) -> QueryRun {
        QueryRun {
            store: self.clone(),
            prepared: prepared.clone(),
            memory: self.settings.query_envelope(prepared),
        }
    }

    /// Runs a prepared query on the latest snapshot to completion and reports how it ran:
    /// the executor, each operator with estimated and actual rows, and times. To a reader
    /// who may not read every graph, only what is its own (`for_its_reader`).
    pub fn explain_query(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::Explanation> {
        let memory = self.settings.query_envelope(prepared);
        self.explain_in(prepared, &memory, cancellation)
    }

    /// [`Self::explain_query`] in the envelope `memory`.
    fn explain_in(
        &self,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::Explanation> {
        for_its_reader(
            prepared,
            crate::EntryPoint::Explain,
            self.explain_whole(prepared, memory, cancellation),
            |explanation| {
                explanation.steps.clear();
                explanation.rewrites.clear();
                explanation.decoded = 0;
                certification_for_its_reader(
                    prepared,
                    &mut explanation.completeness,
                    &mut explanation.ql,
                    self.config.equality_compact,
                );
            },
        )
    }

    fn explain_whole(
        &self,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::Explanation> {
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        let memory = memory.clone();
        let snapshot = self.query_view(prepared, &memory, cancellation)?;
        #[cfg(test)]
        hooks::run(&hooks::AFTER_DECIDED);
        let mut explanation =
            explain_prepared(&snapshot, prepared, &self.settings, cancellation, &memory)?;
        self.certify_ql(explanation.ql.as_mut(), prepared, &snapshot);
        if let Some(mode) = self.dl_mode(prepared, &snapshot) {
            // The status as the query would answer, never failing for it.
            let mode = match mode {
                crate::DlAnswers::Exact => crate::DlAnswers::CertainWhereComplete,
                other => other,
            };
            let answered = answer(self, &snapshot, prepared, cancellation, &memory, mode)?;
            let (status, detail) = match answered {
                crate::dl::query::Outcome::Answers(_, status, detail)
                | crate::dl::query::Outcome::Stream(status, _, detail) => (status, detail),
            };
            explanation.completeness = Some(status);
            explanation.decided_by = detail.paths;
            explanation.candidates = Some(nrese_sparql::Candidates {
                proved: detail.proved,
                refuted: detail.refuted,
                refuted_by_labels: detail.refuted_by_labels,
            });
        } else {
            let at = Revision::Committed(&snapshot);
            explanation.completeness =
                self.status_outside_dl(prepared, explanation.ql.as_ref(), at);
        }
        Ok(explanation)
    }

    /// The plan `prepared` would run as, each node with its estimated rows, without
    /// running it (EXPLAIN without ANALYZE), within the query's memory envelope. Planning
    /// stops with [`nrese_sparql::QueryEvaluationError::Cancelled`] once `cancellation` is
    /// cancelled. To a reader who may not read every graph, only what is its own
    /// (`for_its_reader`).
    pub fn plan_query(
        &self,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::PlannedQuery> {
        let memory = self.settings.query_envelope(prepared);
        self.plan_in(prepared, &memory, cancellation)
    }

    /// [`Self::plan_query`] in the envelope `memory`.
    fn plan_in(
        &self,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::PlannedQuery> {
        for_its_reader(
            prepared,
            crate::EntryPoint::Plan,
            self.plan_whole(prepared, memory, cancellation),
            |planned| {
                planned.steps.clear();
                planned.rewrites.clear();
                certification_for_its_reader(
                    prepared,
                    &mut planned.completeness,
                    &mut planned.ql,
                    self.config.equality_compact,
                );
            },
        )
    }

    fn plan_whole(
        &self,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
    ) -> StoreResult<crate::PlannedQuery> {
        let memory = memory.clone();
        let snapshot = self.query_view(prepared, &memory, cancellation)?;
        #[cfg(test)]
        hooks::run(&hooks::AFTER_DECIDED);
        let mut planned =
            plan_prepared(&snapshot, prepared, &self.settings, cancellation, &memory)?;
        self.certify_ql(planned.ql.as_mut(), prepared, &snapshot);
        let at = Revision::Committed(&snapshot);
        planned.completeness = match self.dl_mode(prepared, &snapshot) {
            Some(_) => Some(plan_status(self, &snapshot, prepared, cancellation)?),
            None => self.status_outside_dl(prepared, planned.ql.as_ref(), at),
        };
        Ok(planned)
    }

    /// Bytes the running queries have reserved now, the most reserved at once, and the
    /// limit; `None` without a limit ([`StoreConfig::total_query_memory_bytes`]). A
    /// reservation is what a query's intermediate results hold plus what it took ahead and
    /// hasn't used yet: its credit (at most 64 KiB) and its operators' spare, both bounded
    /// per query and returned when it ends.
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
        let mut printed = nrese_sparql::ql::print(&snapshot, &query, form)?;
        // The status this store gives the same answers ([`Self::status_outside_dl`]): its
        // regime, as the rewriting's run sets it for the RL closure, and its composition, the
        // list mark included.
        if let nrese_sparql::ql::Printed::Query { completeness, .. } = &mut printed {
            completeness.regime = Some(nrese_sparql::Regime::Owl2Rl);
            let reads = crate::dl::query::Reads::default();
            let query = (&query, &reads);
            crate::dl::query::certify_report(Some(completeness), query, Some(&snapshot), self);
        }
        Ok(printed)
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
        self.with_pending_record(pending, scope, memory, cancellation, |view, _| read(view))
    }

    /// [`Self::with_pending`], handing `read` too whether the operations keep their base's
    /// record of what the reader left out (`dl::bounds::Bounds::keeps_record`), kept with a
    /// session's view as the view is.
    fn with_pending_record<T>(
        &self,
        pending: &crate::StatementsRequest,
        scope: &crate::ReadScope,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
        read: impl FnOnce(&nrese_engine::Snapshot, bool) -> StoreResult<T>,
    ) -> StoreResult<T> {
        // The rules the view is decided under, read before it is made: one made under later
        // rules is kept as of these, never the reverse.
        let epoch = self.supports.epoch();
        if let Some((view, keeps_record)) =
            self.sessions
                .view(&self.engine.snapshot(), scope, pending, epoch)
        {
            return read(&view, keeps_record);
        }
        // Never committed: no writer slot, so other clients commit meanwhile. The operations
        // read as the reader may; what they change is checked at the commit (the view is
        // the reader's own, and shows the graphs it may read only). A private transaction
        // (dictionary lifecycle V1): the operations' own terms and pending changes are charged
        // to the reading query's envelope, and none enters the dictionary; the commit replays
        // the operations, never these ids. One budget for the request, so its pending snapshot
        // is made once (`pending_snapshot_within`'s memo).
        let budget = std::sync::Arc::new(memory.part());
        let mut tx = self.engine.speculative(std::sync::Arc::clone(&budget));
        let context = self.update_context(memory.clone(), cancellation);
        let requester = crate::Requester::new(scope.clone(), crate::WriteScope::All);
        crate::statements::apply_statements(
            &mut tx,
            pending,
            &requester,
            &budget,
            &mut |tx, request| {
                crate::mutation::command::apply_sparql_update(tx, request, &requester, &context)
            },
        )
        .map_err(crate::StoreError::refusal_within)?;
        self.rebuild_pending_declarations(&mut tx, &budget, cancellation)?;
        let keeps_record = self.dl().bounds.keeps_record(&tx);
        let view = match self.reading(scope.access(), memory, Some(cancellation)) {
            Some(reading) => reading.pending(&tx, &budget)?,
            // Operations that touch no input of the list scan (a list fact; under compact
            // equality, any) leave out the lists their base does (a reader's own view, above,
            // may drop equality's copies of list cells: it scans).
            None => {
                use crate::reasoning::Program;
                let view = tx.pending_snapshot_within(&budget)?;
                if !Program::touches_a_list(&view, tx.inserted().chain(tx.deleted())) {
                    self.lists.own(&view, || false);
                }
                view
            }
        };
        self.sessions.keep_view(
            tx.base().clone(),
            scope,
            pending,
            (view.clone(), keeps_record),
            epoch,
            self.supports.epoch(),
        );
        read(&view, keeps_record)
    }

    /// A pending change that changes the set of annotation properties the rules read
    /// rebuilds `tx`'s inferred stack over its pending state, as its commit does. A read
    /// inside the transaction answers by that set. Other pending changes read the committed
    /// inferred stack until commit.
    fn rebuild_pending_declarations(
        &self,
        tx: &mut nrese_engine::PrivateTransaction<'_>,
        budget: &std::sync::Arc<nrese_exec::Budget>,
        cancellation: &CancellationToken,
    ) -> StoreResult<()> {
        use crate::reasoning::Program;
        let Some(rules) = self.supports.rules() else {
            return Ok(());
        };
        let changes = tx.inserted().chain(tx.deleted());
        if !Program::touches_a_declaration(&|term| tx.lookup(term), changes) {
            return Ok(());
        }
        let pending = tx.pending_snapshot_within(budget)?;
        let program = self.bound_program(&pending, rules)?;
        if !program.annotation_set_changed(tx.base(), &pending) {
            return Ok(());
        }
        let program = (program.by_representatives(self.config.equality_by_representatives))
            .storing_representatives(self.config.equality_compact);
        let memory = self.memory_stop();
        let stop = || cancellation.is_cancelled() || memory.exceeded();
        let room = || memory.room();
        let operation = nrese_reasoner::eval::OperationStop::new(&stop).within(&room);
        let cancelled = || nrese_sparql::QueryEvaluationError::Cancelled.into();
        let mut closure =
            crate::reasoning::materialise_with(&program, &pending, &operation, budget)
                .map_err(|interrupted| memory.error(interrupted).unwrap_or_else(cancelled))?;
        nrese_reasoner::engine::install_closure(&pending, &mut closure.inferred, |fact, add| {
            match add {
                true => tx.insert_inferred(fact),
                false => tx.remove_inferred(fact),
            }
        });
        Ok(())
    }

    /// [`run_query`](Self::run_query) on the data as `pending` would leave it.
    pub fn run_query_pending(
        &self,
        pending: &crate::StatementsRequest,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
    ) -> StoreResult<()> {
        self.run_query_pending_reporting(pending, prepared, cancellation, out, |_, _| None)
    }

    /// [`Self::run_query_pending`], handing `report` the answers' status as
    /// [`Self::run_query_reporting`] does. A transaction reads the latest revision: with
    /// no operations pending, exactly as a query outside it (under `owl2-dl`, through that
    /// revision's bounds). Pending operations aren't reasoned over until the commit (but for
    /// a change of the declarations the rules read, `Self::rebuild_pending_declarations`):
    /// the status says so, sound only, and not sound where they may delete statements whose
    /// inferences remain.
    pub fn run_query_pending_reporting(
        &self,
        pending: &crate::StatementsRequest,
        prepared: &PreparedQuery,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>, StatusHeader) -> Option<String>,
    ) -> StoreResult<()> {
        let memory = self.settings.query_envelope(prepared);
        self.run_pending_reporting(pending, prepared, &memory, cancellation, out, report)
    }

    /// [`Self::run_query_pending_reporting`] in the envelope `memory`.
    fn run_pending_reporting(
        &self,
        pending: &crate::StatementsRequest,
        prepared: &PreparedQuery,
        memory: &nrese_exec::Envelope,
        cancellation: &CancellationToken,
        out: impl std::io::Write,
        report: impl FnOnce(Option<nrese_sparql::Completeness>, StatusHeader) -> Option<String>,
    ) -> StoreResult<()> {
        if pending.ops.is_empty() {
            return self
                .run_reporting(prepared, memory, cancellation, out, report)
                .map(|_| ());
        }
        if prepared.inference_scope.is_some() {
            return Err(nrese_sparql::QueryEvaluationError::Argument(
                "inference-scope=dataset is unavailable with operations pending".into(),
            )
            .into());
        }
        let _running = self
            .running
            .register(prepared.text(), prepared.origin(), cancellation);
        let scope = crate::ReadScope::of(prepared.access().cloned());
        let deletes = !pending.ops.iter().all(adds_only);
        let memory = memory.clone();
        // Announced on the committed view, as outside a transaction: what the transaction
        // asserts itself isn't (a refused view is told as the run's refusal is).
        let latest = &self.engine.snapshot();
        let committed = self.view_of(latest, prepared.access(), &memory, Some(cancellation));
        if let Err(error) = committed.map(|view| prepared.announce_on(&view)) {
            let run = Err(refusal_for_its_reader(prepared, error));
            return for_its_reader(prepared, crate::EntryPoint::RunPending, run, |_| {});
        }
        let run = self.with_pending_record(
            pending,
            &scope,
            &memory,
            cancellation,
            |snapshot, keeps_record| {
                // The status the answers carry, of the pending state `at` they are read from.
                // Under `owl2-dl` the QL rewriting doesn't run: the status needs no report.
                let at = Revision::Pending {
                    snapshot,
                    deletes,
                    keeps_record,
                };
                let status = || self.status_outside_dl(prepared, None, at);
                self.refuse_exact(prepared, snapshot, status)?;
                run_query(
                    snapshot,
                    prepared,
                    &self.settings,
                    cancellation,
                    &memory,
                    out,
                    |mut ql| {
                        // A transaction's pending state: never answered from the cache.
                        self.certify_ql(ql.as_deref_mut(), prepared, snapshot);
                        let ql = ql.as_deref();
                        let status = self.status_outside_dl(prepared, ql, at);
                        report(self.told(prepared, status, ql.is_some()), None);
                        (Vec::new(), |_| None)
                    },
                )
                .map(|_| ())
            },
        );
        let run = run.map_err(|error| refusal_for_its_reader(prepared, error));
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
        // A group a graph: as many as the store has graphs, bounded by them, not by a
        // request (uncharged as before; the follow-up is D16-039 F7).
        let unlimited = nrese_exec::Budget::unlimited();
        let counts: Vec<(TermId, u64)> = snapshot
            .group_counts_in(
                ReadModel::Asserted,
                &QuadPattern::all(),
                nrese_engine::quad::Permutation::Gspo,
                &unlimited,
            )
            .expect("an unlimited budget")
            .map(|groups| groups.into_parts().0)
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
                &self.read_memory(context),
                &context.cancel,
                read,
            ),
        };
        context.scope.told(entry, whole)
    }

    /// Writes an image backup of the latest snapshot into `dir` ([`crate::image_backup`]).
    pub fn backup_image(&self, dir: &std::path::Path) -> StoreResult<crate::ImageManifest> {
        crate::image_backup::backup_image(&self.engine, dir, false)
    }

    /// A read replica's start: [`Self::backup_image`], refused
    /// ([`crate::StoreError::CompactPrimary`]) where the version written is stored over
    /// representatives, which the image doesn't say.
    pub fn replication_image(&self, dir: &std::path::Path) -> StoreResult<crate::ImageManifest> {
        crate::image_backup::backup_image(&self.engine, dir, true)
    }

    /// A read replica's feed: the committed records after revision `after`, at most about
    /// `max_bytes` ([`nrese_engine::Engine::log_since`]). An error for a log that no
    /// longer holds them (`EngineError::LogTruncated`): the replica starts from an image.
    /// Refused ([`crate::StoreError::CompactPrimary`]) where the version served is stored
    /// over representatives: the records don't say so, and a replica would read them as
    /// they are. Where the log no longer holds them, the version served now answers: a
    /// replica started again from an image would be refused that image too.
    pub fn replication_log(
        &self,
        after: u64,
        max_bytes: usize,
    ) -> StoreResult<nrese_engine::LogBatch> {
        self.engine
            .log_since(after, max_bytes)
            .map_err(|error| match (error, self.engine.equality()) {
                (EngineError::LogTruncated { .. }, Some(_)) => crate::StoreError::CompactPrimary,
                (error, _) => crate::StoreError::Engine(error),
            })
            .and_then(|batch| match batch.equality {
                Some(_) => Err(crate::StoreError::CompactPrimary),
                None => Ok(batch),
            })
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
        stop: &StopSource,
    ) -> StoreResult<nrese_engine::Applied> {
        // A marker left from a start as a primary would vouch, after a restart, for the
        // records applied here (nothing reasons on a replica to write one again).
        if self.marker.load(std::sync::atomic::Ordering::Acquire) {
            self.invalidate_reasoning()?;
        }
        let before = self.current_revision();
        let applied = self.engine.apply_log(frames, expected, stop);
        #[cfg(test)]
        hooks::run(&hooks::AFTER_WRITE_PUBLICATION);
        let view = self.engine.snapshot();
        // Whatever was published, a whole answer or the records before a stop or a
        // failure, the support sets start afresh.
        if view.revision() != before {
            self.supports().forget();
        }
        self.refresh_kept_shapes(&view, stop);
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
        let withheld =
            self.invalidate_held(tx.writer().expect("a transaction holds the writer slot"))?;
        let cancellation = CancellationToken::new();
        let context = self.update_context(self.envelope(None), &cancellation);
        let report = command.apply(&mut tx, requester, &context)?;
        if let Some(withheld) = withheld {
            tx.vouch(withheld);
        }
        let summary = tx.commit()?;
        #[cfg(test)]
        hooks::run(&hooks::AFTER_WRITE_PUBLICATION);
        self.refresh_kept_shapes(&self.engine.snapshot(), &StopSource::none());
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
        let invalidate = |writer: nrese_engine::WriterHeld<'_>| self.invalidate_held(writer);
        let loaded = bulk_load(
            &self.engine,
            request,
            progress,
            invalid,
            &self.quarantine,
            &invalidate,
        );
        #[cfg(test)]
        hooks::run(&hooks::AFTER_WRITE_PUBLICATION);
        // A quarantine error can follow a publication. Count the shapes that are served.
        self.refresh_kept_shapes(&self.engine.snapshot(), &progress.stop());
        loaded
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
        let snapshot = self.engine.snapshot();
        Self::reasoning_state_at(&snapshot).is_some_and(|state| {
            state.is_current_with(
                &program,
                self.config.hide_unnamed_classes,
                self.config.equality_compact,
                || crate::reasoning_state::nil_equated(&snapshot),
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
        // Admitted as every read is ([`Self::view_of`]) before anything of the snapshot is
        // read: a proof under rules nothing vouches its stack for would show a reader of
        // some graphs what a premise it can't read decides.
        let envelope = self.settings.envelope(None);
        self.view_of(&snapshot, scope.access(), &envelope, None)?;
        let program = self.bound_program(&snapshot, program)?;
        let Some(fact) = Self::fact_of(&snapshot, statement) else {
            return Ok(None);
        };
        let explain = |readable| {
            self.proof_within(|stop| {
                nrese_reasoner::engine::explain_fact_until(
                    &program, &snapshot, fact, readable, stop,
                )
            })
        };
        let crate::ReadScope::Graphs(access) = scope else {
            return explain(None);
        };
        let readable = |fact: [u64; 3]| {
            crate::support::asserted(&snapshot, access.as_ref(), fact)
                == crate::support::Asserted::InReach
        };
        let Some(steps) = explain(Some(&readable))? else {
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
        // Admitted before anything of it is read, as [`Self::explain_statement_whole`].
        let envelope = self.settings.envelope(None);
        self.view_of(&snapshot, scope.access(), &envelope, None)?;
        let program = self.bound_program(&snapshot, program)?;
        let Some(fact) = Self::fact_of(&snapshot, statement) else {
            return Ok(None);
        };
        let justify = |readable| {
            self.proof_within(|stop| {
                nrese_reasoner::engine::justify_fact_until(
                    &program, &snapshot, fact, mode, readable, stop,
                )
            })
        };
        let crate::ReadScope::Graphs(access) = scope else {
            return justify(None);
        };
        let readable = |fact: [u64; 3]| {
            crate::support::asserted(&snapshot, access.as_ref(), fact)
                == crate::support::Asserted::InReach
        };
        let visible = readable(fact) || self.inferred_visible(&snapshot, access, fact)?;
        if !visible {
            return Ok(None);
        }
        justify(Some(&readable))
    }

    fn proof_within<T>(
        &self,
        read: impl FnOnce(nrese_reasoner::eval::Stop<'_>) -> Option<T>,
    ) -> StoreResult<Option<T>> {
        let memory = self.memory_stop();
        let answer = read(&|| memory.exceeded());
        memory.error(Default::default()).map_or(Ok(answer), Err)
    }

    /// `program` with its constants by the snapshot's ids, without the writer: a constant
    /// the store doesn't hold gets an id no statement uses (its rules can't fire); or the
    /// refusal of a process past its memory limit ([`Self::interruption_refusal`]).
    fn bound_program(
        &self,
        snapshot: &nrese_engine::Snapshot,
        program: nrese_reasoner::RuleProgram,
    ) -> StoreResult<crate::reasoning::Program> {
        let unknown = std::cell::Cell::new(u64::MAX >> 1);
        crate::reasoning::Program::compile(&program, &|term| {
            snapshot.lookup(term).unwrap_or_else(|| {
                unknown.set(unknown.get() - 1);
                nrese_engine::TermId::from_raw(unknown.get())
            })
        })
        .hiding_unnamed_classes(self.config.hide_unnamed_classes)
        .map_err(|interrupted| self.interruption_refusal(interrupted))
    }

    /// A revision no commit here recorded (a reopened store's, a replica's) is scanned. Only
    /// a scan of the published version writes the record: a pending or a reader's own view
    /// of it takes the latest version's answer at its revision, else the record's, else left
    /// out, with a scan of its own statements.
    pub(crate) fn lists_left_out(&self, snapshot: &nrese_engine::Snapshot) -> bool {
        use crate::reasoning::Program;
        let latest = self.engine.snapshot();
        let base = match latest.revision() == snapshot.revision() {
            true => {
                (self.lists).left_out(latest.revision(), || Program::leaves_out_a_list(&latest))
            }
            false => self.lists.known(snapshot.revision()).unwrap_or(true),
        };
        base || (!latest.same_version(snapshot)
            && (self.lists).own(snapshot, || Program::leaves_out_a_list(snapshot)))
    }

    /// The QL rewriting's report of `prepared`'s read of `snapshot`, certified
    /// ([`crate::dl::query::certify_report`]) with this store's record of the lists its
    /// closure leaves out.
    fn certify_ql(
        &self,
        ql: Option<&mut nrese_sparql::ql::QlReport>,
        prepared: &PreparedQuery,
        snapshot: &nrese_engine::Snapshot,
    ) {
        let completeness = ql.map(|r| &mut r.completeness);
        crate::dl::query::certify_report(completeness, prepared.reads(), Some(snapshot), self);
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
    /// The limit is found here, at the operation's start ([`Self::process_memory_bytes`]);
    /// where it can't be, the watch reads as unread: the operation has no room and stops
    /// ([`nrese_exec::memory::MemoryStopCause::Unread`]).
    pub(crate) fn memory_stop(&self) -> MemoryStop {
        MemoryStop::of(self.process_memory_bytes())
    }

    /// The refusal of an operation the process's memory `interrupted` before it reasoned (a
    /// program's compilation, [`crate::reasoning::Program::hiding_unnamed_classes`]), as
    /// [`MemoryStop::error`] names it; nothing of the operation is applied.
    pub(crate) fn interruption_refusal(
        &self,
        interrupted: nrese_reasoner::delta::Interrupted,
    ) -> crate::StoreError {
        let limit = nrese_exec::memory::process_limit().unwrap_or(0);
        let refusal = self.memory_stop().error(interrupted);
        refusal.unwrap_or(crate::StoreError::ProcessMemoryLimit { limit })
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
    /// change of rules; commits keep it current afterwards. Register `program` as well
    /// ([`Self::use_reasoning_rules`]; the mutation pipeline registers its own): while the
    /// rules in place aren't the ones the stack was made with, the new stack isn't certified
    /// (`Self::vouches`) and readers of some graphs who see inferred statements are refused
    /// until the store is rematerialised under the rules in place. A store used
    /// as a library that rematerialises with no reasoner registered (no pipeline) reads a
    /// later version without inferred statements or certificate as one no reasoning can
    /// leave answers out of: no status, not `not-certified`; a server registers its rules
    /// first.
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
                .map_err(|interrupted| self.interruption_refusal(interrupted))?
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
        // Under `owl2-dl`, whether it decides the violations (`ReasoningState::undecided`).
        let undecided = (violations > 0 && self.dl.active())
            .then(|| self.dl.undecided(base, &self.config.dl.treatments))
            .flatten();
        // The last word under the writer slot: a caller whose claim ended meanwhile (a
        // retired pipeline, a cancelled request) changes nothing.
        if stop() {
            return Err(stopped(nrese_reasoner::delta::Interrupted::default()));
        }
        // The marker of the closure about to be replaced goes first, durably: a stop or a
        // failed marker write between the publication and the new marker must not leave the
        // old one describing the new closure after a start.
        self.forget_marker(rematerialisation.writer())?;
        // Where asked, what the reasoner freed goes back before the inferred stack, the
        // heap's peak, is built (as between the reasoner's rounds).
        nrese_engine::memory::release_between_rounds();
        nrese_exec::heap::phase("store: inferred stack");
        #[cfg(test)]
        hooks::run(&hooks::BEFORE_PUBLICATION);
        // Published with the writer slot still held: certified before any other write. The
        // new stack comes with its reading, as its program keeps it (expanded by the id it is
        // kept over, or as stored), in one install: until then readers read the old stack in
        // its own reading, uncertified (`invalidate_held` above), and with its certificate. A
        // publication that fails installs nothing, so there is nothing to put back.
        let state = crate::ReasoningState {
            undecided,
            ..crate::ReasoningState::of_with(
                rules,
                self.config.hide_unnamed_classes,
                self.config.equality_compact,
                violations,
            )
        };
        let literal = match closure.literal_equality {
            true => Literal::Set,
            false => Literal::Clear,
        };
        // The whole closure assessed: the hold set by a clash, cleared by none; the rules' word
        // where the rules registered (or, with none, this rebuild's) made it.
        let hide = self.config.hide_unnamed_classes;
        let checked = self
            .supports
            .rules()
            .is_none_or(|p| state.made_by(&p, hide));
        let facts = Facts {
            literal,
            held: violations > 0,
            assessed: true,
            checked,
            rebuilt_here: true,
        };
        let reading = program.stored_equality();
        let record = self.certificate(state.clone(), None, facts);
        let finished = rematerialisation.finish_held(closure.inferred, reading, Some(record))?;
        let summary = finished.summary();
        nrese_exec::heap::phase("store: done");
        nrese_engine::memory::release_all();
        #[cfg(test)]
        hooks::run(&hooks::AFTER_PUBLICATION);
        // From here the new inferences are published: a failure says so.
        self.record_reasoning(finished.writer(), &state)
            .map_err(|error| crate::StoreError::InferencesNotRecorded(Box::new(error)))?;
        self.engine.notify_publication();
        drop(finished);
        if let Some(why) = undecided {
            let text = why.text(violations);
            tracing::error!(ruleset = %rules.name(), violations_at_least, "{text}; the store is in quarantine until it is repaired");
        } else if violations > 0 {
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

/// The status of its answers that `prepared`'s reader is told. A
/// reader of some graphs who sees every inference (`inferred = "visible"`) is told
/// `not-certified` wherever the OWL 2 QL rewriting ran: its assessment reads the schema of
/// every graph. Whether the rewriting runs depends on the query and the configuration, not
/// on the schema, so the token tells nothing of hidden data. So is every reader of some
/// graphs told it for a status under `owl2-dl`: whether OWL 2 DL reads the data
/// otherwise than the rules is a question of every graph's axioms
/// ([`crate::dl::query::mark`]), and the regime is the configuration's. Under compact
/// equality (`compact`) that reader reads the publication, whose closure isn't its own: it
/// is told `not-certified` for every status, the data and its assessment the publication's.
fn status_for_its_reader(
    prepared: &PreparedQuery,
    status: Option<nrese_sparql::Completeness>,
    rewritten: bool,
    compact: bool,
) -> Option<nrese_sparql::Completeness> {
    let dl = status.as_ref().and_then(|s| s.regime) == Some(nrese_sparql::Regime::Owl2Dl);
    let withheld = rewritten || (compact && status.is_some());
    match (withheld && withholds_certification(prepared)) || (dl && prepared.access().is_some()) {
        true => Some(nrese_sparql::Completeness::not_certified()),
        false => status,
    }
}

/// An exact answer's refusal as its reader may be told it: its status through
/// [`status_for_its_reader`], its unresolved candidates only where that is the status.
fn refusal_for_its_reader(prepared: &PreparedQuery, error: crate::StoreError) -> crate::StoreError {
    let crate::StoreError::Incomplete(mut answer) = error else {
        return error;
    };
    // An exact answer is refused under `owl2-dl` alone: told as every `owl2-dl` status is.
    let told = status_for_its_reader(prepared, Some(answer.status.clone()), false, false);
    if let Some(told) = told.filter(|told| *told != answer.status) {
        answer.status = told;
        answer.unresolved.clear();
    }
    crate::StoreError::Incomplete(answer)
}

/// [`status_for_its_reader`] for an answer that carries the rewriting's report too, as an
/// EXPLAIN does: the status as its reader is told it, and no report where that is withheld.
fn certification_for_its_reader(
    prepared: &PreparedQuery,
    completeness: &mut Option<nrese_sparql::Completeness>,
    ql: &mut Option<nrese_sparql::ql::QlReport>,
    compact: bool,
) {
    *completeness = status_for_its_reader(prepared, completeness.take(), ql.is_some(), compact);
    if withholds_report(prepared, completeness.as_ref()) {
        *ql = None;
    }
}

/// Whether `prepared`'s reader, told `told`, is shown no report of the rewriting: where its
/// assessment is withheld ([`withholds_certification`]), and where its status is. Rows a
/// withheld view's rewriting gives stand; its report, a claim, doesn't.
fn withholds_report(prepared: &PreparedQuery, told: Option<&nrese_sparql::Completeness>) -> bool {
    withholds_certification(prepared) || told.is_some_and(|c| c.certification_withheld)
}

/// Whether `prepared`'s reader is told no assessment of the rewriting, neither its status
/// nor its report ([`status_for_its_reader`]).
fn withholds_certification(prepared: &PreparedQuery) -> bool {
    prepared
        .access()
        .is_some_and(|access| access.inferred && !access.inferred_by_support)
}

/// Whether `op` only adds statements: an Add, or an update of `INSERT DATA` alone as it
/// parses without the store's prefixes (one that needs them counts as one that may delete).
fn adds_only(op: &crate::StatementOp) -> bool {
    use nrese_sparql_syntax::GraphUpdateOperation as Op;
    let update = match op {
        crate::StatementOp::Add { .. } => return true,
        crate::StatementOp::Update(update) => &update.update,
        _ => return false,
    };
    let insert = |op: &Op| matches!(op, Op::InsertData { .. });
    let parsed = nrese_sparql_syntax::SparqlParser::new().parse_update(update);
    parsed.is_ok_and(|u| u.operations.iter().all(insert))
}

/// The result cache's context of an answer under `owl2-dl` in `mode`: a tag no other
/// context starts with, the mode, then the status, every field of it.
fn dl_context(mode: crate::DlAnswers, status: &nrese_sparql::Completeness) -> Vec<u8> {
    use crate::DlAnswers::{CertainWhereComplete, Exact, Sound};
    let mode = match mode {
        CertainWhereComplete => 0,
        Sound => 1,
        Exact => 2,
    };
    let mut context = vec![b'D', mode];
    status.write_cache_context(&mut context);
    context
}

/// The result cache's context of an answer outside the DL bounds: its whole status, or
/// none, each with a tag of its own, after a byte for whether the QL rewriting ran (what
/// a reader is told depends on it: [`status_for_its_reader`]).
fn context_outside_dl(status: Option<&nrese_sparql::Completeness>, rewritten: bool) -> Vec<u8> {
    let Some(status) = status else {
        return vec![u8::from(rewritten), b'N'];
    };
    let mut context = vec![u8::from(rewritten), b'S'];
    status.write_cache_context(&mut context);
    context
}

/// A run or an EXPLAIN of `prepared` as its reader may be told it. A
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
    if !scope.reads_everything() || prepared.inference_scope.is_some() {
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
        /// A bulk load holds the writer slot and is about to read its files.
        pub(crate) static BEFORE_LOAD_READS: Hook = const { RefCell::new(None) };
        /// A bulk load has cleared the reasoning certificate and is about to publish.
        pub(crate) static AFTER_INVALIDATION: Hook = const { RefCell::new(None) };
        /// A rematerialisation has dropped the old certificate and computed its inferences, and
        /// is about to publish them.
        pub(crate) static BEFORE_PUBLICATION: Hook = const { RefCell::new(None) };
        /// A rematerialisation has published its inferences and is about to certify them.
        pub(crate) static AFTER_PUBLICATION: Hook = const { RefCell::new(None) };
        /// A write has published and is about to return or refresh its shapes count.
        pub(crate) static AFTER_WRITE_PUBLICATION: Hook = const { RefCell::new(None) };
        /// A change of the QL rewriting setting has read the certificate and is about to
        /// install the rewriting it calls for.
        pub(crate) static AFTER_QL_READ: Hook = const { RefCell::new(None) };
        /// A pipeline write has passed its preflight certificate check and is about to take
        /// its transaction.
        pub(crate) static AFTER_PREFLIGHT: Hook = const { RefCell::new(None) };
        /// A pipeline write's commit has failed, its writer slot released, and the write is
        /// about to return its error.
        pub(crate) static AFTER_FAILED_COMMIT: Hook = const { RefCell::new(None) };
        /// An entailment check has its answer, its snapshot let go, and is about to validate it.
        pub(crate) static AFTER_ENTAILMENT_READS: Hook = const { RefCell::new(None) };
        /// An entailment check has read its mode and is about to read the closure.
        pub(crate) static AFTER_MODE_READ: Hook = const { RefCell::new(None) };
        /// An entailment check has found the certificate current and is about to read the
        /// closure.
        pub(crate) static AFTER_CERTIFIED: Hook = const { RefCell::new(None) };
        /// An entailment check has read the closure for the positive statements and is about
        /// to refute the negative ones.
        pub(crate) static BETWEEN_ENTAILMENT_HALVES: Hook = const { RefCell::new(None) };
        /// An `owl2-dl` entailment check has checked the premise and is about to test the
        /// conclusion.
        pub(crate) static AFTER_DL_PREMISE: Hook = const { RefCell::new(None) };
        /// Invalidation is about to remove the marker. The old record is still published.
        pub(crate) static INSIDE_INVALIDATION: Hook = const { RefCell::new(None) };
        /// What a read reads is fixed and its status is still to come: an `owl2-dl` query has
        /// decided its answers, or another read has taken its snapshot.
        pub(crate) static AFTER_DECIDED: Hook = const { RefCell::new(None) };
        /// An `owl2-dl` query told `complete` is about to pose U1's element for every element.
        pub(crate) static BEFORE_STAND_IN: Hook = const { RefCell::new(None) };
        /// A clear of the inferred stack has invalidated the certificate and is about to
        /// publish the empty stack.
        pub(crate) static BEFORE_CLEAR_PUBLICATION: Hook = const { RefCell::new(None) };
        /// A reasoning commit in quarantine has published and is about to revalidate.
        pub(crate) static BEFORE_REVALIDATION: Hook = const { RefCell::new(None) };
        /// A registration of rules has registered them and is about to certify under them.
        pub(crate) static AFTER_RULES_REGISTERED: Hook = const { RefCell::new(None) };
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

    // An ungated write publishes its statements with the existing hold and no rules verdict.
    #[test]
    fn ungated_publication_preserves_quarantine() {
        use super::certificate_lifecycle_tests::certificate_record_tests::quarantined;
        use crate::{ConsistencyStatus, SparqlQueryRequest};

        const EX: &str = "PREFIX ex: <http://example.org/>";
        let (store, _) = quarantined(StoreConfig::in_memory());
        let before = store.current_revision();
        store
            .update_str_ungated(&format!("{EX} INSERT DATA {{ ex:s ex:p ex:o }}"))
            .expect("setup: ungated update committed");
        assert!(
            store.current_revision() > before,
            "setup: new revision published"
        );
        assert!(store.is_quarantined(), "readiness after ungated write");
        assert_eq!(store.consistency(), ConsistencyStatus::Unknown);
        let ask = SparqlQueryRequest::all(format!("{EX} ASK {{ ex:s ex:p ex:o }}"));
        let result = store.execute_query(&ask).expect("query answered");
        let json: serde_json::Value = serde_json::from_slice(&result.payload).expect("ASK result");
        assert_eq!(json["boolean"], true);
    }

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

    /// C1: a rematerialisation between a bulk load's clearing of the certificate and its
    /// publication.
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

    /// A bulk load clears the certificate before it publishes, never after, and a load whose
    /// clearing fails (the marker can't be removed) changes nothing: it publishes nothing, and
    /// the certificate stays whole, the QL rewriting it switches on included, so every answer
    /// stays, in the process and after a reopen. Published first, the load's data would be
    /// served and on disk under the old marker; the rewriting dropped first, the existential's
    /// answer would go while the marker still certifies the data.
    #[test]
    fn a_load_that_cannot_clear_the_certificate_publishes_nothing() {
        const EX: &str = "http://example.com/";
        let dir = tempfile::tempdir().unwrap();
        // `:a :p ?o` holds only through the rewriting of the existential, which the
        // certificate switches on.
        let ontology = dir.path().join("ontology.ttl");
        std::fs::write(
            &ontology,
            format!(
                "@prefix : <{EX}> .\n@prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
                 @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                 :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; \
                 owl:someValuesFrom owl:Thing ] .\n:a a :A .\n"
            ),
        )
        .unwrap();
        let more = dir.path().join("more.nt");
        std::fs::write(&more, format!("<{EX}b> <{EX}q> <{EX}c> .\n")).unwrap();
        let store_dir = dir.path().join("store");
        let config = || StoreConfig {
            ql_rewriting: crate::QlRewritingMode::On,
            ..StoreConfig::on_disk(&store_dir)
        };
        let marker = || std::fs::read(store_dir.join(super::REASONING_MARKER)).ok();
        let ask = |store: &StoreService, query: &str| {
            let answer = store.execute_query_str(query).unwrap();
            String::from_utf8_lossy(&answer.payload).contains("true")
        };
        let store = StoreService::new(config()).unwrap();
        load(&store, &ontology);
        store
            .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Ql)
            .unwrap();
        let existential = format!("ASK {{ <{EX}a> <{EX}p> ?o }}");
        assert!(ask(&store, &existential), "the rewriting answers it");
        let recorded = |store: &StoreService| store.reasoning_state().map(|s| s.to_text());
        let state = recorded(&store);
        let saved = marker();
        let consistency = store.consistency();
        assert!(state.is_some() && saved.is_some(), "setup: certified");
        crate::metadata::tests::FAIL_REMOVE.set(true);
        let failed = store.bulk_load(&crate::BulkLoadRequest {
            files: vec![more],
            replace: false,
            graph: crate::GraphTarget::DefaultGraph,
            invalid: None,
        });
        crate::metadata::tests::FAIL_REMOVE.set(false);
        let mut wrong = Vec::new();
        if failed.is_ok() {
            wrong.push("the load succeeded without clearing the certificate".to_owned());
        }
        let mut unchanged = |store: &StoreService, when: &str, verdict| {
            let seen = [
                (ask(store, &existential), "the existential's answer"),
                (
                    !ask(store, &format!("ASK {{ <{EX}b> <{EX}q> <{EX}c> }}")),
                    "nothing published",
                ),
                (
                    recorded(store) == state && store.consistency() == verdict && marker() == saved,
                    "the certificate and its marker",
                ),
            ];
            for (held, what) in seen {
                if !held {
                    wrong.push(format!("{when}: {what} changed"));
                }
            }
        };
        unchanged(&store, "in process", consistency);
        drop(store);
        let store = StoreService::new(config()).unwrap();
        unchanged(&store, "after a reopen", crate::ConsistencyStatus::Unknown);
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The QL schema is part of the certificate: a bulk load between a QL
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
        let current = store.engine().snapshot();
        let rewriting = super::Certificate::of(&current).is_some_and(|c| c.ql.is_some());
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

    /// A reasoning write leaves the closure of the data it commits over: a bulk
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

    /// An entailment answers from one certified closure (the read side): a bulk
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

    /// An entailment that starts inside an invalidation answers from a certified closure:
    /// a bulk replace with the same statements has
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

    /// An entailment started during invalidation still answers the published RDFS closure.
    #[test]
    fn an_entailment_takes_the_published_record_during_invalidation() {
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
            assert_eq!(
                racer.entails(rdfs(), &during).unwrap(),
                crate::Entailment::Entailed
            );
        })));
        store.invalidate_reasoning().unwrap();
        store.rematerialise(rdfs()).unwrap();
        assert_eq!(
            store.entails(rdfs(), &conclusion).unwrap(),
            crate::Entailment::Entailed
        );
    }

    /// A positive entailment reads its pinned closure across another rebuild at the same revision.
    #[test]
    fn an_entailment_keeps_its_closure_across_a_same_revision_rebuild() {
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
        assert_eq!(
            store.entails(rdfs(), &conclusion).unwrap(),
            crate::Entailment::Entailed
        );
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

    /// A replacement closure never reopens under the certificate of the one it replaced:
    /// over an OWL 2 RL closure and certificate, an RDFS rematerialisation
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

    /// The marker alone cannot rule out an equality. The registered RDFS program
    /// can, once the store checks that its fingerprint matches the closure.
    #[test]
    fn an_rdfs_marker_needs_its_own_program_to_clear_reopen_uncertainty() {
        use nrese_reasoner::{RuleProgram, rulesets::Ruleset};

        let dir = tempfile::tempdir().unwrap();
        let config = || StoreConfig::on_disk(dir.path());
        let program = RuleProgram::builtin(Ruleset::Rdfs);
        {
            let store = StoreService::new(config()).unwrap();
            store.rematerialise(&program).unwrap();
        }
        let store = StoreService::new(config()).unwrap();
        let literal = || super::Certificate::of(&store.engine().snapshot()).map(|c| c.literal);
        let unknown = literal();
        assert_eq!(store.consistency(), crate::ConsistencyStatus::Unknown);
        store.use_reasoning_rules(Some(program.clone().with_named_keys(true)));
        let mismatch = literal();
        store.use_reasoning_rules(Some(program));
        assert_eq!(literal(), Some(super::Literal::Clear));
        assert_eq!(store.consistency(), crate::ConsistencyStatus::Consistent);
        assert_eq!(unknown, Some(super::Literal::Unknown));
        assert_eq!(
            mismatch, None,
            "vouches rejects a certificate for a different fingerprint"
        );
        store.use_reasoning_rules(None);
        assert_eq!(literal(), Some(super::Literal::Unknown));
        assert_eq!(store.consistency(), crate::ConsistencyStatus::Unknown);
    }

    /// A reasoning marker whose publication failed after its rename is still cleared by the
    /// next clear of the inferences. A rematerialisation under RDFS
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

    /// A reopened OWL 2 RL store knows its closure but cannot certify that its rules
    /// derived no literal equality. A literal read is complete after a rebuild and
    /// sound-only after a reopen, including after the rules register.
    #[test]
    fn a_marker_read_from_disk_counts_a_literal_equality_as_unknown() {
        use nrese_reasoner::rulesets::Ruleset;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data.nt");
        std::fs::write(
            &data,
            "<http://example.com/a> <http://example.com/p> \"Rex\" .\n",
        )
        .unwrap();
        let config = StoreConfig {
            ql_rewriting: crate::QlRewritingMode::On,
            ..StoreConfig::on_disk(dir.path().join("store"))
        };
        let store = StoreService::new(config.clone()).unwrap();
        store
            .bulk_load(&crate::BulkLoadRequest {
                files: vec![data],
                replace: false,
                graph: crate::GraphTarget::DefaultGraph,
                invalid: None,
            })
            .unwrap();
        store.rematerialise(Ruleset::Owl2Rl).unwrap();
        store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
        // The read's word and its reasons' texts ("none" without a status).
        let word = |store: &StoreService| {
            let read = "SELECT ?o { <http://example.com/a> <http://example.com/p> ?o }";
            let status = store.execute_query_str(read).unwrap().completeness;
            status.map_or(("none".to_owned(), Vec::new()), |s| {
                let reasons = s.reasons.iter().map(|r| r.text.clone()).collect();
                (s.as_str().to_owned(), reasons)
            })
        };
        let (rebuilt, reasons) = word(&store);
        assert_eq!(rebuilt, "complete", "the setup, rebuilt: {reasons:?}");
        drop(store);
        let store = StoreService::new(config).unwrap();
        assert_eq!(
            super::Certificate::of(&store.engine().snapshot()).map(|c| c.literal),
            Some(super::Literal::Unknown)
        );
        store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
        // Certified, so `sound-only` (the closure alone), not `not-certified` or none.
        let (certified, reasons) = word(&store);
        assert_eq!(
            certified, "sound-only",
            "under the certificate made from the marker: {reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|r| r.contains("opened without a rebuild")),
            "under the certificate made from the marker, the reason: {reasons:?}"
        );
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

    /// DC-07: an operation the process limit stopped is named by the cause
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

    /// D2, A-902: a process limit that couldn't be found (its memory unread at the limit's
    /// first need) leaves the operation no room and stops it as unread, never as "no
    /// limit"; none (0) watches nothing.
    #[test]
    fn a_limit_that_couldnt_be_found_stops_the_operation() {
        let stop = MemoryStop::of(Err(Unread::Failed));
        assert_eq!(stop.room(), Err(Unread::Failed));
        assert!(stop.exceeded());
        assert!(matches!(
            stop.error(Interrupted::default()),
            Some(StoreError::MemoryUnreadable { .. })
        ));
        let none = MemoryStop::of(Ok(0));
        assert!(!none.exceeded());
        assert_eq!(none.room(), Ok(u64::MAX));
    }
}

#[cfg(test)]
mod context_tests {
    use super::{context_outside_dl, dl_context};
    use crate::DlAnswers;
    use nrese_sparql::{Completeness, Regime};

    /// One status keys apart under `owl2-dl` and outside it, apart in each DL answers mode,
    /// apart from no status at all, and apart where the QL rewriting ran and where it didn't:
    /// a cached answer of one is never another's.
    #[test]
    fn the_cache_contexts_of_one_status_never_meet() {
        let status = Completeness::under(Regime::Owl2Dl);
        let contexts = [
            context_outside_dl(None, false),
            context_outside_dl(None, true),
            context_outside_dl(Some(&status), false),
            context_outside_dl(Some(&status), true),
            dl_context(DlAnswers::CertainWhereComplete, &status),
            dl_context(DlAnswers::Sound, &status),
            dl_context(DlAnswers::Exact, &status),
        ];
        for (i, a) in contexts.iter().enumerate() {
            for b in &contexts[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}

#[cfg(test)]
#[path = "certificate_lifecycle_tests.rs"]
mod certificate_lifecycle_tests;

#[cfg(test)]
#[path = "run_envelope_tests.rs"]
mod run_envelope_tests;

/// A rematerialisation under `reasoner.equality = "compact"` is read as it will be certified
/// from the moment its stack is published: a reader between the publication and the
/// certificate (`hooks::AFTER_PUBLICATION`) answers as a reader after both, its rows and their
/// count. Over an `owl:sameAs`, from a closure that closes no equality (RDFS: every copy
/// stored, nothing expanded) to one kept over representatives (RDFS-Plus), and back.
#[cfg(test)]
mod equality_publication_tests {
    use std::sync::{Arc, Mutex};

    use super::{StoreService, hooks};
    use crate::StoreConfig;
    use nrese_reasoner::ReasoningMode;

    const DATA: &str = "<http://example.com/a> <http://www.w3.org/2002/07/owl#sameAs> \
                        <http://example.com/b> .\n\
                        <http://example.com/a> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
                        <http://example.com/Dog> .\n\
                        <http://example.com/Dog> <http://www.w3.org/2000/01/rdf-schema#subClassOf> \
                        <http://example.com/Animal> .\n";
    const READS: [&str; 2] = [
        "SELECT ?x WHERE { ?x a <http://example.com/Animal> } ORDER BY ?x",
        "SELECT (COUNT(*) AS ?n) WHERE { ?x a <http://example.com/Animal> }",
    ];

    fn answers(store: &StoreService) -> Vec<String> {
        READS
            .iter()
            .map(|read| {
                let result = store.execute_query_str(read).expect("query");
                String::from_utf8(result.payload).expect("utf-8")
            })
            .collect()
    }

    #[test]
    fn a_compact_stack_is_read_expanded_from_its_publication() {
        let program = |mode| {
            nrese_reasoner::ReasonerConfig::for_mode(mode)
                .materialised_program()
                .expect("it reasons")
        };
        let mut wrong = Vec::new();
        // The Animals `to` entails over DATA: `a, b` under RDFS-Plus, `a` alone under RDFS.
        for (from, to, entailed) in [
            (ReasoningMode::Rdfs, ReasoningMode::RdfsPlus, 2),
            (ReasoningMode::RdfsPlus, ReasoningMode::Rdfs, 1),
        ] {
            let dir = tempfile::tempdir().expect("dir");
            let file = dir.path().join("data.nt");
            std::fs::write(&file, DATA).expect("data");
            let store = Arc::new(
                StoreService::new(StoreConfig {
                    equality_by_representatives: true,
                    equality_compact: true,
                    ..StoreConfig::on_disk(dir.path().join("store"))
                })
                .expect("store"),
            );
            store
                .bulk_load(&crate::BulkLoadRequest {
                    files: vec![file],
                    replace: false,
                    graph: crate::GraphTarget::DefaultGraph,
                    invalid: None,
                })
                .expect("load");
            store
                .rematerialise(program(from))
                .expect("the first closure");
            let between = Arc::new(Mutex::new(Vec::new()));
            let (reader, seen) = (Arc::clone(&store), Arc::clone(&between));
            hooks::AFTER_PUBLICATION.set(Some(Box::new(move || {
                *seen.lock().unwrap() = answers(&reader);
            })));
            store
                .rematerialise(program(to))
                .expect("the second closure");
            let (between, after) = (between.lock().unwrap().clone(), answers(&store));
            let case = format!("{from:?} to {to:?}");
            // Written in: a fresh store goes through the same `finish_held` and would be as wrong.
            if !after[1].contains(&format!("\"value\":\"{entailed}\"")) {
                println!(
                    "CASE {case}: WRONG count {} where {to:?} entails {entailed}",
                    after[1]
                );
                wrong.push(case);
            } else if between == after {
                println!("CASE {case}: right {after:?}");
            } else {
                println!("CASE {case}: WRONG between {between:?} | after {after:?}");
                wrong.push(case);
            }
        }
        assert!(wrong.is_empty(), "wrong: {wrong:?}");
    }

    /// A certified compact store reopened to store its closure expanded, then rematerialised
    /// under the same program, answers as that program entails: over DATA, RDFS-Plus makes
    /// `b` an Animal. The expanded closure reads as the compact stack did; it is stored
    /// otherwise, and its own rows go in.
    #[test]
    fn a_compact_store_rematerialised_expanded_answers_as_its_program_entails() {
        let program = || {
            nrese_reasoner::ReasonerConfig::for_mode(ReasoningMode::RdfsPlus)
                .materialised_program()
                .expect("it reasons")
        };
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("data.nt");
        std::fs::write(&file, DATA).expect("data");
        let config = |compact| StoreConfig {
            equality_by_representatives: true,
            equality_compact: compact,
            ..StoreConfig::on_disk(dir.path().join("store"))
        };
        let store = StoreService::new(config(true)).expect("store");
        store
            .bulk_load(&crate::BulkLoadRequest {
                files: vec![file],
                replace: false,
                graph: crate::GraphTarget::DefaultGraph,
                invalid: None,
            })
            .expect("load");
        store.rematerialise(program()).expect("the compact closure");
        assert!(store.reasoning_state().is_some(), "not certified");
        drop(store);
        let store = StoreService::new(config(false)).expect("the reopened store");
        store
            .rematerialise(program())
            .expect("the expanded closure");
        let animals = answers(&store);
        assert!(
            animals[0].contains("http://example.com/b"),
            "b is no Animal: {animals:?}"
        );
    }

    /// Before a rebuild publishes its compact stack, readers still see the old RDFS-Plus
    /// record and rows. The RDFS record and rows appear together afterward. Over
    /// `a sameAs b . b a Dog . Dog subClassOf Animal`, `a` loaded first so that it has the
    /// smaller id and represents the class: RDFS-Plus's stack keeps `a a Animal` over it and
    /// is read expanded (`a`, `b`); RDFS entails only `b a Animal`. `a` alone is neither.
    #[test]
    fn a_store_read_while_its_reasoning_changes_answers_as_one_program() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("data.nt");
        std::fs::write(
            &file,
            "<http://example.com/a> <http://www.w3.org/2002/07/owl#sameAs> \
             <http://example.com/b> .\n\
             <http://example.com/b> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
             <http://example.com/Dog> .\n\
             <http://example.com/Dog> <http://www.w3.org/2000/01/rdf-schema#subClassOf> \
             <http://example.com/Animal> .\n",
        )
        .expect("data");
        let store = Arc::new(
            StoreService::new(StoreConfig {
                equality_by_representatives: true,
                equality_compact: true,
                ..StoreConfig::on_disk(dir.path().join("store"))
            })
            .expect("store"),
        );
        store
            .bulk_load(&crate::BulkLoadRequest {
                files: vec![file],
                replace: false,
                graph: crate::GraphTarget::DefaultGraph,
                invalid: None,
            })
            .expect("load");
        let program = |mode| {
            nrese_reasoner::ReasonerConfig::for_mode(mode)
                .materialised_program()
                .expect("it reasons")
        };
        store
            .rematerialise(program(ReasoningMode::RdfsPlus))
            .expect("RDFS-Plus");
        let before = answers(&store);
        let window = Arc::new(Mutex::new(None));
        let (reader, seen) = (Arc::clone(&store), Arc::clone(&window));
        hooks::BEFORE_PUBLICATION.set(Some(Box::new(move || {
            *seen.lock().unwrap() = Some((
                answers(&reader),
                reader.reasoning_state().map(|state| state.ruleset),
            ));
        })));
        store
            .rematerialise(program(ReasoningMode::Rdfs))
            .expect("RDFS");
        let after = answers(&store);
        let (between, ruleset) = window.lock().unwrap().take().expect("the hook ran");
        println!(
            "CASE RdfsPlus to Rdfs: before {before:?} | between {between:?} | after {after:?}"
        );
        assert_ne!(
            before, after,
            "the two programs answer alike: the case shows nothing"
        );
        assert_eq!(between, before, "rows before publication");
        assert_eq!(
            ruleset.as_deref(),
            Some("rdfs-plus"),
            "record before publication"
        );
        assert_eq!(
            store
                .reasoning_state()
                .map(|state| state.ruleset)
                .as_deref(),
            Some("rdfs"),
            "record after publication"
        );
    }
}
