//! Segmented redo log (imperative shell over [`codec`](super::codec)).
//!
//! Segments live in `<dir>/wal/` and are named after the revision of their first record
//! (`00000000000000000042.wal`), so a segment's revision range ends where the next one
//! starts. Only the newest segment is appended to.
//!
//! With archiving, the segments a checkpoint covers move to `<dir>/wal-archive/` instead of
//! being deleted: with an image backup they replay a store to any later revision
//! (point-in-time restore).
//!
//! Failure model: if an append fails midway, the file may end in a partial frame. The log
//! is then *poisoned*: every later append fails until the engine is reopened, and recovery
//! truncates the torn tail. Appending after a partial frame would turn a recoverable torn
//! tail into mid-log corruption.
//!
//! Each segment records the sync policy it is written under in a sidecar
//! (`<segment>.wal.meta`, [`super::sidecar`]), written before the segment: recovery tells a
//! write that never landed from damage by it ([`super::`] module docs). A segment never
//! mixes policies: opening under another policy starts a new one.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::SyncPolicy;
use super::codec::{
    CommitRecord, FRAME_HEADER, Frame, decode_frame, encode_record, structural_len,
};
use super::fault::{self, Step};
use super::publish::publish_or_fail;
use super::sidecar;
use crate::error::{EngineError, EngineResult};

/// Version 2 added the inferred stack to commit records (roadmap E6); version 3 inlines
/// `xsd:decimal`, `xsd:date` and `xsd:dateTime` (E1), which changes the ids of such terms;
/// version 4 made integer ids order-preserving and split literal kinds (XC1); version 5
/// inlines the integer-derived datatypes. A store created before keeps them in its
/// dictionary and keeps writing version 4 segments, which are otherwise the same.
/// Version 6 adds the commit time to every record.
const SEGMENT_MAGIC: &[u8; 8] = b"NRESEWL6";
const SEGMENT_MAGIC_V5: &[u8; 8] = b"NRESEWL5";
const SEGMENT_MAGIC_V4: &[u8; 8] = b"NRESEWL4";
/// Magic prefix shared by every WAL format version.
const SEGMENT_FAMILY: &[u8; 7] = b"NRESEWL";

pub(crate) fn wal_dir(root: &Path) -> PathBuf {
    root.join("wal")
}

/// Where archived segments go.
pub(crate) fn archive_dir(root: &Path) -> PathBuf {
    root.join("wal-archive")
}

fn segment_path(dir: &Path, first_revision: u64) -> PathBuf {
    dir.join(format!("{first_revision:020}.wal"))
}

/// All segments, sorted by first revision.
pub(crate) fn list_segments(dir: &Path) -> EngineResult<Vec<(u64, PathBuf)>> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let revision = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".wal"))
            .and_then(|stem| stem.parse::<u64>().ok());
        if let Some(revision) = revision {
            segments.push((revision, path));
        }
    }
    segments.sort_unstable_by_key(|(revision, _)| *revision);
    Ok(segments)
}

/// Decoded contents of one segment.
pub(crate) struct SegmentContents {
    pub records: Vec<CommitRecord>,
    /// Where each record ends in the file.
    pub ends: Vec<u64>,
    /// Length of the valid prefix (header plus complete records).
    pub valid_len: u64,
    /// What the bytes after `valid_len` are.
    pub tail: Tail,
    /// A version 4 segment: integer-derived literals are dictionary entries.
    pub integers_in_dictionary: bool,
}

/// What follows a segment's complete records.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Tail {
    /// Nothing.
    Clean,
    /// A write that never landed whole: not acknowledged, or acknowledged under
    /// [`SyncPolicy::OsBuffered`] only, which may lose the last commits. Truncated at the
    /// end of the log.
    Torn,
    /// Damage to what may hold acknowledged commits, and why: recovery stops.
    Corrupt(String),
}

