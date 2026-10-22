//! Optional logical time provenance, independent of v6's positive physical tail marker.
//! A nonempty segment's fact never changes. Unknown suffixes survive cuts and reuse;
//! readers pin this fact with the file under the WAL lock before releasing that lock.

use std::path::Path;

use super::{CommitRecord, EngineResult, SyncPolicy, Wal, segment_first_revision, segment_path};
use crate::durability::sidecar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Fact {
    /// `None`: all records known; `Some(r)`: revisions >= r are unknown.
    unknown_from: Option<u64>,
}

impl Fact {
    pub(super) const KNOWN: Self = Self { unknown_from: None };
    const UNKNOWN: Self = Self {
        unknown_from: Some(0),
    };

    fn intersect(self, other: Self) -> Self {
        Self {
            unknown_from: match (self.unknown_from, other.unknown_from) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
        }
    }

    fn encode(self, first: u64) -> String {
        let value = match self.unknown_from {
            None => format!("1:{first}:known"),
            Some(revision) => format!("1:{first}:unknown:{revision}"),
        };
        format!("{value}:{:08x}", crc32fast::hash(value.as_bytes()))
    }

    fn decode(value: &str, first: u64) -> Option<Self> {
        let (body, crc) = value.rsplit_once(':')?;
        if u32::from_str_radix(crc, 16).ok()? != crc32fast::hash(body.as_bytes()) {
            return None;
        }
        let mut fields = body.split(':');
        if fields.next()? != "1" || fields.next()?.parse::<u64>().ok()? != first {
            return None;
        }
        let fact = match fields.next()? {
            "known" => Self::KNOWN,
            "unknown" => Self {
                unknown_from: Some(fields.next()?.parse().ok()?),
            },
            _ => return None,
        };
        fields.next().is_none().then_some(fact)
    }

    pub(super) fn apply(self, record: &mut CommitRecord) {
        if self
            .unknown_from
            .is_some_and(|first| record.revision >= first)
            || record.committed_micros == Some(0)
        {
            record.committed_micros = None;
        }
    }
}

pub(super) fn read(path: &Path) -> Fact {
    from_text(path, sidecar::read_text(path).ok().flatten().as_deref())
}

pub(super) fn from_text(path: &Path, text: Option<&str>) -> Fact {
    text.and_then(|text| sidecar::value(text, "time"))
        .and_then(|value| Fact::decode(value, segment_first_revision(path)))
        .unwrap_or(Fact::UNKNOWN)
}

/// Publish before data. On replacement the fact must describe either set of bytes:
/// interruption before the data rename must not certify the old destination's unknowns.
pub(super) fn write(path: &Path, sync: SyncPolicy, fact: Fact) -> EngineResult<Fact> {
    let fact = if path.try_exists()? {
        fact.intersect(read(path))
    } else {
        fact
    };
    sidecar::write(
        path,
        &[
            ("sync", sync.word()),
            ("time", &fact.encode(segment_first_revision(path))),
        ],
    )?;
    Ok(fact)
}

/// Existing sync metadata travels first, with optional provenance intersected on replace.
/// The source is sealed, or (copying) its bytes have already been captured in a partial.
pub(super) fn copy(from: &Path, to: &Path) -> EngineResult<()> {
    let text = sidecar::read_text(from)?;
    copy_text(from, to, text.as_deref())
}

pub(super) fn copy_text(from: &Path, to: &Path, text: Option<&str>) -> EngineResult<()> {
    let source = from_text(from, text);
    let fact = if to.try_exists()? {
        source.intersect(read(to))
    } else {
        source
    };
    let mut text: String = text
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with("time="))
        .map(|line| format!("{line}\n"))
        .collect();
    text.push_str(&format!(
        "time={}\n",
        fact.encode(segment_first_revision(to))
    ));
    sidecar::write_text(to, &text)
}

