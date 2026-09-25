//! The engine façade (E3/E4): MVCC snapshots, a single writer slot, durability, and
//! background maintenance.
//!
//! Concurrency model:
//! - `Versions::current` holds the latest committed [`Version`]. A [`Snapshot`] clones that
//!   `Arc` under a read lock held for nanoseconds and afterwards touches no shared state except
//!   the append-only dictionary. Readers are never blocked by an open transaction.
//! - One [`Transaction`] at a time holds the writer slot. A commit builds its run without any
//!   lock, appends the WAL record (durable mode) and swaps `current` under a short write lock.
//!   The WAL mutex is held across append and publish, so a checkpoint that snapshots under the
//!   WAL mutex sees exactly the logged revisions.
//! - Compaction is serialised by its own slot and installs under the same short write lock.
//!   Commits only append runs and only the compactor replaces them, so a window planned from
//!   any version keeps its position and contents until the compactor installs the merge.
//!   Small windows are merged inside the commit; larger ones, and checkpoints, go to the
//!   background worker.

mod snapshot;
mod transaction;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use parking_lot::{Mutex, RwLock};

use crate::durability::{DurabilityConfig, Durable, checkpoint};
use crate::error::EngineResult;
use crate::index::compaction::merge_runs;
use crate::index::{CompactionPolicy, IndexVersion};
use crate::term::{Dictionary, DictionaryStats};

pub use snapshot::Snapshot;
pub use transaction::{CommitSummary, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub compaction: CompactionPolicy,
    /// Run large compactions and automatic checkpoints on a background thread. When
    /// `false`, every commit compacts fully inline and checkpoints happen only through
    /// [`Engine::checkpoint`] (deterministic; meant for tests and tools).
    pub background_maintenance: bool,
    /// Used by [`Engine::open`] only.
    pub durability: DurabilityConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            compaction: CompactionPolicy::default(),
            background_maintenance: true,
            durability: DurabilityConfig::default(),
        }
    }
}

/// Point-in-time engine statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineStats {
    pub revision: u64,
    pub quads: u64,
    pub runs: usize,
    pub index_bytes: u64,
    pub dictionary: DictionaryStats,
}

/// One committed state of the dataset. Immutable; shared by snapshots via `Arc`.
#[derive(Debug, Default)]
pub(crate) struct Version {
    pub(crate) index: IndexVersion,
    pub(crate) revision: u64,
    /// Dictionary entries visible to this version; newer terms are hidden from lookups.
    pub(crate) dictionary_len: u64,
}

impl Version {
    fn with_index(&self, index: IndexVersion) -> Self {
        Self {
            index,
            revision: self.revision,
            dictionary_len: self.dictionary_len,
        }
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
    compaction_slot: Mutex<()>,
    policy: CompactionPolicy,
}

impl Versions {
    fn load(&self) -> Arc<Version> {
        self.current.read().clone()
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
    fn compact(&self, inline_only: bool) -> CompactionOutcome {
        let _slot = if inline_only {
            match self.compaction_slot.try_lock() {
                Some(slot) => slot,
                None => return CompactionOutcome::Deferred,
            }
        } else {
            self.compaction_slot.lock()
        };
        loop {
            let version = self.load();
            let Some(plan) = self.policy.plan(version.index.runs()) else {
                return CompactionOutcome::Stable;
            };
            if inline_only && !self.policy.is_inline(&plan) {
                return CompactionOutcome::Deferred;
            }
            let merged = merge_runs(&version.index.runs()[plan.window.clone()]);
            self.publish(|current| {
                debug_assert!(
                    plan.window
                        .clone()
                        .all(|i| Arc::ptr_eq(&current.index.runs()[i], &version.index.runs()[i])),
                    "compaction window changed while merging"
                );
                current.with_index(current.index.with_compacted(plan.window, merged))
            });
        }
    }
}

/// State shared between engine handles and the background worker. The worker holds an
/// `Arc<Shared>`, never the engine itself, so dropping the last [`Engine`] stops it.
struct Shared {
    dictionary: Arc<Dictionary>,
    versions: Versions,
    durable: Option<Durable>,
    wants_compaction: AtomicBool,
    wants_checkpoint: AtomicBool,
}

impl Shared {
    fn snapshot(&self) -> Snapshot {
        Snapshot::new(self.versions.load(), Arc::clone(&self.dictionary))
    }