/// The sync policy `segment` was written under, if it has a sidecar; an error if the sidecar
/// can't be read or names a policy this engine doesn't know (recovery must not take it for
/// a missing one and so be lenient).
fn segment_policy(segment: &Path) -> EngineResult<Option<SyncPolicy>> {
    match sidecar::read(segment, "sync")? {
        None => Ok(None),
        Some(word) => SyncPolicy::from_word(&word).map(Some).ok_or_else(|| {
            EngineError::Corruption(format!(
                "{}: unknown sync policy {word:?}",
                sidecar::path_of(segment).display()
            ))
        }),
    }
}

/// Removes what a crash left of a segment or sidecar being written under a temporary name
/// ([`create_segment`], [`copy_log_segment`], [`sidecar::write`]).
pub(crate) fn remove_partials(dir: &Path) -> EngineResult<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if [".creating", ".copying", ".meta.tmp"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Removes a segment and its sidecar.
pub(crate) fn remove_segment(path: &Path) -> EngineResult<()> {
    fs::remove_file(path)?;
    sidecar::remove(path);
    Ok(())
}

pub(crate) fn read_segment(path: &Path) -> EngineResult<SegmentContents> {
    let bytes = fs::read(path)?;
    if bytes.len() < SEGMENT_MAGIC.len() {
        // Crash while creating the segment: treat it as empty and torn.
        return Ok(SegmentContents {
            records: Vec::new(),
            ends: Vec::new(),
            valid_len: 0,
            tail: match bytes.is_empty() {
                true => Tail::Clean,
                false => Tail::Torn,
            },
            integers_in_dictionary: false,
        });
    }
    let magic = &bytes[..SEGMENT_MAGIC.len()];
    let integers_in_dictionary = magic == SEGMENT_MAGIC_V4;
    if magic != SEGMENT_MAGIC && magic != SEGMENT_MAGIC_V5 && !integers_in_dictionary {
        if bytes.starts_with(SEGMENT_FAMILY) {
            return Err(EngineError::UnsupportedFormat(path.to_path_buf()));
        }
        return Err(EngineError::Corruption(format!(
            "{} is not a WAL segment",
            path.display()
        )));
    }
    let mut pos = SEGMENT_MAGIC.len();
    let mut records = Vec::new();
    let mut ends = Vec::new();
    loop {
        match decode_frame(&bytes[pos..]) {
            Frame::Record(record, used) => {
                records.push(record);
                pos += used;
                ends.push(pos as u64);
            }
            Frame::End => break,
            frame @ (Frame::Short | Frame::Damaged(_)) => {
                let damaged = match frame {
                    Frame::Damaged(len) => Some(len),
                    _ => None,
                };
                let timed = magic == SEGMENT_MAGIC;
                // What a record after the damage would have at least: its revision, and its
                // dictionary start where an earlier record of the segment gives it (the log's
                // terms are contiguous within a segment; across one, a bulk load may intern).
                let next = match records.last() {
                    Some(record) => Expected {
                        revision: record.revision + 1,
                        dictionary: Some(record.dictionary_start + record.keys.len() as u64),
                    },
                    None => Expected {
                        revision: segment_first_revision(path),
                        dictionary: None,
                    },
                };
                let policy = segment_policy(path)?;
                return Ok(SegmentContents {
                    tail: classify(&bytes[pos..], damaged, timed, policy, next),
                    records,
                    ends,
                    valid_len: pos as u64,
                    integers_in_dictionary,
                });
            }
        }
    }
    Ok(SegmentContents {
        records,
        ends,
        valid_len: pos as u64,
        tail: Tail::Clean,
        integers_in_dictionary,
    })
}

/// Microseconds since 1970, 0 for a clock before it.
fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_micros() as u64)
}

