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
//! Recovery = newest checkpoint + replay of all later WAL records in revision order. In the
//! newest segment, a torn tail (what a crash during an append leaves, [`tail`]) is cut, its
//! bytes kept in `<dir>/wal-cut/`. Other damage there refuses to open, the log left as it
//! is, unless [`WalDamage::KeepPrefix`] allows cutting it too. Invalid data in an earlier
//! segment always refuses to open; nothing is skipped.

pub(crate) mod checkpoint;
pub(crate) mod codec;
#[cfg(test)]
pub(crate) mod fault;
#[cfg(test)]
mod first_use_tests;
#[cfg(test)]
mod link_tests;
mod lock;
#[cfg(test)]
mod note_tests;
pub mod place;
pub(crate) mod tail;
#[cfg(test)]
mod tests;
pub(crate) mod wal;

use std::fs;
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

/// What opening does with damage in the newest WAL segment that a crash during an append
/// doesn't leave ([`tail`]): a complete record failing its check, or invalid bytes with an
/// intact record after them. A torn tail is cut either way, its bytes kept. Neither choice
/// costs a healthy commit anything; they trade downtime after a damaged crash against the
/// silent loss of acknowledged commits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WalDamage {
    /// Refuse to open, leaving the log as it is, until an operator decides (the default):
    /// no acknowledged commit is dropped without someone's word.
    #[default]
    Refuse,
    /// Open with the log up to the damage. The rest is copied to `<dir>/wal-cut/` first and
    /// the cut logged as an error; commits acknowledged after the damage are then in that
    /// copy only.
    KeepPrefix,
}

impl WalDamage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::KeepPrefix => "keep-prefix",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "refuse" => Some(Self::Refuse),
            "keep-prefix" => Some(Self::KeepPrefix),
            _ => None,
        }
    }
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
    /// earlier. Opening fails if the checkpoint's last commit was made after it, or its
    /// time isn't known (format 11 and older, or any commit of unknown time in the store
    /// before it, a bulk replacement since included): restore from an older image, or by
    /// revision.
    pub recover_until_micros: Option<u64>,
    /// Damage in the newest WAL segment: refuse to open (the default), or keep the log up
    /// to it.
    pub wal_damage: WalDamage,
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
            wal_damage: WalDamage::Refuse,
        }
    }
}

/// The durable side of an open engine.
pub(crate) struct Durable {
    root: PathBuf,
    pub(crate) wal: Mutex<Wal>,
    pub(crate) checkpoint_slot: Mutex<()>,
    pub(crate) config: DurabilityConfig,
    /// Set once the names a reopen left pending are synced ([`Self::settle_names`]): read
    /// without the WAL's lock, so the log is served without taking it after that.
    names_settled: std::sync::atomic::AtomicBool,
    _lock: lock::StoreLock,
}

