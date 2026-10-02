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
use crate::index::{CompactionPolicy, IndexVersion, Layout};
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
    /// Check the whole checkpoint when opening: its CRC, every index block and every
    /// dictionary key. Off by default: a checkpoint is used in place from a memory map, and
    /// opening then reads only its headers, so a restart costs milliseconds and memory
    /// grows with what queries touch. On, opening reads the whole file once (and a damaged
    /// one is refused at once rather than failing when a query reaches the damage).
    pub verify_on_open: bool,
    /// Serve the data from a checkpoint once it is written (after a bulk load, and by
    /// background or explicit checkpoints): the index runs and dictionary entries it covers
    /// are then used in place from the file, as after a restart, and their copies in memory
    /// are freed once no snapshot holds them. Memory then grows with what queries touch,
    /// and the OS can page the rest out. Off: the data stays in memory too.
    pub map_checkpoints: bool,
    /// Memory for a bulk load's quads, in bytes (`None`: no limit). A load into an empty
    /// store, or one replacing its data, whose quads outgrow it sorts them in chunks of a
    /// third of it (one filling, one being sorted, and its sorted copy) spilled to the
    /// store's directory, and merges them into each permutation
    /// ([`crate::engine::bulk`]): slower by a pass over the disk, but its quads then take
    /// this much plus one packed permutation, however large the data. The dictionary is
    /// apart: it grows with the distinct terms. Needs `map_checkpoints`.
    pub bulk_load_memory: Option<u64>,
    /// Keep the WAL segments checkpoints cover in `<root>/wal-archive/` instead of deleting
    /// them: with an image backup they replay the store to any later revision
    /// (point-in-time restore). The archive grows until it is pruned outside the engine.
    pub wal_archive: bool,
    /// Recover up to this revision only: the log after it is cut off (the segment holding
    /// the next record truncated before it, later segments removed), and new commits
    /// continue from it. For point-in-time restore on a copy of a store; opening fails if
    /// the checkpoint is newer or the log ends before it.
    pub recover_until: Option<u64>,
    /// Recover the commits made up to this time only (microseconds since 1970), cut as
    /// with `recover_until`; commits logged without a time (before WAL version 6) count as
    /// earlier.
    pub recover_until_micros: Option<u64>,
}

impl Default for DurabilityConfig {
    fn default() -> Self {
        Self {
            sync: SyncPolicy::EveryCommit,
            wal_segment_bytes: 64 << 20,
            checkpoint_after_wal_bytes: 256 << 20,
            verify_on_open: false,
            map_checkpoints: true,
            bulk_load_memory: None,
            wal_archive: false,
            recover_until: None,
            recover_until_micros: None,
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
        crate::engine::spill::remove_leftovers(root)?;
        let clock = std::time::Instant::now();
        let loaded = checkpoint::load_latest(root, dictionary, config.verify_on_open)?;
        tracing::debug!(
            seconds = clock.elapsed().as_secs_f64(),
            "recovery: checkpoint read"
        );
        let clock = std::time::Instant::now();
        // The store's encoding of integer-derived literals: its checkpoint's, else its
        // first WAL segment's, else (a new store) inline.
        let mut integers_in_dictionary = loaded.as_ref().map(|l| l.integers_in_dictionary);
        let mut version = match loaded {
            Some(loaded) => {
                let (asserted, inferred) = match loaded.stacks {
                    checkpoint::Stacks::Packed([asserted, inferred]) => {
                        let index = |stack: Stack, packed| {
                            checkpoint::stack_index(stack, packed).map_err(|error| {
                                EngineError::Corruption(format!("checkpoint: {error}"))
                            })
                        };
                        (
                            index(Stack::Asserted, asserted)?,
                            index(Stack::Inferred, inferred)?,
                        )
                    }
                    checkpoint::Stacks::Quads { quads, inferred } => (
                        IndexVersion::from_quads(Layout::holding(&quads), quads),
                        IndexVersion::from_quads(
                            Layout::DefaultGraph,
                            inferred
                                .into_iter()
                                .map(|triple| triple.in_default_graph())
                                .collect(),
                        ),
                    ),
                };
                Version {
                    asserted,
                    inferred,
                    revision: loaded.revision,
                    dictionary_len: 0,
                    equality: Default::default(),
                }
            }
            None => Version::empty(),
        };
        tracing::debug!(
            seconds = clock.elapsed().as_secs_f64(),
            "recovery: indexes built"
        );

        let wal_dir = wal::wal_dir(root);
        fs::create_dir_all(&wal_dir)?;
        let segments = wal::list_segments(&wal_dir)?;
        let last = segments.len().checked_sub(1);
        if let Some(until) = config.recover_until
            && until < version.revision
        {
            return Err(EngineError::Configuration(format!(
                "the checkpoint is at revision {}, after the requested revision {until}",
                version.revision
            )));
        }
        // Set once the log has been cut at `recover_until`: later segments go.
        let mut cut = false;
        for (position, (_, path)) in segments.iter().enumerate() {
            if cut {
                fs::remove_file(path)?;
                continue;
            }
            let contents = wal::read_segment(path)?;
            if contents.valid_len > 0 {
                match integers_in_dictionary {
                    None => integers_in_dictionary = Some(contents.integers_in_dictionary),
                    Some(store) if store != contents.integers_in_dictionary => {
                        return Err(EngineError::Corruption(format!(
                            "{} encodes integer-derived literals unlike the rest of the store",
                            path.display()
                        )));
                    }
                    Some(_) => {}
                }
            }
            dictionary.set_integers_in_dictionary(integers_in_dictionary.unwrap_or(false));
            for (index, record) in contents.records.iter().enumerate() {
                if record.revision <= version.revision {
                    continue; // covered by the checkpoint
                }
                let after = config
                    .recover_until
                    .is_some_and(|until| record.revision > until)
                    || config
                        .recover_until_micros
                        .zip(record.committed_micros)
                        .is_some_and(|(until, committed)| committed > until);
                if after {
                    // Point-in-time restore: the log ends before this record.
                    match index {
                        0 => fs::remove_file(path)?,
                        _ => wal::truncate_segment(path, contents.ends[index - 1])?,
                    }
                    cut = true;
                    break;
                }
                if record.revision != version.revision + 1 {
                    return Err(EngineError::Corruption(format!(
                        "WAL gap: expected revision {}, found {} in {}",
                        version.revision + 1,
                        record.revision,
                        path.display()
                    )));
                }
                replay(&mut version, dictionary, policy, record)?;
            }
            if contents.torn && !cut {
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

        if let Some(until) = config.recover_until
            && version.revision < until
        {
            return Err(EngineError::Configuration(format!(
                "the log ends at revision {}, before the requested revision {until}",
                version.revision
            )));
        }
        let integers_in_dictionary = integers_in_dictionary.unwrap_or(false);
        dictionary.set_integers_in_dictionary(integers_in_dictionary);
        let wal = Wal::open(
            &wal_dir,
            version.revision + 1,
            config.wal_segment_bytes,
            config.sync,
            integers_in_dictionary,
            config.wal_archive.then(|| wal::archive_dir(root)),
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
    let asserted = match record.names_a_graph() {
        true => version.asserted.with_quads_layout(),
        false => version.asserted.clone(),
    };
    let runs = record.runs(asserted.layout());
    version.asserted = asserted;
    for (stack, run) in Stack::ALL.into_iter().zip(runs) {
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