/// What `rest`, a segment from its first frame that doesn't decode, is: `damaged` holds
/// that frame's length if it is complete; `timed`, a version 6 segment; `policy`, the
/// segment's sync policy if recorded; `next`, the least revision a record after the damage
/// can have ([`Expected`]). The rules are the [`super`] module docs'.
fn classify(
    rest: &[u8],
    damaged: Option<usize>,
    timed: bool,
    policy: Option<SyncPolicy>,
    next: Expected,
) -> Tail {
    if rest.iter().all(|&byte| byte == 0) {
        return Tail::Torn;
    }
    match intact_record_in(rest, next, timed, &mut Work::default()) {
        Search::Found => return Tail::Corrupt("damage before an intact record".into()),
        Search::Exhausted => {
            return Tail::Corrupt(
                "damage too extensive to search for intact records within its budget".into(),
            );
        }
        Search::None => {}
    }
    let Some(len) = damaged else {
        return Tail::Torn;
    };
    if policy != Some(SyncPolicy::EveryCommit) {
        return Tail::Torn;
    }
    let (frame, after) = rest.split_at(len);
    if !after.iter().all(|&byte| byte == 0) {
        return Tail::Corrupt("a damaged record with data after it".into());
    }
    // A version 6 frame ends in its non-zero commit time (the writer sees to it): zeros there
    // mean its end never reached the disk, so the write was in flight. That is read from the
    // segment's format, never from the frame: an untimed frame (version 4) can end in zeros
    // whole, and its damage fails closed.
    match timed && frame.ends_with(&[0; 8]) {
        true => Tail::Torn,
        false => Tail::Corrupt("a damaged last record, synced and so acknowledged".into()),
    }
}

/// The least revision and dictionary start a record after damage can have.
#[derive(Debug, Clone, Copy)]
struct Expected {
    revision: u64,
    /// `None` where no earlier record of the segment gives it.
    dictionary: Option<u64>,
}

/// The outcome of [`intact_record_in`].
#[derive(Debug, PartialEq, Eq)]
enum Search {
    /// An intact record of a revision after the damage.
    Found,
    /// None anywhere.
    None,
    /// The budget ran out first: not known, so recovery refuses.
    Exhausted,
}

/// What a search did: offsets looked at, and bytes parsed or checksummed.
#[derive(Debug, Default)]
struct Work {
    offsets: u64,
    verified: u64,
}

/// The smallest frame: its header, revision, dictionary start, key count and four counts.
const SMALLEST: usize = FRAME_HEADER + 8 + 8 + 4 + 4 * 4;

/// Bytes a search may parse and checksum for a damaged tail of `len` bytes.
fn search_budget(len: usize) -> u64 {
    4 * len as u64 + (1 << 20)
}