impl Wal {
    pub(super) fn prepare_time(&mut self, record: &CommitRecord) -> EngineResult<()> {
        if self.magic != super::SEGMENT_MAGIC
            || record.committed_micros.is_some_and(|time| time > 0)
            || self.active_time.unknown_from.is_some()
        {
            return Ok(());
        }
        let fact = Fact {
            unknown_from: Some(record.revision),
        };
        // Rotating keeps a reader's already pinned nonempty file/fact immutable.
        if self.active_bytes > super::SEGMENT_MAGIC.len() as u64 {
            self.rotate_with_time(record.revision, fact)
        } else {
            let path = segment_path(&self.dir, self.active_first_revision);
            match write(&path, self.sync, fact) {
                Ok(fact) => {
                    self.active_time = fact;
                    Ok(())
                }
                Err(error) => {
                    self.poisoned = true;
                    Err(error)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durability::wal::{
        copy_log_segment, read_open_segment, read_segment, segment_facts, truncate_segment,
    };
    use std::fs::{self, File};

    /// Known records stay known through pinning/copy/archive; unknown input remains
    /// unknown through later timed appends, cuts/reuse, missing facts and replacement.
    #[test]
    fn provenance_survives_the_segment_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tempfile::tempdir().unwrap();
        let mut wal = Wal::open(
            dir.path(),
            1,
            u64::MAX,
            SyncPolicy::EveryCommit,
            false,
            Some(archive.path().to_path_buf()),
        )
        .unwrap();
        let record = |revision, time| CommitRecord {
            revision,
            committed_micros: time,
            ..CommitRecord::default()
        };
        assert_eq!(wal.append(&record(1, Some(7))).unwrap(), Some(7));
        let known = segment_path(dir.path(), 1);
        let mut pinned = File::open(&known).unwrap();
        let facts = segment_facts(&known);
        let unknown_floor = wal.append(&record(2, None)).unwrap().unwrap();
        let _ = wal.append(&record(3, Some(9))).unwrap();
        let unknown = segment_path(dir.path(), 2);
        let times = |path: &Path| {
            read_segment(path, 0)
                .unwrap()
                .records
                .into_iter()
                .map(|record| record.committed_micros)
                .collect::<Vec<_>>()
        };
        assert_eq!(times(&known), [Some(7)]);
        assert_eq!(times(&unknown), [None, None]);
        let contents = read_segment(&unknown, 0).unwrap();
        assert_eq!(contents.ends[0].1, unknown_floor);
        assert_eq!(contents.ends[1].1, 9);
        let recovered = contents.records.iter().zip(&contents.ends).fold(
            crate::durability::history::CommitTime::fresh(),
            |time, (record, (_, floor))| time.observe(record.committed_micros).with_floor(*floor),
        );
        assert!(!recovered.known);
        assert_eq!(
            recovered.clock,
            unknown_floor.max(9),
            "unknown clock cannot roll back"
        );
        let copied = tempfile::tempdir().unwrap();
        let target = copy_log_segment(&known, copied.path()).unwrap();
        assert_eq!(times(&target), [Some(7)]);
        // Before a replacement's data publication, its new fact already covers old bytes.
        sidecar::write(&target, &[("sync", "every-commit")]).unwrap();
        copy(&known, &target).unwrap();
        assert_eq!(times(&target), [None]);
        copy_log_segment(&known, copied.path()).unwrap();
        assert_eq!(times(&target), [None]);
        wal.release_through(1).unwrap();
        let pinned = read_open_segment(&mut pinned, &known, 0, facts).unwrap();
        assert_eq!(pinned.records[0].committed_micros, Some(7));
        assert_eq!(times(&segment_path(archive.path(), 1)), [Some(7)]);
        drop(wal);
        let cut = read_segment(&unknown, 0).unwrap().ends[0].0;
        truncate_segment(&unknown, cut).unwrap();
        let mut wal = Wal::open(
            dir.path(),
            3,
            u64::MAX,
            SyncPolicy::EveryCommit,
            false,
            None,
        )
        .unwrap();
        let _ = wal.append(&record(3, Some(11))).unwrap();
        assert_eq!(times(&unknown), [None, None]);
        let archived = segment_path(archive.path(), 1);
        for metadata in [Some("sync=every-commit\ntime=1:1:known:00000000\n"), None] {
            match metadata {
                Some(text) => fs::write(sidecar::path_of(&archived), text).unwrap(),
                None => fs::remove_file(sidecar::path_of(&archived)).unwrap(),
            }
            assert_eq!(times(&archived), [None]);
        }
    }
}
