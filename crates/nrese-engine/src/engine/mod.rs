//! The engine façade (E3/E4): MVCC snapshots, a single writer slot, durability, and
//! background maintenance.
//!
//! Concurrency model:
//! - `Versions::current` holds the latest committed [`Version`]. A [`Snapshot`] clones that
//!   `Arc` under a read lock held for nanoseconds and afterwards touches no shared state except
//!   the append-only dictionary. Readers are never blocked by an open transaction.
//! - A version holds two index stacks (ADR-0003, roadmap E6): *asserted* statements and
//!   *inferred* ones. They are disjoint, committed together under one revision, and read
//!   through a [`ReadModel`]. The engine stores inferences; deriving them is the reasoner's job.
//! - One [`Transaction`] at a time holds the writer slot. A commit builds its run without any
//!   lock, appends the WAL record (durable mode) and swaps `current` under a short write lock.
//!   The WAL mutex is held across append and publish, so a checkpoint that snapshots under the
//!   WAL mutex sees exactly the logged revisions.
//! - Compaction merges one window at a time (the merge claim, [`Versions::claim`]) without
//!   any slot, and publishes under the compaction slot where the window's runs are still
//!   there. Commits only append runs; holders of the compaction slot replace them (a
//!   merge's publication, a conversion to the quad layout, a mapped checkpoint, a load's
//!   install), and keep it only for serial work. Small windows are merged inside the
//!   commit; larger ones, and checkpoints, go to the background worker.
//! - Slots and Rayon (SYN-22): the holders of the writer and checkpoint slots run parallel
//!   work on their caller's pool, so a Rayon pool thread never waits for either slot: it
//!   takes one only if free, and is refused otherwise ([`EngineError::SlotBusy`]; the
//!   infallible writer forms panic, naming their `try_` form). No holder of the compaction
//!   slot runs parallel work, so anyone may wait for it. Order: writer, checkpoint,
//!   compaction, WAL, the current version; all before any dictionary lock.

mod bulk;
pub mod characteristic;
mod corpora;
mod equality;
#[cfg(test)]
mod lock_tests;
mod private;
mod replication;
#[cfg(test)]
mod shared_state_tests;
mod snapshot;
pub(crate) mod spill;
mod statistics;
mod transaction;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use parking_lot::{Condvar, Mutex, MutexGuard, RwLock};

use crate::durability::codec::CommitRecord;
use crate::durability::{DurabilityConfig, Durable, checkpoint};
use crate::error::{EngineError, EngineResult};
use crate::index::compaction::merge_runs;
use crate::index::run::Run;
use crate::index::{CompactionPolicy, IndexVersion, Layout};
use crate::quad::{EncodedQuad, EncodedTriple};
use crate::term::{Dictionary, DictionaryStats, TermId};

pub use bulk::{BulkLoad, BulkMode, Rematerialisation};
pub use corpora::{CorpusTuning, DEFAULT_READER_CORPUS_BYTES, TextCorpusStats};
pub use equality::Classes as EqualityClasses;
pub use private::PrivateTransaction;
pub use replication::{Applied, LogBatch, LogExpectation};
pub use snapshot::{
    Count, HeavyGroups, InferredMask, InferredSubset, ProbeCursor, SearchRefused, Seek,
    SharedState, Snapshot, SnapshotIdentity, TextScope,
};
pub use transaction::{CommitSummary, Transaction};

/// `slot` (the writer or checkpoint slot, `name`d), taken if free; otherwise waited for off
/// the Rayon pool, and refused on a pool thread ([`EngineError::SlotBusy`]). Its holder runs
/// parallel work on its caller's pool with the slot held: a pool thread waiting for the slot
/// may be what that work needs, or may have that work under it on its own stack.
fn admit<'s>(slot: &'s Mutex<()>, name: &'static str) -> EngineResult<MutexGuard<'s, ()>> {
    if let Some(held) = slot.try_lock() {
        return Ok(held);
    }
    match rayon::current_thread_index() {
        Some(_) => Err(EngineError::SlotBusy(name)),
        None => Ok(slot.lock()),
    }
}

/// The infallible writer forms' answer to a Rayon pool thread that finds the writer slot
/// held, where waiting could hang the process.
#[cold]
fn on_the_pool(instead: &str) -> ! {
    panic!(
        "a Rayon pool thread asked for the engine's writer slot while it is held; waiting \
         could hang (the holder's parallel work may need this thread): use Engine::{instead}"
    )
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub compaction: CompactionPolicy,
    /// Run large compactions and automatic checkpoints on a background thread. When
    /// `false`, every commit compacts fully inline and checkpoints happen only through
    /// [`Engine::checkpoint`] (deterministic; meant for tests and tools).
    pub background_maintenance: bool,
    /// Used by [`Engine::open`] only.
    pub durability: DurabilityConfig,
    /// The process's memory root, which the scratch of every checkpoint the engine writes
    /// (its own, images, bulk loads') is charged within before it is allocated: one more
    /// can't spend what queries or another store's checkpoint hold. `None`: no limit.
    pub admission: Option<Arc<nrese_exec::SharedBudget>>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            compaction: CompactionPolicy::default(),
            background_maintenance: true,
            durability: DurabilityConfig::default(),
            admission: None,
        }
    }
}

/// An image [`Engine::write_image`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub path: std::path::PathBuf,
    pub revision: u64,
    pub quads: u64,
    pub inferred: u64,
}