/// Whether `rest` (a segment from its first frame that doesn't decode) holds an intact
/// record after the damage anywhere: acknowledged commits, whatever damage, holes or damaged
/// lengths come before. Every offset is tried, the record read by its length field or by its
/// payload's own structure ([`structural_len`]).
///
/// Cost, for a tail of n bytes: each offset is first checked in O(1), with no parse and no
/// checksum. Its revision field must lie in `next.revision ..= next.revision + n / SMALLEST`,
/// and its dictionary start, where the segment gives one, in `start ..= start + n / 4` (each
/// key of a later record takes 4 bytes or more). Term ids carry their kind in their high
/// bits, so in a log's own bytes almost only frame starts pass. An offset that passes is
/// parsed and checksummed, each byte charged to a budget of 4n + 1 MiB
/// ([`search_budget`]) before the work is done, and parsed no further than the budget left. The worst case, data crafted so that every offset passes, is
/// therefore n cheap checks and 4n + 1 MiB of parsing and checksums, linear in the tail.
/// Once the budget is spent the search gives up ([`Search::Exhausted`]) and recovery refuses
/// to open: it never truncates what it couldn't search. Bytes of a term that happen to form
/// a checksummed record of a plausible revision would also count as one: opening then fails
/// closed, and loses no commit.
fn intact_record_in(rest: &[u8], next: Expected, timed: bool, work: &mut Work) -> Search {
    let n = rest.len();
    let latest = next.revision + (n / SMALLEST) as u64;
    let dictionary = next
        .dictionary
        .map(|start| start..=start.saturating_add((n / 4) as u64));
    let budget = search_budget(n);
    let field =
        |tail: &[u8], at: usize| u64::from_le_bytes(tail[at..at + 8].try_into().expect("8 bytes"));
    for at in 0..n.saturating_sub(SMALLEST - 1) {
        work.offsets += 1;
        let tail = &rest[at..];
        if !(next.revision..=latest).contains(&field(tail, FRAME_HEADER))
            || dictionary
                .as_ref()
                .is_some_and(|range| !range.contains(&field(tail, FRAME_HEADER + 8)))
        {
            continue;
        }
        // By its length field, if that fits: its checksum (and decoding) cost its length.
        let len = u32::from_le_bytes(tail[..4].try_into().expect("4 bytes")) as usize;
        if (SMALLEST - FRAME_HEADER..=tail.len() - FRAME_HEADER).contains(&len) {
            if work.verified + 2 * len as u64 > budget {
                return Search::Exhausted;
            }
            work.verified += 2 * len as u64;
            if matches!(decode_frame(tail), Frame::Record(..)) {
                return Search::Found;
            }
        }
        // By its payload's structure (a damaged length field), parsed no further than the
        // budget left, then checksummed.
        let room = (budget - work.verified) as usize;
        let window = &tail[..tail.len().min(FRAME_HEADER + room)];
        match structural_len(window, timed) {
            Ok(size) => {
                if work.verified + 2 * size as u64 > budget {
                    return Search::Exhausted;
                }
                work.verified += 2 * size as u64;
                let crc = u32::from_le_bytes(tail[4..8].try_into().expect("4 bytes"));
                if crc32fast::hash(&tail[FRAME_HEADER..size]) == crc {
                    return Search::Found;
                }
            }
            // The structure runs past the room left: it can't be checked within the budget.
            Err(parsed) if parsed >= window.len() && window.len() < tail.len() => {
                return Search::Exhausted;
            }
            Err(parsed) => work.verified += parsed as u64,
        }
    }
    Search::None
}

/// The revision a segment is named after: a lower bound of its records'.
fn segment_first_revision(path: &Path) -> u64 {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.parse().ok())
        .unwrap_or(0)
}

/// Cuts a torn tail off a segment and syncs the result.
pub(crate) fn truncate_segment(path: &Path, valid_len: u64) -> EngineResult<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    if valid_len < SEGMENT_MAGIC.len() as u64 {
        file.set_len(0)?;
        drop(file);
        remove_segment(path)?;
        return Ok(());
    }
    file.set_len(valid_len)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) struct Wal {
    dir: PathBuf,
    active: File,
    active_first_revision: u64,
    active_bytes: u64,
    bytes_since_checkpoint: u64,
    segment_bytes: u64,
    sync: SyncPolicy,
    poisoned: bool,
    buffer: Vec<u8>,
    /// The magic of new segments (by the store's encoding).
    magic: &'static [u8; 8],
    /// Where released segments go instead of being deleted.
    archive: Option<PathBuf>,
}

