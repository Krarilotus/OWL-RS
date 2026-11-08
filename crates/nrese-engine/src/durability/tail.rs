//! What opening does with an invalid frame in the newest WAL segment (EEV-03, B-302).
//!
//! Owner, 26 Oct 2026: refuse by default; an opt-in prefix recovery that keeps a copy and
//! logs the loss ([`WalDamage`](super::WalDamage)); later intact records are no longer
//! discarded with the damage.
//!
//! Only one shape is cut without the operator's word, a **torn tail**: what a crash during
//! an append leaves. That is one incomplete final frame (fewer bytes than its header, or a
//! length running past the end of the file) or zeros to the end of the file (a size
//! extension whose data never landed), with no intact frame starting anywhere after the
//! damage. Under `EveryCommit` such a frame was never acknowledged: a commit is
//! acknowledged once its frame is synced, and a failed append poisons the log. Everything
//! else is **damage**: a complete frame that fails its check, or any invalid bytes with an
//! intact frame after them, whose records may well have been acknowledged.
//!
//! Limits: damage to the length field of the very last frame looks like a torn write (the
//! header has no checksum of its own); and nothing finds records lost whole at a frame
//! boundary. Whatever is cut, either way, is first copied to `<root>/wal-cut/`.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::codec::{FRAME_HEADER, Frame, decode_frame};
use crate::error::EngineResult;

/// Where cut log bytes are kept, in the store's directory.
pub(crate) const CUT_DIR: &str = "wal-cut";

/// The smallest frame: its header and an untimed payload of fixed fields only (revision,
/// dictionary start and the five counts).
const MIN_FRAME: usize = FRAME_HEADER + 8 + 8 + 4 * 5;

/// The invalid end of the newest segment, from its first invalid frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Tail {
    /// What an interrupted append leaves: cut it.
    Torn,
    /// Anything else. `intact_after`: the first intact frame found after the damage, as
    /// (offset, revision).
    Damaged { intact_after: Option<(u64, u64)> },
}

/// Classifies the segment `bytes` from offset `at`, its first invalid frame. `next` is the
/// first revision recovery doesn't hold yet (the recovered revision plus one): every record
/// that could be lost has it or a later one. Not the segment's name, a lower bound only: a
/// bulk load advances the revision by a checkpoint alone, without a new segment when the
/// active one is empty. Linear in the bytes after `at`; only candidates whose length fits
/// the file and whose revision could follow are checksummed.
pub(crate) fn classify(bytes: &[u8], at: usize, next: u64) -> Tail {
    let rest = &bytes[at..];
    let incomplete = match rest.get(..4) {
        Some(len) if rest.len() >= FRAME_HEADER => {
            let len = u32::from_le_bytes(len.try_into().expect("4 bytes"));
            FRAME_HEADER as u64 + u64::from(len) > rest.len() as u64
        }
        _ => true,
    };
    let zeros = rest.iter().all(|&byte| byte == 0);
    match intact_after(bytes, at, next) {
        Some(found) => Tail::Damaged {
            intact_after: Some(found),
        },
        None if incomplete || zeros => Tail::Torn,
        None => Tail::Damaged { intact_after: None },
    }
}

/// The first offset after `at` where an intact frame starts whose revision could follow
/// `next`: between `next` and `next` plus as many frames as fit before it.
fn intact_after(bytes: &[u8], at: usize, next: u64) -> Option<(u64, u64)> {
    let word = |at: usize, width: usize| {
        bytes[at..at + width]
            .iter()
            .rev()
            .fold(0u64, |value, &byte| value << 8 | u64::from(byte))
    };
    let last = bytes.len().checked_sub(MIN_FRAME)?;
    for start in at + 1..=last {
        let len = word(start, 4);
        if len < (MIN_FRAME - FRAME_HEADER) as u64
            || (start + FRAME_HEADER) as u64 + len > bytes.len() as u64
        {
            continue;
        }
        let revision = word(start + FRAME_HEADER, 8);
        let reach = ((start - at) / MIN_FRAME) as u64 + 1;
        if revision < next || revision > next.saturating_add(reach) {
            continue;
        }
        if let Frame::Record(record, _) = decode_frame(&bytes[start..])
            && record.revision == revision
        {
            return Some((start as u64, revision));
        }
    }
    None
}