/// Point-in-time engine statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineStats {
    pub revision: u64,
    /// Asserted quads.
    pub quads: u64,
    /// Inferred quads (all in the default graph; disjoint from the asserted ones).
    pub inferred: u64,
    /// Runs of both stacks.
    pub runs: usize,
    /// Index memory of both stacks on the heap.
    pub index_bytes: u64,
    /// Index data of both stacks used in place from a mapped checkpoint.
    pub index_mapped_bytes: u64,
    pub dictionary: DictionaryStats,
    /// Bytes logged since the last checkpoint (0 in memory): what a restart replays.
    pub wal_bytes_since_checkpoint: u64,
    /// Merges of runs published since the engine started.
    pub compactions: u64,
    /// Checkpoints written since the engine started (bulk loads write one each).
    pub checkpoints: u64,
}

/// Which statements a read sees. GraphDB calls them explicit and implicit statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReadModel {
    /// Asserted and inferred statements (the default).
    #[default]
    Materialised,
    /// Asserted (explicit) statements only.
    Asserted,
    /// Inferred (implicit) statements only.
    Inferred,
}

impl ReadModel {
    pub(crate) fn includes(self, stack: Stack) -> bool {
        matches!(
            (self, stack),
            (Self::Materialised, _)
                | (Self::Asserted, Stack::Asserted)
                | (Self::Inferred, Stack::Inferred)
        )
    }
}

/// The two index stacks of a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stack {
    Asserted,
    Inferred,
}

impl Stack {
    pub(crate) const ALL: [Self; 2] = [Self::Asserted, Self::Inferred];

    /// The read model that sees exactly this stack.
    pub(crate) const fn model(self) -> ReadModel {
        match self {
            Self::Asserted => ReadModel::Asserted,
            Self::Inferred => ReadModel::Inferred,
        }
    }
}

impl CommitRecord {
    /// The runs this record adds to the asserted stack, of `asserted` layout, and to the
    /// inferred stack. Shared by commit and WAL replay, so both build identical versions.
    pub(crate) fn runs(&self, asserted: Layout) -> [Run; 2] {
        let in_default_graph = |triples: &[EncodedTriple]| -> Vec<EncodedQuad> {
            triples.iter().map(|t| t.in_default_graph()).collect()
        };
        [
            Run::from_delta(asserted, &self.inserts, &self.deletes),
            Run::from_delta(
                Layout::DefaultGraph,
                &in_default_graph(&self.inferred_inserts),
                &in_default_graph(&self.inferred_deletes),
            ),
        ]
    }

    /// Whether the record puts a quad in a named graph: then a default-graph asserted stack
    /// turns into a quad stack ([`IndexVersion::with_quads_layout`]).
    pub(crate) fn names_a_graph(&self) -> bool {
        Layout::holding(&self.inserts) == Layout::Quads
            || Layout::holding(&self.deletes) == Layout::Quads
    }
}

/// One committed state of the dataset. Immutable; shared by snapshots via `Arc`.
///
/// Invariant: no quad is visible in both stacks. Transactions enforce it at commit.
#[derive(Debug)]
pub(crate) struct Version {
    pub(crate) asserted: IndexVersion,
    pub(crate) inferred: IndexVersion,
    pub(crate) revision: u64,
    /// A time no commit in it was made after, microseconds since 1970: the latest of its
    /// commits' ([`commit_time`](crate::durability::commit_time),
    /// [`later`](crate::durability::later)); 0 for revision 0. `None` where that isn't known:
    /// recovered from a format 11 checkpoint or an untimed log record, and kept until a bulk
    /// replacement holds nothing from before. Checkpoints keep it (format 12).
    pub(crate) committed_micros: Option<u64>,
    /// Dictionary entries visible to this version; newer terms are hidden from lookups.
    pub(crate) dictionary_len: u64,
    /// Its `owl:sameAs` classes, once a read needs them ([`equality`]).
    pub(crate) equality: Arc<equality::EqualityCell>,
    /// What its statements are: kept where only their layout changes (compaction, a
    /// checkpoint mapped), new for any other version ([`Content`]).
    pub(crate) content: Content,
    /// What its asserted statements are ([`AssertedIdentity`]): new wherever the asserted
    /// stack is built or changed (a commit with asserted changes, a bulk publication,
    /// recovery and open, a transaction's pending state), kept wherever only the inferred
    /// stack, the layout or the view changes. No default: each construction chooses, and
    /// `Engine::build`'s `..version` is the only copy by spread (its version is recovery's
    /// or the empty one).
    pub(crate) asserted_content: AssertedIdentity,
    /// What canonical reads of it read, once asked for ([`equality::CanonicalCache`]).
    pub(crate) canonical: equality::CanonicalCache,
}

/// The identity of a snapshot's asserted statements ([`Snapshot::asserted_identity`]
/// (crate::Snapshot::asserted_identity)): two snapshots with the same one hold the same
/// asserted statements, by the same ids. It is shared across the views of one asserted
/// stack (restricted, inferred-masked, equality and canonical views): a cache keyed by it
/// must not depend on the view's scope or equality mode. A transaction's pending state has
/// one of its own. Unique within the process, never persisted: an open or a recovery gives
/// a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssertedIdentity(u64);