impl Wal {
    /// Opens the log for appending after recovery. Appends to the newest segment, or starts
    /// one for `next_revision` if there is none. New segments are of the store's encoding
    /// (`integers_in_dictionary`: version 4).
    pub(crate) fn open(
        dir: &Path,
        next_revision: u64,
        segment_bytes: u64,
        sync: SyncPolicy,
        integers_in_dictionary: bool,
        archive: Option<PathBuf>,
    ) -> EngineResult<Self> {
        let magic = match integers_in_dictionary {
            true => SEGMENT_MAGIC_V4,
            false => SEGMENT_MAGIC,
        };
        fs::create_dir_all(dir)?;
        if let Some(archive) = &archive {
            fs::create_dir_all(archive)?;
        }
        // The newest segment is appended to if it is of the format written now and was
        // written under this sync policy; otherwise (a store from before version 6, another
        // policy, or no policy recorded) a new segment starts.
        let newest = list_segments(dir)?.pop();
        let reusable = match &newest {
            Some((_, path)) => {
                let mut head = [0u8; 8];
                File::open(path)
                    .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut head))
                    .is_ok_and(|()| &head == magic)
                    && segment_policy(path)? == Some(sync)
            }
            None => false,
        };
        let (first_revision, active, active_bytes) = match newest {
            Some((first_revision, path)) if reusable => {
                let mut file = OpenOptions::new().append(true).open(&path)?;
                let len = file.seek(SeekFrom::End(0))?;
                (first_revision, file, len)
            }
            newest => {
                // Commits are acknowledged in the new segment only once what recovery
                // replayed from the newest one is on disk too: after a process crash it may
                // still be in the page cache alone (an OsBuffered write, or one not yet synced).
                if let Some((_, path)) = newest {
                    super::publish::sync_file(&path)?;
                }
                let file = create_segment(dir, next_revision, magic, sync)?;
                (next_revision, file, SEGMENT_MAGIC.len() as u64)
            }
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            active,
            active_first_revision: first_revision,
            active_bytes,
            bytes_since_checkpoint: 0,
            segment_bytes,
            sync,
            poisoned: false,
            buffer: Vec::new(),
            magic,
            archive,
        })
    }

    pub(crate) fn bytes_since_checkpoint(&self) -> u64 {
        self.bytes_since_checkpoint
    }

    /// Appends one record and makes it durable according to the sync policy. The caller
    /// publishes the commit only after this returns `Ok`.
    pub(crate) fn append(&mut self, record: &CommitRecord) -> EngineResult<()> {
        if self.poisoned {
            return Err(EngineError::WalPoisoned);
        }
        if self.active_bytes >= self.segment_bytes {
            self.rotate(record.revision)?;
        }
        self.buffer.clear();
        // Version 4 segments (stores that keep integers in their dictionary) have no times.
        // Every version 6 frame ends in a non-zero time, which recovery relies on (a final
        // frame ending in zeros never landed whole: [`classify`]): a record without one (a
        // replica's from an older primary) or with a zero clock gets this one's.
        let restamped;
        let record = match (self.magic == SEGMENT_MAGIC, record.committed_micros) {
            (true, Some(micros)) if micros > 0 => record,
            (true, _) => {
                restamped = CommitRecord {
                    committed_micros: Some(now_micros().max(1)),
                    ..record.clone()
                };
                &restamped
            }
            (false, _) => {
                restamped = CommitRecord {
                    committed_micros: None,
                    ..record.clone()
                };
                &restamped
            }
        };
        encode_record(record, &mut self.buffer)?; // nothing written yet on error
        let result = self
            .active
            .write_all(&self.buffer)
            .and_then(|()| match self.sync {
                SyncPolicy::EveryCommit => self.active.sync_data(),
                SyncPolicy::OsBuffered => Ok(()),
            });
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error.into());
        }
        self.active_bytes += self.buffer.len() as u64;
        self.bytes_since_checkpoint += self.buffer.len() as u64;
        Ok(())
    }

    /// Starts a new segment whose first record will be `next_revision` or later. No-op if
    /// the active segment is still empty. A failure poisons the log, as an append's does: the
    /// active segment's sync failed (its earlier records can't be trusted to be on disk), or
    /// the new segment is in an unknown state.
    pub(crate) fn rotate(&mut self, next_revision: u64) -> EngineResult<()> {
        if self.poisoned {
            return Err(EngineError::WalPoisoned);
        }
        if self.active_bytes <= SEGMENT_MAGIC.len() as u64 {
            return Ok(());
        }
        let rotated = fault::check(Step::Rotate)
            .and_then(|()| self.active.sync_all())
            .map_err(EngineError::from)
            .and_then(|()| create_segment(&self.dir, next_revision, self.magic, self.sync));
        match rotated {
            Ok(active) => {
                self.active = active;
                self.active_first_revision = next_revision;
                self.active_bytes = SEGMENT_MAGIC.len() as u64;
                Ok(())
            }
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }

    /// Stops all writing until the engine is reopened: a bulk publication's outcome is
    /// unknown until then ([`EngineError::CommitUnknown`]).
    pub(crate) fn poison(&mut self) {
        self.poisoned = true;
    }

    /// Called after a checkpoint at `revision`: restarts the byte counter and deletes (or
    /// archives) every segment whose records are all covered by the checkpoint. The active
    /// segment stays.
    pub(crate) fn release_through(&mut self, revision: u64) -> EngineResult<()> {
        self.bytes_since_checkpoint = 0;
        fault::check(Step::Release)?;
        let segments = list_segments(&self.dir)?;
        for pair in segments.windows(2) {
            let ((_, path), (next_first, _)) = (&pair[0], &pair[1]);
            if *next_first <= revision + 1 && *next_first <= self.active_first_revision {
                match &self.archive {
                    None => remove_segment(path)?,
                    Some(archive) => archive_segment(path, archive)?,
                }
            }
        }
        Ok(())
    }
}