    /// Writes a checkpoint of the latest revision and releases the WAL it covers. Writers
    /// continue during the (long) file write. Returns the checkpointed revision.
    fn checkpoint(&self) -> EngineResult<u64> {
        let Some(durable) = &self.durable else {
            return Ok(self.versions.load().revision);
        };
        let _slot = durable.checkpoint_slot.lock();
        let snapshot = {
            let mut wal = durable.wal.lock();
            let snapshot = self.snapshot();
            wal.rotate(snapshot.revision() + 1)?;
            snapshot
        };
        checkpoint::write(durable.root(), &snapshot)?;
        durable.wal.lock().release_through(snapshot.revision())?;
        checkpoint::remove_older_than(durable.root(), snapshot.revision())?;
        Ok(snapshot.revision())
    }

    /// Runs whatever maintenance was requested. Called by the background worker.
    fn maintain(&self) {
        if self.wants_compaction.swap(false, Ordering::AcqRel) {
            self.versions.compact(false);
        }
        if self.wants_checkpoint.swap(false, Ordering::AcqRel)
            && let Err(error) = self.checkpoint()
        {
            tracing::error!(%error, "background checkpoint failed");
        }
    }
}

/// Background maintenance thread. Wakeups are coalesced; the thread exits when the engine
/// is dropped (the sender closes), after finishing the task in progress.
struct Worker {
    wake: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn spawn(shared: Arc<Shared>) -> std::io::Result<Self> {
        let (wake, wakeups) = mpsc::sync_channel::<()>(1);
        let thread = std::thread::Builder::new()
            .name("nrese-maintenance".to_owned())
            .spawn(move || {
                while wakeups.recv().is_ok() {
                    shared.maintain();
                }
            })?;
        Ok(Self {
            wake: Some(wake),
            thread: Some(thread),
        })
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            // Full channel = a wakeup is already pending, which is all we need.
            let _ = wake.try_send(());
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
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
        Self::build(config, Arc::default(), None, IndexVersion::default(), 0)
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
        tracing::info!(
            dir = %dir.as_ref().display(),
            revision = recovered.revision,
            quads = recovered.index.len(),
            "engine recovered"
        );
        Self::build(
            config,
            dictionary,
            Some(recovered.durable),
            recovered.index,
            recovered.revision,
        )
    }

    fn build(
        config: EngineConfig,
        dictionary: Arc<Dictionary>,
        durable: Option<Durable>,
        index: IndexVersion,
        revision: u64,
    ) -> EngineResult<Self> {
        let version = Version {
            index,
            revision,
            dictionary_len: dictionary.len(),
        };
        let shared = Arc::new(Shared {
            dictionary,
            versions: Versions {
                current: RwLock::new(Arc::new(version)),
                compaction_slot: Mutex::new(()),
                policy: config.compaction,
            },
            durable,
            wants_compaction: AtomicBool::new(false),
            wants_checkpoint: AtomicBool::new(false),
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

    /// Starts a transaction, waiting for the writer slot. Dropping it without
    /// [`commit`](Transaction::commit) aborts it.
    pub fn transaction(&self) -> Transaction<'_> {
        let slot = self.inner.writer.lock();
        Transaction::new(&self.inner, slot)
    }

    /// Compacts until the policy is satisfied, waiting for any background merge.
    pub fn compact(&self) {
        self.inner.shared.versions.compact(false);
    }

    /// Writes a checkpoint of the latest revision and deletes the WAL segments it covers.
    /// Writers are not blocked while the checkpoint is written. No-op for in-memory engines.
    /// Returns the checkpointed revision.
    pub fn checkpoint(&self) -> EngineResult<u64> {
        self.inner.shared.checkpoint()
    }

    pub fn is_durable(&self) -> bool {
        self.inner.shared.durable.is_some()
    }

    pub fn stats(&self) -> EngineStats {
        let version = self.inner.shared.versions.load();
        EngineStats {
            revision: version.revision,
            quads: version.index.len(),
            runs: version.index.runs().len(),
            index_bytes: version
                .index
                .runs()
                .iter()
                .map(|run| run.memory_bytes())
                .sum(),
            dictionary: self.inner.shared.dictionary.stats(),
        }
    }
}