impl AssertedIdentity {
    pub(crate) fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The identity as a number.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// The identity of a version's statements: two versions with the same one show the same
/// statements, whatever their runs. Every commit, transaction's pending state and masked
/// view gets a new one, so a result computed on one is never taken for another's, even
/// at the same revision ([`Snapshot::identity`](crate::Snapshot::identity)). Unique within
/// the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Content(u64);

impl Content {
    pub(crate) fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The identity as a number.
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl Version {
    /// The empty revision 0.
    pub(crate) fn empty() -> Self {
        Self {
            asserted: IndexVersion::empty(Layout::DefaultGraph),
            inferred: IndexVersion::empty(Layout::DefaultGraph),
            revision: 0,
            // No commit yet: every one is after it.
            committed_micros: Some(0),
            dictionary_len: 0,
            equality: Default::default(),
            content: Content::fresh(),
            asserted_content: AssertedIdentity::fresh(),
            canonical: Default::default(),
        }
    }

    pub(crate) fn stack(&self, stack: Stack) -> &IndexVersion {
        match stack {
            Stack::Asserted => &self.asserted,
            Stack::Inferred => &self.inferred,
        }
    }

    pub(crate) fn stack_mut(&mut self, stack: Stack) -> &mut IndexVersion {
        match stack {
            Stack::Asserted => &mut self.asserted,
            Stack::Inferred => &mut self.inferred,
        }
    }

    /// This version with the time `committed_micros` (a bulk publication, stamped as it
    /// commits).
    pub(crate) fn with_time(&self, committed_micros: Option<u64>) -> Self {
        Self {
            asserted: self.asserted.clone(),
            inferred: self.inferred.clone(),
            revision: self.revision,
            committed_micros,
            dictionary_len: self.dictionary_len,
            equality: Arc::clone(&self.equality),
            content: self.content,
            asserted_content: self.asserted_content,
            canonical: Default::default(),
        }
    }

    fn with_stack(&self, stack: Stack, index: IndexVersion) -> Self {
        let mut next = Self {
            asserted: self.asserted.clone(),
            inferred: self.inferred.clone(),
            revision: self.revision,
            committed_micros: self.committed_micros,
            dictionary_len: self.dictionary_len,
            equality: Arc::clone(&self.equality),
            // Only compaction replaces a stack this way: the same statements.
            content: self.content,
            asserted_content: self.asserted_content,
            // A canonical view of the old runs would hold them: made afresh.
            canonical: Default::default(),
        };
        *next.stack_mut(stack) = index;
        next
    }

    fn runs(&self) -> impl Iterator<Item = &Arc<Run>> {
        Stack::ALL
            .into_iter()
            .flat_map(|stack| self.stack(stack).runs())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactionOutcome {
    /// The policy is satisfied.
    Stable,
    /// Work remains that is too large to do inline, or another compaction is running.
    Deferred,
}

struct Versions {
    current: RwLock<Arc<Version>>,
    /// Held to replace runs, and only for serial work: anyone may wait for it.
    compaction_slot: Mutex<()>,
    /// A merge runs ([`Self::claim`]); `merged` is notified when it ends.
    merging: Mutex<bool>,
    merged: Condvar,
    policy: CompactionPolicy,
    /// Merges published.
    compactions: AtomicU64,
}

impl Versions {
    fn load(&self) -> Arc<Version> {
        self.current.read().clone()
    }

    /// Replaces the current version with a prebuilt one. The caller must hold the writer slot,
    /// so that no statement changed since `version` was built from the latest content (a merge
    /// or a mapped checkpoint published meanwhile changed runs only, and is replaced), and the
    /// compaction slot, so that none is published while it is replaced.
    fn install(&self, version: Arc<Version>) {
        *self.current.write() = version;
    }

    /// Replaces the current version with `next(current)` atomically.
    fn publish(&self, next: impl FnOnce(&Version) -> Version) -> Arc<Version> {
        let mut current = self.current.write();
        let version = Arc::new(next(&current));
        *current = Arc::clone(&version);
        version
    }

    /// Merges windows until the policy is satisfied. With `inline_only`, stops before the
    /// first window above the inline limit and never waits for a running compaction.
    ///
    /// The merge runs without the compaction slot, on the caller's pool, and is published
    /// under it where the window's runs are all still there (a conversion to the quad layout,
    /// a mapped checkpoint or a load's install may have replaced them meanwhile: planned
    /// afresh). One merge at a time ([`Self::claim`]).
    fn compact(&self, inline_only: bool) -> CompactionOutcome {
        let Some(_merging) = self.claim(inline_only) else {
            return CompactionOutcome::Deferred;
        };
        loop {
            let version = self.load();
            let plans: Vec<_> = Stack::ALL
                .into_iter()
                .filter_map(|stack| Some((stack, self.policy.plan(version.stack(stack).runs())?)))
                .collect();
            if plans.is_empty() {
                return CompactionOutcome::Stable;
            }
            let Some((stack, plan)) = plans
                .into_iter()
                .find(|(_, plan)| !inline_only || self.policy.is_inline(plan))
            else {
                return CompactionOutcome::Deferred;
            };
            let runs = version.stack(stack).runs();
            let merged = merge_runs(&runs[plan.window.clone()]);
            let _slot = self.compaction_slot.lock();
            self.publish(|current| {
                let index = current.stack(stack);
                // A commit that turned the stack into the quad layout replaced its runs
                // meanwhile: the merge is of runs that are gone.
                let unchanged = index.layout() == merged.layout()
                    && plan.window.clone().all(|i| {
                        index
                            .runs()
                            .get(i)
                            .is_some_and(|run| Arc::ptr_eq(run, &runs[i]))
                    });
                match unchanged {
                    true => {
                        self.compactions.fetch_add(1, Ordering::Relaxed);
                        current.with_stack(stack, index.with_compacted(plan.window, merged))
                    }
                    false => current.with_stack(stack, index.clone()),
                }
            });
        }
    }

    /// The right to merge, until the claim is dropped. Where a merge runs, an inline
    /// compaction defers to it, and so does any on a Rayon pool thread: that thread may hold
    /// a part of the running merge under its wait. Others wait for it to end.
    fn claim(&self, inline_only: bool) -> Option<Merging<'_>> {
        let mut merging = self.merging.lock();
        if *merging {
            if inline_only || rayon::current_thread_index().is_some() {
                return None;
            }
            while *merging {
                self.merged.wait(&mut merging);
            }
        }
        *merging = true;
        Some(Merging(self))
    }

    /// Waits until no merge runs. Off the Rayon pool only (the background worker's thread):
    /// a pool thread may hold a part of the running merge under its wait.
    fn wait_for_merge(&self) {
        let mut merging = self.merging.lock();
        while *merging {
            self.merged.wait(&mut merging);
        }
    }

    /// The asserted stack of the latest version converted to the quad layout (a record's
    /// first quad in a named graph), with the compaction slot, so that nothing replaces the
    /// stack's runs until the caller publishes. Converted without the slot, on the caller's
    /// pool, and kept only where the stack's runs are still the ones converted (a merge or a
    /// mapped checkpoint may have replaced them meanwhile: converted afresh).
    fn converted_asserted(&self) -> (MutexGuard<'_, ()>, IndexVersion) {
        loop {
            let base = self.load();
            let converted = base.asserted.with_quads_layout();
            let slot = self.compaction_slot.lock();
            let now = self.load();
            let (was, is) = (base.asserted.runs(), now.asserted.runs());
            if was.len() == is.len() && was.iter().zip(is).all(|(a, b)| Arc::ptr_eq(a, b)) {
                return (slot, converted);
            }
        }
    }
}

/// A merge running ([`Versions::claim`]); dropping it, on any path, ends the claim.
struct Merging<'v>(&'v Versions);

impl Drop for Merging<'_> {
    fn drop(&mut self) {
        *self.0.merging.lock() = false;
        self.0.merged.notify_all();
    }
}

/// State shared between engine handles and the background worker. The worker holds an
/// `Arc<Shared>`, never the engine itself, so dropping the last [`Engine`] stops it.
struct Shared {
    dictionary: Arc<Dictionary>,
    statistics: Arc<statistics::Statistics>,
    versions: Versions,
    durable: Option<Durable>,
    wants_compaction: AtomicBool,
    wants_checkpoint: AtomicBool,
    /// The background worker is running requested maintenance ([`Engine::settle`]).
    maintaining: AtomicBool,
    /// Checkpoints written.
    checkpoints: AtomicU64,
    /// `owl:sameAs`, when the inferred stack holds the closure over representatives
    /// ([`Engine::set_equality`]).
    equality: RwLock<Option<TermId>>,
    /// What the canonical views versions keep are charged to
    /// ([`Engine::set_canonical_view_budget`]).
    views: RwLock<Arc<nrese_exec::Budget>>,
    /// The checkpoints' scratch is charged within it ([`EngineConfig::admission`]).
    admission: Option<Arc<nrese_exec::SharedBudget>>,
    /// The store's commit clock, microseconds since 1970: the latest time a commit of it was
    /// stamped or logged with, so no commit is stamped before it
    /// ([`crate::durability::commit_time`]). Apart from a version's time, which is unknown
    /// while it holds commits of unknown time: the clock stays known through them (review r1,
    /// R1-F2). Recovered from the checkpoint and the log; changed by writers only.
    clock: AtomicU64,
}

impl Shared {
    /// A stamp for a new commit: never before the store's clock, which takes it.
    fn stamp(&self) -> u64 {
        let stamp = crate::durability::commit_time(Some(self.clock.load(Ordering::Relaxed)));
        self.clock.fetch_max(stamp, Ordering::Relaxed);
        stamp
    }

