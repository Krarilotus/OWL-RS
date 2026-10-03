//! Read replicas (hardware plan H6, step 1): a primary ships its write-ahead log, a replica
//! applies it, commit by commit, and answers queries.
//!
//! - **The primary** serves [`Engine::log_since`]: the committed records after a revision,
//!   read from its log segments (and their archive), as encoded frames. Records leave the
//!   log when a checkpoint covers them, and bulk loads and rematerialisations write a
//!   checkpoint instead of records: a replica asking for what the log no longer holds gets
//!   [`EngineError::LogTruncated`] and starts again from an image
//!   ([`Engine::write_image`]).
//! - **A replica** starts from the primary's image and applies its records in order
//!   ([`Engine::apply_log`]): each becomes a commit here with the primary's revision and
//!   term ids, in its own log too when the replica is durable, so it restarts where it
//!   stopped. It must intern no term of its own (it takes no writes): a record whose
//!   dictionary doesn't continue the replica's is refused, and the replica starts again.
//!
//! Inferred statements come with the records: a replica doesn't reason.

use super::{Engine, Version};
use crate::durability::codec::{CommitRecord, Frame, decode_frame, encode_record};
use crate::durability::wal;
use crate::error::{EngineError, EngineResult};
use crate::index::Layout;

/// Records of a primary's log ([`Engine::log_since`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogBatch {
    /// The records' frames, back to back (what [`Engine::apply_log`] takes).
    pub frames: Vec<u8>,
    /// How many records.
    pub records: usize,
    /// The revision of the last record (the asked-for revision if there is none).
    pub last: u64,
    /// The primary's latest revision when the batch was read.
    pub latest: u64,
}

