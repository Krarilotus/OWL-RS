//! Segmented redo log (imperative shell over [`codec`](super::codec)).
//!
//! Segments live in `<dir>/wal/` and are named after the revision of their first record
//! (`00000000000000000042.wal`), so a segment's revision range ends where the next one
//! starts. Only the newest segment is appended to.
//!
//! Failure model: if an append fails midway, the file may end in a partial frame. The log
//! is then *poisoned*: every later append fails until the engine is reopened, and recovery
//! truncates the torn tail. Appending after a partial frame would turn a recoverable torn
//! tail into mid-log corruption.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::SyncPolicy;
use super::codec::{CommitRecord, Frame, decode_frame, encode_record};
use crate::error::{EngineError, EngineResult};

/// Version 2 added the inferred stack to commit records (roadmap E6); version 3 inlines
/// `xsd:decimal`, `xsd:date` and `xsd:dateTime` (E1), which changes the ids of such terms.
const SEGMENT_MAGIC: &[u8; 8] = b"NRESEWL3";
/// Magic prefix shared by every WAL format version.
const SEGMENT_FAMILY: &[u8; 7] = b"NRESEWL";

pub(crate) fn wal_dir(root: &Path) -> PathBuf {
    root.join("wal")
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
    /// Length of the valid prefix (header plus complete records).
    pub valid_len: u64,
    /// True if bytes after `valid_len` could not be decoded.
    pub torn: bool,
}

pub(crate) fn read_segment(path: &Path) -> EngineResult<SegmentContents> {
    let bytes = fs::read(path)?;
    if bytes.len() < SEGMENT_MAGIC.len() {
        // Crash while creating the segment: treat it as empty and torn.
        return Ok(SegmentContents {
            records: Vec::new(),
            valid_len: 0,
            torn: !bytes.is_empty(),
        });
    }
    if &bytes[..SEGMENT_MAGIC.len()] != SEGMENT_MAGIC {
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
    loop {
        match decode_frame(&bytes[pos..]) {
            Frame::Record(record, used) => {
                records.push(record);
                pos += used;
            }
            Frame::End => break,
            Frame::Invalid => {
                return Ok(SegmentContents {
                    records,
                    valid_len: pos as u64,
                    torn: true,
                });
            }
        }
    }
    Ok(SegmentContents {
        records,
        valid_len: pos as u64,
        torn: false,
    })
}

/// Cuts a torn tail off a segment and syncs the result.
pub(crate) fn truncate_segment(path: &Path, valid_len: u64) -> EngineResult<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    if valid_len < SEGMENT_MAGIC.len() as u64 {
        file.set_len(0)?;
        drop(file);
        fs::remove_file(path)?;
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
}

impl Wal {
    /// Opens the log for appending after recovery. Appends to the newest segment, or starts
    /// one for `next_revision` if there is none.
    pub(crate) fn open(
        dir: &Path,
        next_revision: u64,
        segment_bytes: u64,
        sync: SyncPolicy,
    ) -> EngineResult<Self> {
        fs::create_dir_all(dir)?;
        let (first_revision, active, active_bytes) = match list_segments(dir)?.pop() {
            Some((first_revision, path)) => {
                let mut file = OpenOptions::new().append(true).open(&path)?;
                let len = file.seek(SeekFrom::End(0))?;
                (first_revision, file, len)
            }
            None => {
                let file = create_segment(dir, next_revision)?;
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

    /// Starts a new segment whose first record will be `next_revision`. No-op if the active
    /// segment is still empty.
    pub(crate) fn rotate(&mut self, next_revision: u64) -> EngineResult<()> {
        if self.active_bytes <= SEGMENT_MAGIC.len() as u64 {
            return Ok(());
        }
        self.active.sync_all()?;
        self.active = create_segment(&self.dir, next_revision)?;
        self.active_first_revision = next_revision;
        self.active_bytes = SEGMENT_MAGIC.len() as u64;
        Ok(())
    }

    /// Called after a checkpoint at `revision`: restarts the byte counter and deletes every
    /// segment whose records are all covered by the checkpoint. The active segment stays.
    pub(crate) fn release_through(&mut self, revision: u64) -> EngineResult<()> {
        self.bytes_since_checkpoint = 0;
        let segments = list_segments(&self.dir)?;
        for pair in segments.windows(2) {
            let ((_, path), (next_first, _)) = (&pair[0], &pair[1]);
            if *next_first <= revision + 1 && *next_first <= self.active_first_revision {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}

fn create_segment(dir: &Path, first_revision: u64) -> EngineResult<File> {
    let path = segment_path(dir, first_revision);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;
    file.write_all(SEGMENT_MAGIC)?;
    file.sync_all()?;
    super::sync_dir(dir)?;
    Ok(OpenOptions::new().append(true).open(&path)?)
}