    /// The installed version, published: the one constructor of a snapshot that grants a
    /// [`snapshot::SharedState`].
    fn snapshot(&self) -> Snapshot {
        Snapshot::published(
            self.versions.load(),
            Arc::clone(&self.dictionary),
            Arc::clone(&self.statistics),
            *self.equality.read(),
            Arc::clone(&self.views.read()),
        )
    }

    /// Writes a checkpoint of the latest revision and releases the WAL it covers. Writers
    /// continue during the (long) file write. Returns the checkpointed revision. With
    /// `map_checkpoints`, the data is then served from the file ([`Self::serve_mapped`]).
    /// On a Rayon pool thread while another checkpoint runs: [`EngineError::SlotBusy`].
    fn checkpoint(&self) -> EngineResult<u64> {
        let Some(durable) = &self.durable else {
            return Ok(self.versions.load().revision);
        };
        let (snapshot, path) = {
            let _slot = admit(&durable.checkpoint_slot, "checkpoint")?;
            let (snapshot, clock) = {
                let mut wal = durable.wal.lock();
                // Commits publish under the log's lock: the clock is that of the snapshot.
                let snapshot = self.snapshot();
                let clock = self.clock.load(Ordering::Relaxed);
                if checkpoint::exists(durable.root(), snapshot.revision()) {
                    // Nothing was committed since (and the file may be in use, mapped);
                    // searches since may have extended the derived indexes.
                    drop(wal);
                    self.dictionary.save_derived();
                    return Ok(snapshot.revision());
                }
                // The names a reopen left pending, before anything is written (deferred-sync
                // design note of 8 Nov): a failure leaves the log as it was.
                wal.settle_names()?;
                wal.rotate(snapshot.revision() + 1)?;
                (snapshot, clock)
            };
            let path =
                checkpoint::write(durable.root(), &snapshot, clock, self.admission.as_ref())?;
            self.checkpoints.fetch_add(1, Ordering::Relaxed);
            durable.wal.lock().release_through(snapshot.revision())?;
            (snapshot, path)
        };
        let revision = snapshot.revision();
        if durable.config.map_checkpoints {
            // After the checkpoint slot: the compaction slot comes first (bulk loads).
            self.serve_mapped(&path, snapshot);
        } else {
            drop(snapshot);
        }
        checkpoint::remove_older_than(durable.root(), revision)?;
        self.dictionary.save_derived();
        Ok(revision)
    }