impl Engine {
    /// The committed records after revision `after`, from the write-ahead log and its
    /// archive, as encoded frames: at least one record if there is any, then more while
    /// they fit in `max_bytes`. Empty when `after` is the latest revision.
    /// [`EngineError::LogTruncated`] if the log no longer holds the record after `after`
    /// (a checkpoint covered it, or a bulk load or rematerialisation wrote none); an
    /// in-memory engine has no log at all.
    pub fn log_since(&self, after: u64, max_bytes: usize) -> EngineResult<LogBatch> {
        let shared = &self.inner.shared;
        let latest = shared.versions.load().revision;
        let mut batch = LogBatch {
            last: after,
            latest,
            ..LogBatch::default()
        };
        if after >= latest {
            return Ok(batch);
        }
        let Some(durable) = &shared.durable else {
            return Err(EngineError::LogTruncated { oldest: latest });
        };
        // Segments are named after their first revision: those that may hold `after + 1`
        // or later, in order, archived ones first.
        let mut segments = Vec::new();
        for dir in [
            wal::archive_dir(durable.root()),
            wal::wal_dir(durable.root()),
        ] {
            if dir.is_dir() {
                segments.extend(wal::list_segments(&dir)?);
            }
        }
        segments.sort_by_key(|(first, _)| *first);
        segments.dedup_by_key(|(first, _)| *first);
        let start = segments
            .iter()
            .rposition(|(first, _)| *first <= after + 1)
            .unwrap_or(0);
        let mut expected = after + 1;
        let mut oldest = None;
        'segments: for (_, path) in &segments[start..] {
            // The active segment may grow while it is read: its complete records count.
            let contents = wal::read_segment(path)?;
            for record in contents.records {
                oldest.get_or_insert(record.revision);
                if record.revision < expected {
                    continue;
                }
                if record.revision != expected {
                    break 'segments;
                }
                let before = batch.frames.len();
                encode_record(&record, &mut batch.frames)?;
                if batch.records > 0 && batch.frames.len() > max_bytes {
                    batch.frames.truncate(before);
                    break 'segments;
                }
                batch.records += 1;
                batch.last = record.revision;
                expected += 1;
            }
        }
        if batch.records == 0 {
            return Err(EngineError::LogTruncated {
                oldest: oldest.map_or(latest, |o| o.saturating_sub(1)),
            });
        }
        Ok(batch)
    }

    /// Applies records of a primary's log ([`Engine::log_since`]) in order, each as one
    /// commit with the primary's revision and term ids; records at or below this engine's
    /// revision are skipped. Returns the revision reached. A record that doesn't follow
    /// (a gap) or whose dictionary doesn't continue this one's is refused
    /// ([`EngineError::Corruption`]): the replica starts again from an image.
    pub fn apply_log(&self, frames: &[u8]) -> EngineResult<u64> {
        let _slot = self.inner.writer.lock();
        let shared = &self.inner.shared;
        let mut pos = 0;
        let mut revision = shared.versions.load().revision;
        while pos < frames.len() {
            let record = match decode_frame(&frames[pos..]) {
                Frame::Record(record, used) => {
                    pos += used;
                    record
                }
                Frame::End => break,
                Frame::Invalid => {
                    return Err(EngineError::Corruption(format!(
                        "an invalid log record at byte {pos} of a replicated batch"
                    )));
                }
            };
            if record.revision <= revision {
                continue;
            }
            if record.revision != revision + 1 {
                return Err(EngineError::Corruption(format!(
                    "replicated record {} doesn't follow revision {revision}",
                    record.revision
                )));
            }
            self.apply_record(&record)?;
            revision = record.revision;
        }
        Ok(revision)
    }

    /// One record as a commit (the writer slot held).
    fn apply_record(&self, record: &CommitRecord) -> EngineResult<()> {
        let shared = &self.inner.shared;
        let dictionary_len = shared.dictionary.len();
        if record.dictionary_start > dictionary_len {
            return Err(EngineError::Corruption(format!(
                "replicated record {} starts the dictionary at {}, the replica has {dictionary_len}",
                record.revision, record.dictionary_start
            )));
        }
        for (offset, key) in record.keys.iter().enumerate() {
            shared
                .dictionary
                .restore_key(record.dictionary_start + offset as u64, key)?;
        }
        let dictionary_len = shared.dictionary.len();
        let latest = shared.versions.load();
        let converting = latest.asserted.layout() == Layout::DefaultGraph && record.names_a_graph();
        let _compaction = converting.then(|| shared.versions.compaction_slot.lock());
        let converted = converting.then(|| shared.versions.load().asserted.with_quads_layout());
        let asserted_layout = converted
            .as_ref()
            .map_or(latest.asserted.layout(), |index| index.layout());
        drop(latest);
        let [asserted_run, inferred_run] = record.runs(asserted_layout);
        let revision = record.revision;
        // Equality classes are found again where `sameAs` statements changed.
        let same_as = *shared.equality.read();
        let changes_equality = same_as.is_some_and(|same_as| {
            record
                .inserts
                .iter()
                .chain(&record.deletes)
                .any(|q| q.predicate == same_as)
                || record
                    .inferred_inserts
                    .iter()
                    .chain(&record.inferred_deletes)
                    .any(|t| t.predicate == same_as)
        });
        let publish = move |current: &Version| {
            let asserted = converted.as_ref().unwrap_or(&current.asserted);
            Version {
                asserted: asserted.with_run(asserted_run),
                inferred: current.inferred.with_run(inferred_run),
                revision,
                dictionary_len,
                equality: match changes_equality {
                    true => Default::default(),
                    false => std::sync::Arc::clone(&current.equality),
                },
            }
        };
        let wal_bytes = match &shared.durable {
            Some(durable) => {
                let mut wal = durable.wal.lock();
                wal.append(record)?;
                shared.versions.publish(publish);
                wal.bytes_since_checkpoint()
            }
            None => {
                shared.versions.publish(publish);
                0
            }
        };
        drop(_compaction);
        self.inner.after_commit(wal_bytes);
        Ok(())
    }
}