/// Moves a segment into `archive` (a copy where a rename can't cross file systems), its
/// sidecar placed there first.
fn archive_segment(path: &Path, archive: &Path) -> EngineResult<()> {
    let target = archive.join(path.file_name().expect("segment file name"));
    sidecar::copy(path, &target)?;
    if publish_or_fail(path, &target).is_err() {
        // Another file system: a copy, published, then the original goes.
        let mut partial = target.clone().into_os_string();
        partial.push(".copying");
        let partial = PathBuf::from(partial);
        fs::copy(path, &partial)?;
        publish_or_fail(&partial, &target)?;
        fs::remove_file(path)?;
    }
    sidecar::remove(path);
    Ok(())
}

/// Copies the WAL segment `segment` into `dir` (a restore's `wal/`) and returns its path
/// there. Its sidecar goes first, then the segment under a temporary name, synced and
/// renamed: the segment never counts there without the sync policy its recovery reads.
pub fn copy_log_segment(segment: &Path, dir: &Path) -> EngineResult<PathBuf> {
    let name = segment.file_name().ok_or_else(|| {
        EngineError::Configuration(format!("{} is not a segment", segment.display()))
    })?;
    let target = dir.join(name);
    sidecar::copy(segment, &target)?;
    let mut partial = target.clone().into_os_string();
    partial.push(".copying");
    let partial = PathBuf::from(partial);
    fs::copy(segment, &partial)?;
    publish_or_fail(&partial, &target)?;
    Ok(target)
}

/// Removes the WAL segment `segment` (of an archive) and its sidecar.
pub fn remove_log_segment(segment: &Path) -> EngineResult<()> {
    remove_segment(segment)
}