    /// Serves what `written`, just checkpointed to `path`, holds from the mapped file: the
    /// runs of the current version that it covers are replaced by the file's (where the
    /// version still starts with them), and the dictionary entries by the file's. Their
    /// copies in memory are freed once no snapshot holds them. A failure leaves everything
    /// in memory (the checkpoint is written either way).
    fn serve_mapped(&self, path: &Path, written: Snapshot) {
        let (base, [asserted, inferred]) = match checkpoint::map_written(path) {
            Ok(mapped) => mapped,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "checkpoint not mapped; data stays in memory");
                return;
            }
        };
        let covered = |stack: Stack, current: &Version| {
            let runs = written.version().stack(stack).runs();
            let now = current.stack(stack).runs();
            (now.len() >= runs.len() && runs.iter().zip(now).all(|(a, b)| Arc::ptr_eq(a, b)))
                .then_some(runs.len())
        };
        {
            let _compaction = self.versions.compaction_slot.lock();
            self.versions.publish(|current| {
                let index = |stack: Stack, mapped: &IndexVersion| match covered(stack, current) {
                    Some(runs) => current.stack(stack).with_base(runs, mapped),
                    None => current.stack(stack).clone(),
                };
                Version {
                    asserted: index(Stack::Asserted, &asserted),
                    inferred: index(Stack::Inferred, &inferred),
                    revision: current.revision,
                    committed_micros: current.committed_micros,
                    dictionary_len: current.dictionary_len,
                    equality: Arc::clone(&current.equality),
                    content: current.content,
                    // A checkpoint mapped: the same statements.
                    asserted_content: current.asserted_content,
                    canonical: Default::default(),
                }
            });
        }
        drop(written);
        if let Err(error) = self.dictionary.rebase(base) {
            tracing::warn!(%error, "checkpoint dictionary not mapped; it stays in memory");
        }
    }

    /// Runs whatever maintenance was requested: a task of the background worker. Returns what
    /// is to be done again because it couldn't be done now (on a pool thread nothing waits
    /// for a running merge or checkpoint); the requests stay set for it.
    fn maintain(&self) -> Again {
        let mut again = Again::default();
        if self.wants_compaction.swap(false, Ordering::AcqRel)
            && self.versions.compact(false) == CompactionOutcome::Deferred
        {
            self.wants_compaction.store(true, Ordering::Release);
            again.merge = true;
        }
        if self.wants_checkpoint.swap(false, Ordering::AcqRel) {
            match self.checkpoint() {
                Ok(_) => {}
                // Another checkpoint is being written; it may be of an older revision than
                // the one this request is for: again once it is written.
                Err(crate::EngineError::SlotBusy(_)) => {
                    self.wants_checkpoint.store(true, Ordering::Release);
                    again.checkpoint = true;
                }
                Err(error) => tracing::error!(%error, "background checkpoint failed"),
            }
        }
        again
    }

    /// Waits until no checkpoint is being written. Off the Rayon pool only (the background
    /// worker's thread).
    fn wait_for_checkpoint(&self) {
        if let Some(durable) = &self.durable {
            drop(durable.checkpoint_slot.lock());
        }
    }
}

/// Maintenance a task couldn't do yet ([`Shared::maintain`]).
#[derive(Debug, Default, Clone, Copy)]
struct Again {
    /// A compaction deferred to a merge running.
    merge: bool,
    /// A checkpoint refused while another was being written.
    checkpoint: bool,
}

/// Where the background worker's current task is (`Worker`).
enum Task {
    /// No task.
    None,
    /// Spawned on the pool, not started yet.
    Queued,
    Running,
    /// Done: what is to be done again, or the panic the task ended in.
    Ended(std::thread::Result<Again>),
    /// Never started: a drop on a pool thread took it back, to leave it undone.
    Abandoned,
    /// Never started: a drop off the pool took it back, for the worker's thread to run.
    Reclaimed,
}

/// Which thread is dropping the engine, once one is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shutdown {
    /// A Rayon pool thread: no further task is run, and none queued is started.
    OnPool,
    /// A thread of no pool: what was requested is still done, on the worker's own thread.
    OffPool,
}

/// What the background worker and its drop share.
struct Lifecycle {
    /// The current task, and the drop under way, if any.
    state: Mutex<(Task, Option<Shutdown>)>,
    changed: Condvar,
}

/// Background maintenance. Wakeups are coalesced. While the engine lives, each task (a
/// compaction, a checkpoint) runs as one job of the global Rayon pool, the pool its parallel
/// parts use, and the worker's own thread waits for it. The worker exits when the engine is
/// dropped (the sender closes), after the task in progress, and the drop waits for it
/// without running any pool job and without needing a free one (see `Drop`).
struct Worker {
    wake: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
    life: Arc<Lifecycle>,
}

impl Worker {
    fn spawn(shared: Arc<Shared>) -> std::io::Result<Self> {
        let (wake, wakeups) = mpsc::sync_channel::<()>(1);
        let life = Arc::new(Lifecycle {
            state: Mutex::new((Task::None, None)),
            changed: Condvar::new(),
        });
        let thread = {
            let life = Arc::clone(&life);
            std::thread::Builder::new()
                .name("nrese-maintenance".to_owned())
                .spawn(move || {
                    while wakeups.recv().is_ok() {
                        loop {
                            let again = Self::task(&shared, &life);
                            // Again once what held it up has ended (this thread may wait).
                            if again.merge {
                                shared.versions.wait_for_merge();
                            }
                            if again.checkpoint {
                                shared.wait_for_checkpoint();
                            }
                            if !(again.merge || again.checkpoint) {
                                break;
                            }
                        }
                    }
                })?
        };
        Ok(Self {
            wake: Some(wake),
            thread: Some(thread),
            life,
        })
    }