/// Copies the segment's bytes from `at` on to `<root>/wal-cut/<segment name>.<at>`, synced,
/// before they are cut; returns the copy's path. No copy is ever replaced: one of the same
/// bytes (opening again before the cut) is reused, and a different one at that offset (a
/// later cut there) keeps its name while this copy takes the next free `.<n>` after it.
/// Either way the copy's name is synced before this returns, so before the cut.
pub(crate) fn keep(root: &Path, segment: &Path, bytes: &[u8], at: u64) -> EngineResult<PathBuf> {
    let dir = root.join(CUT_DIR);
    fs::create_dir_all(&dir)?;
    super::sync_dir(root)?;
    let name = segment
        .file_name()
        .expect("segment file name")
        .to_string_lossy();
    let cut = &bytes[at as usize..];
    let first = format!("{name}.{at}");
    let mut path = dir.join(&first);
    for n in 1.. {
        match fs::read(&path) {
            // A copy an earlier open placed, whose name may not have been synced then.
            Ok(kept) if kept == cut => return sync_cut_dir(&dir).map(|()| path),
            Ok(_) => path = dir.join(format!("{first}.{n}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    let mut temporary = path.clone().into_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = File::create(&temporary)?;
    file.write_all(cut)?;
    file.sync_all()?;
    drop(file);
    super::place::rename(&temporary, &path)?;
    sync_cut_dir(&dir)?;
    Ok(path)
}

/// Makes the copies' names in `dir` durable.
fn sync_cut_dir(dir: &Path) -> EngineResult<()> {
    #[cfg(test)]
    super::fault::check(&super::fault::FAIL_CUT_DIR_SYNC, "wal-cut directory sync")?;
    super::sync_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durability::codec::{CommitRecord, encode_record};

    /// Records `first..first + n`, framed, after a segment's 8-byte magic.
    fn segment(first: u64, n: u64) -> (Vec<u8>, Vec<usize>) {
        let mut bytes = b"NRESEWL6".to_vec();
        let mut starts = Vec::new();
        for revision in first..first + n {
            starts.push(bytes.len());
            let record = CommitRecord {
                revision,
                keys: vec![format!("Ihttp://example.com/{revision}").into_bytes()],
                committed_micros: Some(1_000),
                ..CommitRecord::default()
            };
            encode_record(&record, &mut bytes).unwrap();
        }
        (bytes, starts)
    }

    /// Every cut inside the last frame, its header included, and zeros to the end are torn;
    /// so is a whole frame's worth of zeros (length 0, whose checksum 0 fits the empty
    /// payload).
    #[test]
    fn what_an_interrupted_append_leaves_is_torn() {
        let (bytes, starts) = segment(5, 3);
        let last = starts[2];
        for cut in last + 1..bytes.len() {
            assert_eq!(classify(&bytes[..cut], last, 7), Tail::Torn, "cut at {cut}");
        }
        let mut zeros = bytes[..last].to_vec();
        zeros.extend_from_slice(&[0; 300]);
        assert_eq!(classify(&zeros, last, 7), Tail::Torn);
        // A segment shorter than its magic.
        assert_eq!(classify(b"NRES", 0, 1), Tail::Torn);
    }

    /// A complete frame failing its check, at the end or not, is damage; so are invalid
    /// bytes of any kind with an intact frame after them, which is found.
    #[test]
    fn damage_and_the_records_after_it_are_found() {
        let (bytes, starts) = segment(5, 4);
        let damaged = |mutate: &dyn Fn(&mut Vec<u8>)| {
            let mut copy = bytes.clone();
            mutate(&mut copy);
            classify(&copy, starts[1], 6)
        };
        let after = |frame: usize, revision: u64| Tail::Damaged {
            intact_after: Some((starts[frame] as u64, revision)),
        };
        // A flipped payload byte: record 7 follows.
        assert_eq!(damaged(&|b| b[starts[1] + 20] ^= 1), after(2, 7));
        // A garbage length running past the end (it would look torn).
        assert_eq!(
            damaged(&|b| b[starts[1]..starts[1] + 4].fill(0xff)),
            after(2, 7)
        );
        // A length under the smallest frame, or 0 (checksum 0 fits the empty payload).
        assert_eq!(
            damaged(&|b| b[starts[1]..starts[1] + 4].fill(0)),
            after(2, 7)
        );
        assert_eq!(
            damaged(&|b| b[starts[1]..starts[1] + 8].fill(0)),
            after(2, 7)
        );
        // Zeros over record 7's header: record 8 follows.
        assert_eq!(
            damaged(&|b| b[starts[1] + 10..starts[2] + 12].fill(0)),
            after(3, 8)
        );
        // The last frame, complete, failing its check.
        let mut copy = bytes.clone();
        copy[starts[3] + 20] ^= 1;
        assert_eq!(
            classify(&copy, starts[3], 8),
            Tail::Damaged { intact_after: None }
        );
    }

    /// A frame after the damage counts only if its revision could follow: a record of an
    /// earlier revision, or one past what fits (`u64::MAX`'s neighbours: no overflow), is
    /// not the log's continuation, so a garbage length before it stays a torn tail.
    #[test]
    fn only_frames_that_could_follow_count() {
        let (bytes, starts) = segment(5, 2);
        for other in [1, u64::MAX - 1] {
            let (stray, _) = segment(other, 1);
            let mut log = bytes[..starts[1]].to_vec();
            log.extend_from_slice(&[0xab; 40]);
            log.extend_from_slice(&stray[8..]);
            assert_eq!(classify(&log, starts[1], 6), Tail::Torn, "revision {other}");
            // Followed by the next revision instead, it is damage.
            let (next, _) = segment(6, 1);
            let mut log = bytes[..starts[1]].to_vec();
            log.extend_from_slice(&[0xab; 40]);
            log.extend_from_slice(&next[8..]);
            assert_eq!(
                classify(&log, starts[1], 6),
                Tail::Damaged {
                    intact_after: Some((starts[1] as u64 + 40, 6))
                }
            );
        }
    }
}
