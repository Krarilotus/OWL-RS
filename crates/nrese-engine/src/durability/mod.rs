//! Durability (E4): redo WAL, checkpoints and recovery for [`Engine::open`](crate::Engine::open).
//!
//! Directory layout:
//!
//! ```text
//! <dir>/LOCK                                   exclusive process lock
//! <dir>/checkpoint-<revision>.nck              newest full image (older ones are deleted)
//! <dir>/wal/<first revision>.wal               redo log segments
//! ```
//!
//! Commit protocol (in [`Transaction::commit`](crate::Transaction::commit)): build the run,
//! append the WAL record and sync it, then publish the new version. A commit is therefore
//! acknowledged only once it is durable, and nothing is visible that isn't in the log.
//!
//! Recovery = newest checkpoint + replay of all later WAL records in revision order. A torn
//! frame at the end of the last segment is truncated (it was never acknowledged); invalid
//! data anywhere else is reported as corruption instead of being silently skipped.

pub(crate) mod checkpoint;
pub(crate) mod codec;
pub(crate) mod wal;

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;

use crate::engine::{Stack, Version};
use crate::error::{EngineError, EngineResult};
use crate::index::compaction::merge_runs;
use crate::index::{CompactionPolicy, IndexVersion};
use crate::term::Dictionary;
use codec::CommitRecord;
use wal::Wal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    /// `fsync` the WAL before acknowledging each commit. Survives power loss.
    EveryCommit,
    /// Hand WAL writes to the OS without waiting. Survives a process crash, but the last
    /// commits can be lost on power failure or an OS crash.
    OsBuffered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurabilityConfig {
    pub sync: SyncPolicy,
    /// A new WAL segment is started once the active one reaches this size.
    pub wal_segment_bytes: u64,
    /// A background checkpoint is requested once this many WAL bytes were written since the
    /// last one. Needs background maintenance; otherwise call `Engine::checkpoint`.
    pub checkpoint_after_wal_bytes: u64,
}

impl Default for DurabilityConfig {
    fn default() -> Self {
        Self {
            sync: SyncPolicy::EveryCommit,
            wal_segment_bytes: 64 << 20,
            checkpoint_after_wal_bytes: 256 << 20,
        }
    }
}

/// The durable side of an open engine.
pub(crate) struct Durable {
    root: PathBuf,
    pub(crate) wal: Mutex<Wal>,
    pub(crate) checkpoint_slot: Mutex<()>,
    pub(crate) config: DurabilityConfig,
    _lock: File,
}

/// State reconstructed by [`Durable::open`].
pub(crate) struct Recovered {
    pub durable: Durable,
    pub version: Version,
}

impl Durable {
    /// Locks `root`, recovers checkpoint + WAL into `dictionary`, and opens the WAL for
    /// appending.
    pub(crate) fn open(
        root: &Path,
        dictionary: &Dictionary,
        policy: &CompactionPolicy,
        config: DurabilityConfig,
    ) -> EngineResult<Recovered> {
        fs::create_dir_all(root)?;
        let lock = File::create(root.join("LOCK"))?;
        if lock.try_lock().is_err() {
            return Err(EngineError::Locked(root.to_path_buf()));
        }
        checkpoint::remove_temporaries(root)?;
        let timing = std::env::var_os("NRESE_RECOVERY_TIMING").is_some();
        let clock = std::time::Instant::now();
        let loaded = checkpoint::load_latest(root, dictionary)?;
        if timing {
            eprintln!(
                "recovery: checkpoint read {:.3} s",
                clock.elapsed().as_secs_f64()
            );
        }
        let clock = std::time::Instant::now();
        let mut version = match loaded {
            Some(loaded) => Version {
                asserted: IndexVersion::from_quads(Stack::Asserted.layout(), loaded.quads),
                inferred: IndexVersion::from_quads(
                    Stack::Inferred.layout(),
                    loaded
                        .inferred
                        .into_iter()
                        .map(|triple| triple.in_default_graph())
                        .collect(),
                ),
                revision: loaded.revision,
                dictionary_len: 0,
            },
            None => Version::empty(),
        };
        if timing {
            eprintln!(
                "recovery: indexes built {:.3} s",
                clock.elapsed().as_secs_f64()
            );
        }

        let wal_dir = wal::wal_dir(root);
        fs::create_dir_all(&wal_dir)?;
        let segments = wal::list_segments(&wal_dir)?;
        let last = segments.len().checked_sub(1);
        for (position, (_, path)) in segments.iter().enumerate() {
            let contents = wal::read_segment(path)?;
            for record in contents.records {
                if record.revision <= version.revision {
                    continue; // covered by the checkpoint
                }
                if record.revision != version.revision + 1 {
                    return Err(EngineError::Corruption(format!(
                        "WAL gap: expected revision {}, found {} in {}",
                        version.revision + 1,
                        record.revision,
                        path.display()
                    )));
                }
                replay(&mut version, dictionary, policy, &record)?;
            }
            if contents.torn {
                if Some(position) != last {
                    return Err(EngineError::Corruption(format!(
                        "invalid record in {} (not the last segment)",
                        path.display()
                    )));
                }
                tracing::warn!(segment = %path.display(), valid_len = contents.valid_len, "truncating torn WAL tail");
                wal::truncate_segment(path, contents.valid_len)?;
            }
        }

        let wal = Wal::open(
            &wal_dir,
            version.revision + 1,
            config.wal_segment_bytes,
            config.sync,
        )?;
        Ok(Recovered {
            durable: Self {
                root: root.to_path_buf(),
                wal: Mutex::new(wal),
                checkpoint_slot: Mutex::new(()),
                config,
                _lock: lock,
            },
            version,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// Applies one logged commit to both stacks. Compacts per policy as it goes, so replaying a
/// long log keeps the run count logarithmic.
fn replay(
    version: &mut Version,
    dictionary: &Dictionary,
    policy: &CompactionPolicy,
    record: &CommitRecord,
) -> EngineResult<()> {
    for (offset, key) in record.keys.iter().enumerate() {
        dictionary.restore_key(record.dictionary_start + offset as u64, key)?;
    }
    for (stack, run) in Stack::ALL.into_iter().zip(record.runs()) {
        let mut index = version.stack(stack).with_run(run);
        while let Some(plan) = policy.plan(index.runs()) {
            let merged = merge_runs(&index.runs()[plan.window.clone()]);
            index = index.with_compacted(plan.window, merged);
        }
        *version.stack_mut(stack) = index;
    }
    version.revision = record.revision;
    Ok(())
}

/// Makes a rename or file creation in `dir` durable. Directories can't be opened as files on
/// Windows; there, NTFS journals the metadata change and this is a no-op.
pub(crate) fn sync_dir(dir: &Path) -> EngineResult<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}