    /// Runs one task and waits for it: on the pool while the engine lives, on this thread
    /// once a drop off the pool is under way, and not at all once one on a pool thread is.
    fn task(shared: &Arc<Shared>, life: &Arc<Lifecycle>) -> Again {
        let here = {
            let mut state = life.state.lock();
            match state.1 {
                Some(Shutdown::OnPool) => return Again::default(),
                Some(Shutdown::OffPool) => true,
                None => {
                    state.0 = Task::Queued;
                    false
                }
            }
        };
        // Before the requests are taken, so that one is always visible ([`Engine::settle`]).
        shared.maintaining.store(true, Ordering::SeqCst);
        if here {
            let again = shared.maintain();
            shared.maintaining.store(false, Ordering::SeqCst);
            return again;
        }
        {
            // Weak: a job taken back by a drop stays queued until the pool runs it, and must
            // not keep the store (its directory lock, its files) beyond the drop.
            let (shared, life) = (Arc::downgrade(shared), Arc::clone(life));
            rayon::spawn(move || {
                {
                    let mut state = life.state.lock();
                    if !matches!(state.0, Task::Queued) {
                        return;
                    }
                    state.0 = Task::Running;
                }
                // Caught here: a panic in a Rayon job would abort the process. It is resumed
                // on the worker's thread, which ends with it.
                let ended = match shared.upgrade() {
                    Some(shared) => {
                        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            shared.maintain()
                        }));
                        // Let go of the store before the drop waiting for this task can return.
                        drop(shared);
                        ended
                    }
                    None => Ok(Again::default()),
                };
                life.state.lock().0 = Task::Ended(ended);
                life.changed.notify_all();
            });
        }
        let task = {
            let mut state = life.state.lock();
            while matches!(state.0, Task::Queued | Task::Running) {
                life.changed.wait(&mut state);
            }
            std::mem::replace(&mut state.0, Task::None)
        };
        let again = match task {
            Task::Ended(Ok(again)) => again,
            Task::Ended(Err(panic)) => std::panic::resume_unwind(panic),
            // The pool may have no thread free to start it (base ran it on this thread).
            Task::Reclaimed => shared.maintain(),
            _ => Again::default(),
        };
        shared.maintaining.store(false, Ordering::SeqCst);
        again
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            // Full channel = a wakeup is already pending, which is all we need.
            let _ = wake.try_send(());
        }
    }
}

impl Drop for Worker {
    /// Ends maintenance and waits for it (IB0024): when the drop returns, maintenance has
    /// ended and released the store. The wait runs no pool job, and needs no pool thread to
    /// be free: a task that has started runs on a pool thread other than this one and ends
    /// without it, and one queued but not started is taken back.
    /// - On a Rayon pool thread (which may be the only one that could start it), it and any
    ///   further task are left undone. That loses nothing: the next open recovers from the
    ///   checkpoint and the log.
    /// - Off the pool, it and what is still requested run on the worker's own thread, as the
    ///   worker did before it ran tasks on the pool.
    fn drop(&mut self) {
        let shutdown = match rayon::current_thread_index() {
            Some(_) => Shutdown::OnPool,
            None => Shutdown::OffPool,
        };
        {
            let mut state = self.life.state.lock();
            state.1 = Some(shutdown);
            if matches!(state.0, Task::Queued) {
                state.0 = match shutdown {
                    Shutdown::OnPool => Task::Abandoned,
                    Shutdown::OffPool => Task::Reclaimed,
                };
                self.life.changed.notify_all();
            }
        }
        self.wake.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Inner {
    shared: Arc<Shared>,
    writer: Mutex<()>,
    worker: Option<Worker>,
}

impl Inner {
    /// Post-commit maintenance: bounded inline compaction, background requests for the rest.
    fn after_commit(&self, wal_bytes_since_checkpoint: u64) {
        let shared = &self.shared;
        let Some(worker) = &self.worker else {
            shared.versions.compact(false);
            return;
        };
        let mut wake = false;
        if shared.versions.compact(true) == CompactionOutcome::Deferred {
            shared.wants_compaction.store(true, Ordering::Release);
            wake = true;
        }
        if let Some(durable) = &shared.durable
            && wal_bytes_since_checkpoint >= durable.config.checkpoint_after_wal_bytes
        {
            shared.wants_checkpoint.store(true, Ordering::Release);
            wake = true;
        }
        if wake {
            worker.wake();
        }
    }
}

/// A quad store. Cheap to clone; clones share the same dataset.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("stats", &self.stats())
            .finish()
    }
}

impl Engine {
    /// An in-memory engine.
    pub fn new(config: EngineConfig) -> EngineResult<Self> {
        Self::build(config, Arc::default(), None, Version::empty(), 0)
    }

    /// Opens (or creates) a durable engine in `dir`, recovering its last committed revision.
    /// The directory is locked for the lifetime of the engine.
    pub fn open(dir: impl AsRef<Path>, config: EngineConfig) -> EngineResult<Self> {
        let dictionary = Arc::new(Dictionary::default());
        let recovered = Durable::open(
            dir.as_ref(),
            &dictionary,
            &config.compaction,
            config.durability,
        )?;
        let version = recovered.version;
        dictionary.set_derived_dir(dir.as_ref().join("derived"));
        let derived = dir.as_ref().join("derived");
        tracing::info!(
            dir = %dir.as_ref().display(),
            revision = version.revision,
            quads = version.asserted.len(),
            inferred = version.inferred.len(),
            "engine recovered"
        );
        let engine = Self::build(
            config,
            dictionary,
            Some(recovered.durable),
            version,
            recovered.clock,
        )?;
        engine.inner.shared.statistics.set_dir(derived);
        Ok(engine)
    }