/// A new segment, its sync policy recorded first: a segment that has frames always has it
/// (older ones excepted).
fn create_segment(
    dir: &Path,
    first_revision: u64,
    magic: &[u8; 8],
    sync: SyncPolicy,
) -> EngineResult<File> {
    let path = segment_path(dir, first_revision);
    sidecar::write(&path, &[("sync", sync.word())])?;
    // Written under a temporary name and published: the segment is there whole or not at all.
    let mut partial = path.clone().into_os_string();
    partial.push(".creating");
    let partial = PathBuf::from(partial);
    File::create(&partial)?.write_all(magic)?;
    publish_or_fail(&partial, &path)?;
    Ok(OpenOptions::new().append(true).open(&path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After a process crash the replayed tail may be in the page cache alone: when opening
    /// starts a new segment (here: another sync policy), the newest old one is synced before
    /// the new one is published, so before any commit is acknowledged in it (crash r1 F5).
    #[test]
    fn the_replayed_tail_is_synced_before_a_new_segment() {
        use super::super::publish::record::{self, Op};
        use crate::{DurabilityConfig, Engine, EngineConfig};
        use nrese_rdf::{GraphName, NamedNode, Quad};
        let dir = tempfile::tempdir().unwrap();
        let under = |sync| EngineConfig {
            background_maintenance: false,
            durability: DurabilityConfig {
                sync,
                ..DurabilityConfig::default()
            },
            ..EngineConfig::default()
        };
        {
            let engine = Engine::open(dir.path(), under(SyncPolicy::OsBuffered)).unwrap();
            let iri = |s: &str| NamedNode::new_unchecked(s);
            let mut tx = engine.transaction();
            let quad = Quad::new(
                iri("http://s"),
                iri("http://p"),
                iri("http://o"),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
            tx.commit().unwrap();
        }
        let old = segment_path(&wal_dir(dir.path()), 1);
        let new = segment_path(&wal_dir(dir.path()), 2);
        record::take();
        let engine = Engine::open(dir.path(), under(SyncPolicy::EveryCommit)).unwrap();
        let ops = record::take();
        let synced = ops.iter().position(|op| *op == Op::Sync(old.clone()));
        let published = ops.iter().position(|op| *op == Op::Publish(new.clone()));
        assert!(
            synced.is_some() && synced < published,
            "the old segment must be synced before the new one is published: {ops:?}"
        );
        assert_eq!(engine.stats().revision, 1);
    }

    /// The evidence search is linear in a damaged tail, never quadratic: for a large tail of
    /// garbage and for one crafted so that every offset passes the cheap checks, it looks at
    /// each offset once and parses or checksums at most its budget, giving up (so recovery
    /// refuses) rather than searching on. And it finds a record behind a large damaged
    /// stretch within that budget.
    #[test]
    fn the_evidence_search_is_bounded_by_its_budget() {
        let next = Expected {
            revision: 5,
            dictionary: Some(100),
        };
        let n = 16 << 20;
        // Garbage: a pseudo-random tail.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let garbage: Vec<u8> = (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        // Crafted: every 8 bytes a plausible length, revision and dictionary start, so that
        // offsets pass the cheap checks and are verified.
        let crafted: Vec<u8> = (0..n / 24)
            .flat_map(|_| {
                [(n as u32 / 2).to_le_bytes(), 5u32.to_le_bytes()]
                    .concat()
                    .into_iter()
                    .chain(5u64.to_le_bytes())
                    .chain(100u64.to_le_bytes())
            })
            .collect();
        for (case, tail, expected) in [
            ("garbage", &garbage, Search::None),
            ("crafted", &crafted, Search::Exhausted),
        ] {
            let mut work = Work::default();
            assert_eq!(
                intact_record_in(tail, next, true, &mut work),
                expected,
                "{case}"
            );
            assert!(work.offsets <= tail.len() as u64, "{case}: {work:?}");
            assert!(
                work.verified <= search_budget(tail.len()),
                "{case}: {work:?}"
            );
        }
        // A record behind 1 MiB of garbage is found, within the budget.
        let mut record = Vec::new();
        encode_record(
            &CommitRecord {
                revision: 9,
                dictionary_start: 100,
                committed_micros: Some(1),
                ..CommitRecord::default()
            },
            &mut record,
        )
        .unwrap();
        let mut tail = garbage[..1 << 20].to_vec();
        tail.extend_from_slice(&record);
        let mut work = Work::default();
        assert_eq!(
            intact_record_in(&tail, next, true, &mut work),
            Search::Found
        );
        assert!(work.verified <= search_budget(tail.len()), "{work:?}");
    }
}