/// State reconstructed by [`Durable::open`].
pub(crate) struct Recovered {
    pub durable: Durable,
    pub version: Version,
    /// The store's commit clock: the latest of the checkpoint's and every logged commit time.
    pub clock: u64,
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
        // Durably named, every level of it (review r1, R1-F2): commits acknowledged in it must
        // not lose the directory that holds them. A root found: its holders' syncs are left to
        // the first durable write that relies on them (`Wal::settle_names`), as the first sync
        // an open does costs it a journal commit and a device flush (timing of 8 Nov,
        // deferred-sync design note). A root made: every level now, as a new store pays once.
        let found = root.is_dir();
        let mut named = match found {
            true => Some(place::Named::resolve(root)?),
            false => {
                place::create_dir_all(root)?;
                None
            }
        };
        // Whether the open changes the store on disk: then what it changed, and the names
        // with it, are synced before it returns, as before.
        let mut changed = !found;
        let lock = lock::StoreLock::acquire(root)?;
        checkpoint::remove_temporaries(root)?;
        crate::engine::spill::remove_leftovers(root)?;
        let clock = std::time::Instant::now();
        let loaded = checkpoint::load_latest(root, dictionary, config.verify_on_open)?;
        let checkpoint_path = loaded.as_ref().map(|loaded| loaded.path.clone());
        tracing::debug!(
            seconds = clock.elapsed().as_secs_f64(),
            "recovery: checkpoint read"
        );
        let clock = std::time::Instant::now();
        // The store's encoding of integer-derived literals: its checkpoint's, else its
        // first WAL segment's, else (a new store) inline.
        let mut integers_in_dictionary = loaded.as_ref().map(|l| l.integers_in_dictionary);
        // The store's commit clock: the checkpoint's, then every logged time, covered or not.
        let mut store_clock = loaded.as_ref().map_or(0, |l| l.clock_micros);
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
                    content: crate::engine::Content::fresh(),
                    // A recovery: replay below changes this version's asserted stack in place.
                    asserted_content: crate::engine::AssertedIdentity::fresh(),
                    asserted,
                    inferred,
                    revision: loaded.revision,
                    // Revision 0 holds no commit: every later one is after it.
                    committed_micros: match loaded.revision {
                        0 => Some(0),
                        _ => loaded.committed_micros,
                    },
                    dictionary_len: 0,
                    equality: Default::default(),
                    canonical: Default::default(),
                    own: Default::default(),
                }
            }
            None => Version::empty(),
        };
        tracing::debug!(
            seconds = clock.elapsed().as_secs_f64(),
            "recovery: indexes built"
        );

        let wal_dir = wal::wal_dir(root);
        // Named in the root: the root's sync after replay (or before the first durable write)
        // names them. One found may itself be a link to a directory elsewhere (review r1,
        // R1-L1): the names its resolution reads (the link's holder, its target's chain) join
        // the holders, under the same found-level rule, as the store's records live there.
        // Resolved from where the root's resolution ended (one walk of their common chain).
        let mut internal = |dir: &Path| -> EngineResult<bool> {
            let made = place::create_store_dir(dir)?;
            if !made && let Some(named) = &mut named {
                let name = dir
                    .file_name()
                    .expect("a store's internal directory has a name");
                named.beyond(name)?;
            }
            Ok(made)
        };
        changed |= internal(&wal_dir)?;
        let archive = config.wal_archive.then(|| wal::archive_dir(root));
        if let Some(archive) = &archive {
            changed |= internal(archive)?;
        }
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
        // A time restore starts from the checkpoint only if every commit in it was made by
        // then: its last commit's time (commit times never go back, `commit_time`) is known
        // and not later (EEV-06).
        if let Some(until) = config.recover_until_micros
            && version.revision > 0
        {
            match version.committed_micros {
                None => {
                    return Err(EngineError::Configuration(format!(
                        "the checkpoint at revision {} has no commit time (format 11 or older, or \
                         a store with commits of unknown time): restore by revision; for restore \
                         by time, export the store as N-Quads and load it into a new data \
                         directory",
                        version.revision
                    )));
                }
                Some(committed) if committed > until => {
                    return Err(EngineError::Configuration(format!(
                        "the checkpoint at revision {} was committed at {committed}, after the \
                         requested time {until} (microseconds since 1970): restore from an \
                         image taken before it",
                        version.revision
                    )));
                }
                Some(_) => {}
            }
        }
        // Set once the log has been cut at `recover_until`: later segments go.
        let mut cut = false;
        // What the checkpoint holds, and the segments records were replayed from.
        let covered_through = version.revision;
        let mut replayed = Vec::new();
        // The newest segment left in place, which `Wal::open` appends to (DUR-5; review r1, N2:
        // known here, not by listing the log again): one covered by the checkpoint included,
        // one the replay removed (a point-in-time cut, a tail without even a header) not.
        let mut newest = None;
        for (position, (_, path)) in segments.iter().enumerate() {
            if cut {
                fs::remove_file(path)?;
                continue;
            }
            let mut kept = true;
            // Wholly covered by the checkpoint (F7): its records all come before the next
            // segment's first, which the checkpoint holds, so none is replayed. A release that
            // failed or crashed left it, and damage in it can't cost a record: it never
            // refuses the open. Its intact records' times still raise the store's clock.
            if segments
                .get(position + 1)
                .is_some_and(|(next, _)| *next <= covered_through.saturating_add(1))
            {
                tracing::debug!(segment = %path.display(), "skipped a WAL segment the checkpoint covers");
                let damaged = match wal::read_segment(path) {
                    Ok(contents) => {
                        let mut damaged = contents.torn;
                        // A time past what a store keeps is damage here too: not taken.
                        for record in &contents.records {
                            match check_commit_time(record) {
                                Ok(()) => {
                                    store_clock =
                                        store_clock.max(record.committed_micros.unwrap_or(0));
                                }
                                Err(_) => damaged = true,
                            }
                        }
                        damaged
                    }
                    Err(_) => true,
                };
                if damaged {
                    tracing::warn!(segment = %path.display(), "damage in a WAL segment the checkpoint covers: its records are all in the checkpoint, nothing is lost");
                }
                newest = Some(path);
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
                check_commit_time(record)?;
                store_clock = store_clock.max(record.committed_micros.unwrap_or(0));
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
                    kept = match index {
                        0 => {
                            fs::remove_file(path)?;
                            false
                        }
                        _ => wal::truncate_segment(path, contents.ends[index - 1])?,
                    };
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
                if replayed.last() != Some(&path) {
                    replayed.push(path);
                }
            }
            if contents.torn && !cut {
                if Some(position) != last {
                    return Err(EngineError::Corruption(format!(
                        "invalid record in {} (not the last segment)",
                        path.display()
                    )));
                }
                let bytes = fs::read(path)?;
                let at = contents.valid_len;
                // Every record that could be lost is past what is recovered.
                let next = version.revision.saturating_add(1);
                let cut_bytes = (bytes.len() as u64).saturating_sub(at);
                match (tail::classify(&bytes, at as usize, next), config.wal_damage) {
                    (tail::Tail::Torn, _) => {
                        let kept = tail::keep(root, path, &bytes, at)?;
                        tracing::warn!(segment = %path.display(), offset = at, bytes = cut_bytes, kept = %kept.display(), "cut a torn WAL tail (an append a crash interrupted)");
                    }
                    (tail::Tail::Damaged { intact_after }, WalDamage::Refuse) => {
                        let after = intact_after.map_or(String::new(), |(offset, revision)| {
                            format!(
                                "; an intact record of revision {revision} follows at byte {offset}"
                            )
                        });
                        return Err(EngineError::Corruption(format!(
                            "damaged WAL record in {} at byte {at}, after revision {}{after}. \
                             The log is left as it is. To open with the log up to revision {} \
                             (the rest copied to {}/), set the WAL damage policy to keep the \
                             prefix (server: store.wal_damage = \"keep-prefix\")",
                            path.display(),
                            version.revision,
                            version.revision,
                            tail::CUT_DIR,
                        )));
                    }
                    (tail::Tail::Damaged { intact_after }, WalDamage::KeepPrefix) => {
                        let kept = tail::keep(root, path, &bytes, at)?;
                        tracing::error!(
                            segment = %path.display(),
                            offset = at,
                            bytes = cut_bytes,
                            kept_through_revision = version.revision,
                            intact_after = ?intact_after,
                            kept = %kept.display(),
                            "cut a damaged WAL tail: commits acknowledged after the damage are only in the copy"
                        );
                    }
                }
                kept = wal::truncate_segment(path, at)?;
                changed = true;
            }
            if kept {
                newest = Some(path);
            }
        }
        // A point-in-time cut removed or truncated segments.
        changed |= cut;

        if let Some(until) = config.recover_until
            && version.revision < until
        {
            return Err(EngineError::Configuration(format!(
                "the log ends at revision {}, before the requested revision {until}",
                version.revision
            )));
        }
        // What was recovered is durable before a new commit is acknowledged (F5): replayed
        // records may be in the page cache only (written under `OsBuffered`, or not synced yet
        // when the process died), and new commits may go to a segment after them. A
        // checkpoint renamed just before the crash, and segments removed or cut, likewise. The
        // root's sync also names the WAL's directory and the archive, found in it above.
        // - What was replayed is synced now: no reader sees content a power loss could undo.
        // - The names (the root's holders, the WAL's directory, the root) are synced now if the
        //   open changed the store; otherwise before the first durable write that relies on
        //   them: a commit's record, a checkpoint, a bulk publication, the log served to a
        //   follower (`Wal::settle_names`). Nothing is acknowledged before they are durable.
        for path in &replayed {
            place::sync_file(path)?;
        }
        // The store's own directories keyed as the holders are (one directory, one key); a
        // made root has no holders to meet.
        let (mut holders, own) = match named {
            Some(named) => {
                let own = vec![
                    (wal_dir.clone(), named.key(wal_dir.file_name())),
                    (root.to_path_buf(), named.key(None)),
                ];
                (named.into_holders(), own)
            }
            None => (
                Vec::new(),
                vec![
                    (wal_dir.clone(), wal_dir.clone()),
                    (root.to_path_buf(), root.to_path_buf()),
                ],
            ),
        };
        // The authoritative files reached through links (DUR-5, fix check r2 on DUR-4, R2-F1):
        // the checkpoint loaded, each segment replayed, and the newest segment, which
        // `Wal::open` appends to (a covered one included), once: it is often the last replayed.
        // A file that is a link has its target's chain named with the found levels; a regular
        // file adds nothing.
        let newest = newest.filter(|path| replayed.last() != Some(path));
        let replayed_files = replayed.iter().map(|path| &**path);
        for file in checkpoint_path.iter().chain(replayed_files).chain(newest) {
            holders.extend(place::file_holders(file)?);
        }
        let mut pending = wal::PendingNames::new(holders, own);
        if changed {
            pending.settle()?;
        }
        let integers_in_dictionary = integers_in_dictionary.unwrap_or(false);
        dictionary.set_integers_in_dictionary(integers_in_dictionary);
        let wal = Wal::open(
            &wal_dir,
            version.revision + 1,
            config.wal_segment_bytes,
            config.sync,
            integers_in_dictionary,
            archive,
            pending,
        )?;
        Ok(Recovered {
            durable: Self {
                root: root.to_path_buf(),
                wal: Mutex::new(wal),
                checkpoint_slot: Mutex::new(()),
                config,
                names_settled: std::sync::atomic::AtomicBool::new(false),
                _lock: lock,
            },
            clock: store_clock.max(version.committed_micros.unwrap_or(0)),
            version,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Syncs the names the open left pending (`Wal::settle_names`), for what relies on them
    /// without appending: the log served to a follower. Takes the WAL's lock only until they
    /// are settled; holds no other lock (call it before taking any).
    pub(crate) fn settle_names(&self) -> EngineResult<()> {
        use std::sync::atomic::Ordering;
        if self.names_settled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.wal.lock().settle_names()?;
        self.names_settled.store(true, Ordering::Release);
        Ok(())
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
        true => version.asserted.with_quads_layout_alone(),
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
    // The latest of all its commits: a log an earlier binary wrote may step back. A record
    // logged without a time (a version 4 or 5 segment) makes it unknown.
    version.committed_micros = later(version.committed_micros, record.committed_micros);
    Ok(())
}

/// The latest commit time a store keeps, microseconds since 1970: a checkpoint holds it in 63
/// bits, the 64th marking a version of unknown time. A wall clock reaches it in the year
/// 292,277; only a replicated record or a damaged log can say otherwise.
pub(crate) const MAX_COMMIT_TIME: u64 = (1 << 63) - 1;

/// Refuses a logged record whose commit time is past [`MAX_COMMIT_TIME`] (sol-loop, review
/// fix S2): its time couldn't be kept, and kept short it would read as earlier than it was.
pub(crate) fn check_commit_time(record: &CommitRecord) -> EngineResult<()> {
    match record.committed_micros {
        Some(time) if time > MAX_COMMIT_TIME => Err(EngineError::Corruption(format!(
            "log record {} has a commit time past what a store keeps ({time})",
            record.revision
        ))),
        _ => Ok(()),
    }
}

/// The time of a version that adds a commit made at `commit` to one at `previous`: the later
/// of the two, so it bounds every commit in it; unknown (`None`) if either is.
pub(crate) fn later(previous: Option<u64>, commit: Option<u64>) -> Option<u64> {
    previous
        .zip(commit)
        .map(|(previous, commit)| previous.max(commit))
}

/// The commit time of a revision after one committed at `previous`: the wall clock, in
/// microseconds since 1970, but never before `previous` and never 0 (a checkpoint's
/// "unknown"). So a store's commit times never go back, whatever its clock does, and a
/// version's time is that of its latest commit: one made by a time has every commit before
/// it made by then too (a time restore's cut, EEV-06).
pub(crate) fn commit_time(previous: Option<u64>) -> u64 {
    now_micros().max(previous.unwrap_or(0)).max(1)
}

/// The wall clock, microseconds since 1970 (0 before it).
fn now_micros() -> u64 {
    #[cfg(test)]
    if let Some(at) = fault::CLOCK.get() {
        return at;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_micros() as u64)
}

/// Makes a rename, creation or removal in `dir` durable, on every platform ([`place`]).
pub(crate) fn sync_dir(dir: &Path) -> EngineResult<()> {
    Ok(place::sync_dir(dir)?)
}