    /// `version.dictionary_len` is replaced by the dictionary's actual length; `clock` is
    /// the store's commit clock ([`Shared::clock`]).
    fn build(
        config: EngineConfig,
        dictionary: Arc<Dictionary>,
        durable: Option<Durable>,
        version: Version,
        clock: u64,
    ) -> EngineResult<Self> {
        let version = Version {
            dictionary_len: dictionary.len(),
            ..version
        };
        let shared = Arc::new(Shared {
            dictionary,
            statistics: Arc::new(statistics::Statistics::for_engine()),
            versions: Versions {
                current: RwLock::new(Arc::new(version)),
                compaction_slot: Mutex::new(()),
                merging: Mutex::new(false),
                merged: Condvar::new(),
                policy: config.compaction,
                compactions: AtomicU64::new(0),
            },
            durable,
            wants_compaction: AtomicBool::new(false),
            wants_checkpoint: AtomicBool::new(false),
            maintaining: AtomicBool::new(false),
            checkpoints: AtomicU64::new(0),
            equality: RwLock::new(None),
            views: RwLock::new(Arc::new(nrese_exec::Budget::unlimited())),
            admission: config.admission,
            clock: AtomicU64::new(clock),
        });
        let worker = config
            .background_maintenance
            .then(|| Worker::spawn(Arc::clone(&shared)))
            .transpose()?;
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                writer: Mutex::new(()),
                worker,
            }),
        })
    }

    /// The latest committed state. Never blocks on writers.
    pub fn snapshot(&self) -> Snapshot {
        self.inner.shared.snapshot()
    }

    /// Builds the planner's statistics of the latest state now (the default graph's
    /// characteristic sets in the default read model) and saves them beside the
    /// checkpoint: after a load, so that no query after a start waits for them or shares
    /// the machine with their build.
    pub fn prepare_statistics(&self) {
        let snapshot = self.snapshot();
        self.inner
            .shared
            .statistics
            .prepare_sets(&snapshot, ReadModel::default());
    }

    /// What the planner's statistics kept are charged to from now on: their owner's budget
    /// (a store's own envelope within its catalog's; unset, an unlimited one of their own).
    pub fn charge_statistics_to(&self, budget: Arc<nrese_exec::budget::Budget>) {
        self.inner.shared.statistics.set_budget(budget);
    }

    /// Equality by representatives (work package W4, stage B): with `Some(sameAs)`, the
    /// inferred stack holds the closure over one representative per `owl:sameAs` class,
    /// each other identity stored as `identity sameAs representative`, and snapshots taken
    /// from now on read the default graph expanded to every identity
    /// ([`equality`](self::equality)). The reasoner that fills the stack decides; `None`
    /// reads the stacks as stored.
    pub fn set_equality(&self, same_as: Option<TermId>) {
        *self.inner.shared.equality.write() = same_as;
    }

    /// What the canonical views of versions are charged to while their versions keep them
    /// (canonical equality answers; `Snapshot::with_canonical_equality`): unlimited unless
    /// given one. A view the budget refuses is made on its reader's own budget, for that
    /// reader alone ([`Snapshot::with_canonical_equality_within`]). Snapshots taken from now
    /// on use it.
    pub fn set_canonical_view_budget(&self, budget: Arc<nrese_exec::Budget>) {
        *self.inner.shared.views.write() = budget;
    }

    /// How reader-scoped searches build their corpora from now on ([`CorpusTuning`]; at
    /// first its defaults). Makes the engine's store of corpora if no reader-scoped search
    /// did yet.
    pub fn set_corpus_tuning(&self, tuning: CorpusTuning) {
        if let Some(store) = self.inner.shared.statistics.corpora.get_or_init() {
            store.set_tuning(tuning);
        }
    }

    /// What the corpora of reader-scoped searches the engine keeps are charged to from now
    /// on ([`Snapshot::text_search_in`]; at first [`DEFAULT_READER_CORPUS_BYTES`] of their
    /// own): a store that uses
    /// reader-scoped search gives them its share. The kept corpora and their lineage are let
    /// go (searches holding one keep it, charged to the old budget, until they let go).
    /// Makes the engine's store of corpora if no reader-scoped search did yet.
    pub fn set_text_corpus_budget(&self, budget: Arc<nrese_exec::Budget>) {
        if let Some(store) = self.inner.shared.statistics.corpora.get_or_init() {
            store.set_budget(budget);
        }
    }

    /// What runs the vector graph builds searches start from now on in the background
    /// (spaces of [`SYNC_BUILD`](crate::term::vectors::SYNC_BUILD) vectors or more): the
    /// owner's executor, which may also hold or refuse them. Unset, each build gets a
    /// thread of its own.
    pub fn run_vector_builds_with(&self, builder: crate::term::vectors::VectorBuilder) {
        self.inner.shared.dictionary.set_vector_builder(builder);
    }

    /// `owl:sameAs` if reads expand equality classes ([`Self::set_equality`]).
    pub fn equality(&self) -> Option<TermId> {
        *self.inner.shared.equality.read()
    }

    /// Starts a transaction, waiting for the writer slot. Dropping it without
    /// [`commit`](Transaction::commit) aborts it.
    ///
    /// Contract (here, for [`Self::bulk_load`] and for [`Self::rematerialisation`]): a Rayon
    /// pool thread never waits for the writer slot, whose holder runs parallel work on its
    /// caller's pool. On a pool thread that finds it held, this panics, naming
    /// [`Self::try_transaction`], which returns [`EngineError::SlotBusy`] instead.
    pub fn transaction(&self) -> Transaction<'_> {
        self.try_transaction()
            .unwrap_or_else(|_| on_the_pool("try_transaction"))
    }

    /// [`Self::transaction`], or [`EngineError::SlotBusy`] on a Rayon pool thread while the
    /// writer slot is held.
    pub fn try_transaction(&self) -> EngineResult<Transaction<'_>> {
        let slot = admit(&self.inner.writer, "writer")?;
        Ok(Transaction::new(&self.inner, Some(slot)))
    }

    /// A transaction over the latest snapshot that never commits and doesn't take the
    /// writer slot: for reading what pending changes would make (a client transaction's
    /// reads) while other writers go on. Its changes stay in it; terms it interns are kept
    /// (as an aborted transaction's are). [`Transaction::commit`] fails on it.
    pub fn speculative(&self) -> Transaction<'_> {
        Transaction::new(&self.inner, None)
    }

    /// A private transaction over `snapshot`: speculative changes that read its statements,
    /// with terms of their own past its dictionary length, charged to `budget`, which never
    /// enter the dictionary. It never commits and doesn't take the writer slot. Refused
    /// ([`EngineError::Configuration`]) for a private snapshot or another engine's.
    pub fn speculative_over(
        &self,
        snapshot: &Snapshot,
        budget: Arc<nrese_exec::Budget>,
    ) -> EngineResult<PrivateTransaction<'_>> {
        PrivateTransaction::over(&self.inner, snapshot, budget)
    }

    /// Starts a bulk load, waiting for the writer slot. See [`BulkLoad`]; on a Rayon pool
    /// thread, the contract of [`Self::transaction`].
    pub fn bulk_load(&self, mode: BulkMode) -> BulkLoad<'_> {
        self.try_bulk_load(mode)
            .unwrap_or_else(|_| on_the_pool("try_bulk_load"))
    }

    /// [`Self::bulk_load`], or [`EngineError::SlotBusy`] on a Rayon pool thread while the
    /// writer slot is held.
    pub fn try_bulk_load(&self, mode: BulkMode) -> EngineResult<BulkLoad<'_>> {
        let slot = admit(&self.inner.writer, "writer")?;
        Ok(BulkLoad::new(&self.inner, slot, mode))
    }

    /// Starts replacing the inferred stack, waiting for the writer slot. See
    /// [`Rematerialisation`]; on a Rayon pool thread, the contract of [`Self::transaction`].
    pub fn rematerialisation(&self) -> Rematerialisation<'_> {
        self.try_rematerialisation()
            .unwrap_or_else(|_| on_the_pool("try_rematerialisation"))
    }

    /// [`Self::rematerialisation`], or [`EngineError::SlotBusy`] on a Rayon pool thread while
    /// the writer slot is held.
    pub fn try_rematerialisation(&self) -> EngineResult<Rematerialisation<'_>> {
        let slot = admit(&self.inner.writer, "writer")?;
        Ok(Rematerialisation::new(&self.inner, slot))
    }

    /// Waits until background maintenance has nothing requested or running: a compaction
    /// or checkpoint holds memory and files while it runs. For measurements and tests;
    /// commits may request more at any time.
    pub fn settle(&self) {
        if self.inner.worker.is_none() {
            return;
        }
        let shared = &self.inner.shared;
        while shared.wants_compaction.load(Ordering::SeqCst)
            || shared.wants_checkpoint.load(Ordering::SeqCst)
            || shared.maintaining.load(Ordering::SeqCst)
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Compacts until the policy is satisfied, waiting for any background merge. On a Rayon
    /// pool thread it doesn't wait: where a merge runs, it leaves the work to that merge,
    /// which goes on until the policy is satisfied.
    pub fn compact(&self) {
        self.inner.shared.versions.compact(false);
    }

    /// Writes a checkpoint of the latest revision and deletes the WAL segments it covers.
    /// Writers are not blocked while the checkpoint is written. No-op for in-memory engines.
    /// Returns the checkpointed revision; [`EngineError::SlotBusy`] on a Rayon pool thread
    /// while another checkpoint is written.
    pub fn checkpoint(&self) -> EngineResult<u64> {
        self.inner.shared.checkpoint()
    }

    /// Writes an image of the latest snapshot into `dir`: a checkpoint file (the dictionary
    /// and both stacks) like the store's own, named by its revision, while writers go on. A
    /// data directory holding it opens at that revision.
    pub fn write_image(&self, dir: &std::path::Path) -> EngineResult<ImageInfo> {
        let shared = &self.inner.shared;
        let snapshot = shared.snapshot();
        // Read after the snapshot: no commit in it is later (one after it may be, which only
        // keeps the clock higher, as it must stay).
        let clock = shared.clock.load(Ordering::Relaxed);
        let path =
            crate::durability::checkpoint::write(dir, &snapshot, clock, shared.admission.as_ref())?;
        let version = snapshot.version();
        Ok(ImageInfo {
            path,
            revision: snapshot.revision(),
            quads: version.asserted.len(),
            inferred: version.inferred.len(),
        })
    }

    pub fn is_durable(&self) -> bool {
        self.inner.shared.durable.is_some()
    }

    pub fn stats(&self) -> EngineStats {
        let version = self.inner.shared.versions.load();
        EngineStats {
            revision: version.revision,
            quads: version.asserted.len(),
            inferred: version.inferred.len(),
            runs: version.runs().count(),
            index_bytes: version.runs().map(|run| run.memory_bytes()).sum(),
            index_mapped_bytes: version.runs().map(|run| run.mapped_bytes()).sum(),
            dictionary: self.inner.shared.dictionary.stats(),
            wal_bytes_since_checkpoint: self
                .inner
                .shared
                .durable
                .as_ref()
                .map_or(0, |durable| durable.wal.lock().bytes_since_checkpoint()),
            compactions: self
                .inner
                .shared
                .versions
                .compactions
                .load(Ordering::Relaxed),
            checkpoints: self.inner.shared.checkpoints.load(Ordering::Relaxed),
        }
    }
}
