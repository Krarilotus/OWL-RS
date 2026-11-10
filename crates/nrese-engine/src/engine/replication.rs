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

use super::Engine;
use crate::durability::codec::{CommitRecord, Frame, decode_frame, encode_record};
use crate::durability::wal;
use crate::error::{EngineError, EngineResult};

/// How often [`open_segments`] lists the segments again when one moved (archived) or went
/// (released) between listing and opening.
const OPEN_ATTEMPTS: usize = 3;

/// The log segments of the store at `root` that may hold `after + 1` or later, in order,
/// archived ones first, each open. Segments are named after their first revision. `None`
/// when they kept moving through every attempt: the log is being cut faster than it is read.
fn open_segments(
    root: &std::path::Path,
    after: u64,
) -> EngineResult<Option<Vec<(std::path::PathBuf, std::fs::File)>>> {
    let mut attempt = 0;
    'list: loop {
        attempt += 1;
        let mut segments = Vec::new();
        for dir in [wal::archive_dir(root), wal::wal_dir(root)] {
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
        #[cfg(test)]
        tests::between_listing_and_opening();
        let mut open = Vec::with_capacity(segments.len() - start);
        for (_, path) in segments.drain(start..) {
            match std::fs::File::open(&path) {
                Ok(file) => open.push((path, file)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match attempt < OPEN_ATTEMPTS {
                        true => continue 'list,
                        false => return Ok(None),
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        return Ok(Some(open));
    }
}

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

impl LogBatch {
    /// What this batch, the records after `after`, says it carries.
    pub fn expectation(&self, after: u64) -> LogExpectation {
        LogExpectation {
            after,
            records: self.records as u64,
            last: self.last,
        }
    }
}

/// What a log answer says it carries ([`Engine::apply_log`]): the records after revision
/// `after`, `records` of them, the last at revision `last` (`after` without records).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogExpectation {
    pub after: u64,
    pub records: u64,
    pub last: u64,
}

/// What [`Engine::apply_log`] did: the revision it reached, and whether it stopped before
/// the answer's last record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub revision: u64,
    pub stopped: bool,
}

impl Engine {
    /// The committed records after revision `after`, from the write-ahead log and its
    /// archive, as encoded frames: at least one record if there is any, then more while
    /// they fit in `max_bytes`. Empty when `after` is the latest revision.
    /// [`EngineError::LogTruncated`] if the log no longer holds the record after `after`
    /// (a checkpoint covered it, or a bulk load or rematerialisation wrote none); an
    /// in-memory engine has no log at all.
    ///
    /// Only published records go: the batch stops at the revision published when it was
    /// asked for, though the log may already hold a later complete record whose commit
    /// is still syncing (or failed to). The segments are opened before they are read, so
    /// that the log releasing or archiving one meanwhile doesn't take it away.
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
        // Nothing leaves the process before the store's names are durable (deferred-sync
        // design note of 8 Nov): a follower must never hold what a power loss here could undo.
        durable.settle_names()?;
        // Segments that kept moving: the follower starts again from an image, as for a log a
        // checkpoint has covered.
        let Some(segments) = open_segments(durable.root(), after)? else {
            return Err(EngineError::LogTruncated { oldest: latest });
        };
        let mut expected = after + 1;
        let mut oldest = None;
        'segments: for (path, mut file) in segments {
            // The active segment may grow while it is read: its complete records count,
            // up to `latest`.
            let contents = wal::read_open_segment(&mut file, &path)?;
            for record in contents.records {
                oldest.get_or_insert(record.revision);
                if record.revision < expected {
                    continue;
                }
                if record.revision != expected || record.revision > latest {
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

    /// Applies a primary's log answer ([`Engine::log_since`]) that says it carries
    /// `expected`: its records in order, each as one commit with the primary's revision and
    /// term ids.
    ///
    /// The whole answer is checked first, under the writer slot, with nothing published:
    /// it follows a revision this engine has reached (its records up to this engine's
    /// revision are skipped, as an answer applied twice); the frames are complete records,
    /// nothing else; their revisions are exactly `after + 1 ..= last`, `records` of them;
    /// their dictionary entries follow on from each other; their commit times are ones a
    /// store keeps ([`crate::durability::check_commit_time`]). Then the batch's dictionary
    /// entries are restored at once, all or none, each checked under that lock as
    /// `Dictionary::restore_key` takes one (continuing this dictionary; a speculative
    /// transaction may have interned meanwhile). Anything else is refused
    /// ([`EngineError::Corruption`]) with the engine as it was: the replica starts again
    /// from an image. Then the records are published one by one. `stop` is checked before each:
    /// once it stops, no further record is published ([`Applied::stopped`]). A local
    /// failure on a record (the log's write) leaves the records before it published.
    ///
    /// On a Rayon pool thread while the writer slot is held: [`EngineError::SlotBusy`]
    /// (the contract of [`Engine::transaction`]).
    pub fn apply_log(
        &self,
        frames: &[u8],
        expected: &LogExpectation,
        stop: &nrese_exec::stop::StopSource,
    ) -> EngineResult<Applied> {
        let _slot = super::admit(&self.inner.writer, "writer")?;
        let shared = &self.inner.shared;
        let records = self.checked_batch(frames, expected)?;
        let keys: Vec<&[u8]> = records
            .iter()
            .flat_map(|record| record.keys.iter().map(Vec::as_slice))
            .collect();
        let mut applied = Applied {
            revision: shared.versions.load().revision,
            stopped: false,
        };
        let Some(first) = records.first() else {
            return Ok(applied);
        };
        // What the published version covers: entries past it (a stopped batch's tail, a
        // speculative transaction's terms) are in memory but not published, and stay so
        // until a record that holds them is.
        let mut published = shared.versions.load().dictionary_len;
        shared
            .dictionary
            .restore_keys_checked(first.dictionary_start, &keys)?;
        // Records this engine already has (an answer applied again) are skipped.
        let reached = applied.revision;
        for record in records.iter().filter(|record| record.revision > reached) {
            #[cfg(test)]
            tests::before_record(record.revision);
            if stop.check().is_err() {
                applied.stopped = true;
                break;
            }
            // Within u64: `checked_batch` refused any range that isn't.
            published = published.max(record.dictionary_start + record.keys.len() as u64);
            self.apply_record(record, published)?;
            applied.revision = record.revision;
        }
        Ok(applied)
    }

    /// The records of `frames` once they are what `expected` says, in order; nothing
    /// changed (the writer slot held).
    fn checked_batch(
        &self,
        frames: &[u8],
        expected: &LogExpectation,
    ) -> EngineResult<Vec<CommitRecord>> {
        let shared = &self.inner.shared;
        let refuse =
            |why: String| Err(EngineError::Corruption(format!("a replicated batch {why}")));
        let revision = shared.versions.load().revision;
        // An answer from before this engine's revision may come again (its records up to
        // here are skipped); one after it would leave a gap.
        if expected.after > revision {
            return refuse(format!(
                "follows revision {}, this engine is at {revision}",
                expected.after
            ));
        }
        let mut records = Vec::new();
        let mut pos = 0;
        loop {
            match decode_frame(&frames[pos..]) {
                Frame::Record(record, used) => {
                    pos += used;
                    records.push(record);
                }
                Frame::End => break,
                Frame::Invalid => {
                    return refuse(format!("holds an invalid log record at byte {pos}"));
                }
            }
        }
        if records.len() as u64 != expected.records {
            return refuse(format!(
                "holds {} records, its answer says {}",
                records.len(),
                expected.records
            ));
        }
        for (offset, record) in records.iter().enumerate() {
            // Revisions from the wire are compared, not added up past u64.
            let Some(at) = expected.after.checked_add(1 + offset as u64) else {
                return refuse(format!(
                    "has record {} past the last revision there is",
                    record.revision
                ));
            };
            if record.revision != at {
                return refuse(format!(
                    "has record {} where revision {at} goes",
                    record.revision
                ));
            }
        }
        let last = records
            .last()
            .map_or(expected.after, |record| record.revision);
        if last != expected.last {
            return refuse(format!(
                "ends at revision {last}, its answer says {}",
                expected.last
            ));
        }
        // Each record's dictionary range ends within u64 (a range from the wire is refused,
        // not added up): checked before anything below adds to it. Its commit time is one a
        // store keeps: refused here, a later record's never leaves an earlier one applied.
        for record in &records {
            crate::durability::check_commit_time(record)?;
            if record
                .dictionary_start
                .checked_add(record.keys.len() as u64)
                .is_none()
            {
                return refuse(format!(
                    "has record {} whose dictionary entries end past u64",
                    record.revision
                ));
            }
        }
        // A primary's records continue each other's dictionary entries.
        for pair in records.windows(2) {
            let ends = pair[0].dictionary_start + pair[0].keys.len() as u64;
            if pair[1].dictionary_start != ends {
                return refuse(format!(
                    "record {} starts the dictionary at {}, the record before ends at {ends}",
                    pair[1].revision, pair[1].dictionary_start
                ));
            }
        }
        Ok(records)
    }

    /// One record as a commit, its dictionary entries already restored and the dictionary
    /// published at `dictionary_len` entries (the writer slot held).
    fn apply_record(&self, record: &CommitRecord, dictionary_len: u64) -> EngineResult<()> {
        let shared = &self.inner.shared;
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
        self.inner
            .publish_record(record, false, None, dictionary_len, changes_equality, None)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use crate::{DurabilityConfig, Engine, EngineConfig, EngineError, SyncPolicy};

    thread_local! {
        /// What a test does between the feed's listing of the segments and its opening them.
        static BETWEEN: RefCell<Option<Box<dyn FnMut()>>> = RefCell::new(None);
    }

    thread_local! {
        /// What a test does before a replicated record is published (its revision).
        static BEFORE_RECORD: RefCell<Option<BeforeRecord>> = RefCell::new(None);
    }

    /// What a test does before a record is published, given its revision.
    type BeforeRecord = Box<dyn FnMut(u64)>;

    pub(super) fn before_record(revision: u64) {
        BEFORE_RECORD.with(|before| {
            if let Some(act) = before.borrow_mut().as_mut() {
                act(revision);
            }
        });
    }

    pub(super) fn between_listing_and_opening() {
        BETWEEN.with(|between| {
            if let Some(act) = between.borrow_mut().as_mut() {
                act();
            }
        });
    }

    /// A durable engine with three commits (each in the active segment).
    fn three_commits(dir: &std::path::Path) -> Engine {
        let config = EngineConfig {
            background_maintenance: false,
            durability: DurabilityConfig {
                sync: SyncPolicy::EveryCommit,
                ..DurabilityConfig::default()
            },
            ..EngineConfig::default()
        };
        let engine = Engine::open(dir, config).unwrap();
        for n in 0..3 {
            let mut tx = engine.transaction();
            let quad = nrese_rdf::Quad::new(
                nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/s{n}")),
                nrese_rdf::NamedNode::new_unchecked("http://example.com/p"),
                nrese_rdf::NamedNode::new_unchecked("http://example.com/o"),
                nrese_rdf::GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
            tx.commit().unwrap();
        }
        engine
    }

    /// Segments that move between every listing and opening (the log cut faster than the
    /// feed reads it) send the follower back to an image, as a truncated log does: not an
    /// error of the server's.
    #[test]
    fn segments_that_keep_moving_are_a_truncated_log() {
        let dir = tempfile::tempdir().unwrap();
        let engine = three_commits(dir.path());
        let (wal, archive) = (
            super::wal::wal_dir(dir.path()),
            super::wal::archive_dir(dir.path()),
        );
        std::fs::create_dir_all(&archive).unwrap();
        BETWEEN.with(|between| {
            *between.borrow_mut() = Some(Box::new(move || {
                // Whatever was listed moves: from the log to the archive, or back.
                for (from, to) in [(&wal, &archive), (&archive, &wal)] {
                    let listed = super::wal::list_segments(from).unwrap();
                    if !listed.is_empty() {
                        for (_, path) in listed {
                            std::fs::rename(&path, to.join(path.file_name().unwrap())).unwrap();
                        }
                        return;
                    }
                }
            }));
        });
        let result = engine.log_since(0, 1 << 20);
        BETWEEN.with(|between| *between.borrow_mut() = None);
        assert!(
            matches!(result, Err(EngineError::LogTruncated { .. })),
            "{result:?}"
        );
    }

    /// A segment archived after the feed listed it and before it opened it is found in
    /// the archive.
    #[test]
    fn a_segment_archived_while_the_feed_lists_is_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let engine = three_commits(dir.path());
        let (wal, archive) = (
            super::wal::wal_dir(dir.path()),
            super::wal::archive_dir(dir.path()),
        );
        let mut once = true;
        BETWEEN.with(|between| {
            *between.borrow_mut() = Some(Box::new(move || {
                if std::mem::take(&mut once) {
                    std::fs::create_dir_all(&archive).unwrap();
                    for (_, path) in super::wal::list_segments(&wal).unwrap() {
                        std::fs::rename(&path, archive.join(path.file_name().unwrap())).unwrap();
                    }
                }
            }));
        });
        let batch = engine.log_since(0, 1 << 20).unwrap();
        BETWEEN.with(|between| *between.borrow_mut() = None);
        assert_eq!((batch.records, batch.last), (3, 3));
    }

    fn durable() -> EngineConfig {
        EngineConfig {
            background_maintenance: false,
            durability: DurabilityConfig {
                sync: SyncPolicy::EveryCommit,
                ..DurabilityConfig::default()
            },
            ..EngineConfig::default()
        }
    }

    /// Commit `n`: one statement with a subject of its own (a new dictionary entry).
    fn commit(engine: &Engine, n: u64) {
        let mut tx = engine.transaction();
        let quad = nrese_rdf::Quad::new(
            nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/s{n}")),
            nrese_rdf::NamedNode::new_unchecked("http://example.com/p"),
            nrese_rdf::NamedNode::new_unchecked("http://example.com/o"),
            nrese_rdf::GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
        tx.commit().unwrap();
    }

    /// A primary at revision 4 and an image of it at revision 1 in `image`; the primary's
    /// log after revision 1.
    fn primary_and_image(primary: &std::path::Path, image: &std::path::Path) -> super::LogBatch {
        let engine = Engine::open(primary, durable()).unwrap();
        commit(&engine, 0);
        engine.write_image(image).unwrap();
        for n in 1..4 {
            commit(&engine, n);
        }
        let batch = engine.log_since(1, 1 << 20).unwrap();
        assert_eq!((batch.records, batch.last), (3, 4));
        batch
    }

    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            match entry.file_type().unwrap().is_dir() {
                true => copy_dir(&entry.path(), &target),
                false => drop(std::fs::copy(entry.path(), &target).unwrap()),
            }
        }
    }

    fn decoded(frames: &[u8]) -> Vec<super::CommitRecord> {
        let mut records = Vec::new();
        let mut pos = 0;
        while let super::Frame::Record(record, used) = super::decode_frame(&frames[pos..]) {
            pos += used;
            records.push(record);
        }
        records
    }

    fn encoded(records: &[super::CommitRecord]) -> Vec<u8> {
        let mut frames = Vec::new();
        for record in records {
            super::encode_record(record, &mut frames).unwrap();
        }
        frames
    }

    /// A stop raised before record 3 of an answer of 2 to 4: record 2 is published, 3 and 4
    /// aren't, now or after a reopen (review p2-b7-syn69 r1, R1-F2: stop at whole records).
    #[test]
    fn a_stop_before_a_record_publishes_no_further_record() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let (primary, image) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let batch = primary_and_image(primary.path(), image.path());
        let replica = Engine::open(image.path(), durable()).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let stop = nrese_exec::stop::StopSource::new(Some(Arc::clone(&flag)), None);
        let raise = Arc::clone(&flag);
        BEFORE_RECORD.with(|before| {
            *before.borrow_mut() = Some(Box::new(move |revision| {
                if revision == 3 {
                    raise.store(true, Ordering::Release);
                }
            }));
        });
        let applied = replica.apply_log(&batch.frames, &batch.expectation(1), &stop);
        BEFORE_RECORD.with(|before| *before.borrow_mut() = None);
        let expected = super::Applied {
            revision: 2,
            stopped: true,
        };
        assert_eq!(applied.unwrap(), expected);
        assert_eq!(replica.stats().revision, 2);
        drop(replica);
        let reopened = Engine::open(image.path(), durable()).unwrap();
        assert_eq!(reopened.stats().revision, 2);
    }

    /// An answer that isn't what it says is refused before anything is published: the
    /// replica keeps its revision and dictionary, also after a reopen (review p2-b7-syn69 r1,
    /// R1-F1). Guard (passes before the check too): an answer that is what it says applies.
    /// Every row is checked.
    #[test]
    fn an_answer_that_is_not_what_it_says_changes_nothing() {
        let (primary, image) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let batch = primary_and_image(primary.path(), image.path());
        let all = decoded(&batch.frames);
        assert_eq!(all.len(), 3);
        let expect = |records, last| super::LogExpectation {
            after: 1,
            records,
            last,
        };
        let mut discontinuous = decoded(&batch.frames);
        discontinuous[1].dictionary_start += 1;
        // Review r2 S1: a record with no keys of its own, its range past the dictionary.
        let mut keyless = decoded(&batch.frames);
        keyless.truncate(1);
        keyless[0].keys.clear();
        keyless[0].dictionary_start += 1;
        // Fix check r2 R2-F1: a range whose end is past u64 (start u64::MAX, one key), then a
        // keyless record starting at 0.
        let mut past_u64 = decoded(&batch.frames);
        past_u64.truncate(2);
        past_u64[0].dictionary_start = u64::MAX;
        past_u64[0].keys.truncate(1);
        past_u64[1].dictionary_start = 0;
        past_u64[1].keys.clear();
        let mut invalid_suffix = encoded(&all[..1]);
        invalid_suffix.extend_from_slice(b"not a record at all");
        // DUR-2 fix check r3: a valid record, then one whose commit time no store keeps
        // (`durability::MAX_COMMIT_TIME`).
        let mut past_time = decoded(&batch.frames);
        past_time.truncate(2);
        past_time[1].committed_micros = Some(1 << 63);
        let later = super::LogExpectation {
            after: 2,
            records: 2,
            last: 4,
        };
        let rows: [(&str, Vec<u8>, super::LogExpectation); 8] = [
            (
                "a commit time past its range after a valid record",
                encoded(&past_time),
                expect(2, 3),
            ),
            (
                "a dictionary range ending past u64",
                encoded(&past_u64),
                expect(2, 3),
            ),
            (
                "a keyless record past the dictionary's end",
                encoded(&keyless),
                expect(1, 2),
            ),
            (
                "more records than it says",
                batch.frames.clone(),
                expect(2, 3),
            ),
            (
                "fewer records than it says",
                encoded(&all[..2]),
                expect(3, 4),
            ),
            (
                "an invalid record after a valid one",
                invalid_suffix,
                expect(2, 3),
            ),
            (
                "a record not continuing the dictionary",
                encoded(&discontinuous),
                expect(3, 4),
            ),
            (
                "records after a revision not reached",
                encoded(&all[1..]),
                later,
            ),
        ];
        let stop = nrese_exec::stop::StopSource::none();
        let mut wrong = Vec::new();
        for (index, (row, frames, expected)) in rows.into_iter().enumerate() {
            let dir = primary.path().join(format!("replica-{index}"));
            copy_dir(image.path(), &dir);
            let replica = Engine::open(&dir, durable()).unwrap();
            let before = (replica.stats().revision, replica.stats().dictionary.terms);
            // A panic is this row's failure, not the whole table's.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                replica.apply_log(&frames, &expected, &stop)
            }))
            .unwrap_or_else(|_| Err(EngineError::Configuration("panicked".to_owned())));
            if !matches!(result, Err(EngineError::Corruption(_))) {
                wrong.push(format!("{row}: {result:?}"));
            }
            let after = (replica.stats().revision, replica.stats().dictionary.terms);
            if after != before {
                wrong.push(format!(
                    "{row}: (revision, terms) {before:?} became {after:?}"
                ));
            }
            drop(replica);
            let reopened = Engine::open(&dir, durable()).unwrap();
            let again = (reopened.stats().revision, reopened.stats().dictionary.terms);
            if again != before {
                wrong.push(format!("{row}: after a reopen {again:?}, not {before:?}"));
            }
        }
        let dir = primary.path().join("replica-guard");
        copy_dir(image.path(), &dir);
        let replica = Engine::open(&dir, durable()).unwrap();
        let applied = replica.apply_log(&batch.frames, &batch.expectation(1), &stop);
        if !matches!(
            applied,
            Ok(super::Applied {
                revision: 4,
                stopped: false
            })
        ) {
            wrong.push(format!("an answer that is what it says: {applied:?}"));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A stopped batch's unpublished tail stays unpublished (review r2 S3): records 2 to 4
    /// stopped before 3, then record 3 alone applied, checkpointed and reopened, hold the
    /// dictionary of a replica that applied 2 and 3 without a stop: none of record 4's
    /// entries.
    #[test]
    fn a_stopped_batch_s_tail_is_not_published_by_a_later_record() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let (primary, image) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let batch = primary_and_image(primary.path(), image.path());
        let all = decoded(&batch.frames);
        let terms_after_reopen = |dir: &std::path::Path, engine: Engine| {
            engine.checkpoint().unwrap();
            drop(engine);
            let reopened = Engine::open(dir, durable()).unwrap();
            (reopened.stats().revision, reopened.stats().dictionary.terms)
        };
        // Stopped before record 3, then record 3 alone.
        let stopped_dir = primary.path().join("stopped");
        copy_dir(image.path(), &stopped_dir);
        let stopped = Engine::open(&stopped_dir, durable()).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let stop = nrese_exec::stop::StopSource::new(Some(Arc::clone(&flag)), None);
        let raise = Arc::clone(&flag);
        BEFORE_RECORD.with(|before| {
            *before.borrow_mut() = Some(Box::new(move |revision| {
                if revision == 3 {
                    raise.store(true, Ordering::Release);
                }
            }));
        });
        let first = stopped.apply_log(&batch.frames, &batch.expectation(1), &stop);
        BEFORE_RECORD.with(|before| *before.borrow_mut() = None);
        assert!(
            matches!(
                first,
                Ok(super::Applied {
                    revision: 2,
                    stopped: true
                })
            ),
            "{first:?}"
        );
        let none = nrese_exec::stop::StopSource::none();
        let three = super::LogExpectation {
            after: 2,
            records: 1,
            last: 3,
        };
        let second = stopped.apply_log(&encoded(&all[1..2]), &three, &none);
        assert!(
            matches!(
                second,
                Ok(super::Applied {
                    revision: 3,
                    stopped: false
                })
            ),
            "{second:?}"
        );
        let held = terms_after_reopen(&stopped_dir, stopped);
        // 2 and 3 without a stop.
        let clean_dir = primary.path().join("clean");
        copy_dir(image.path(), &clean_dir);
        let clean = Engine::open(&clean_dir, durable()).unwrap();
        let two_three = super::LogExpectation {
            after: 1,
            records: 2,
            last: 3,
        };
        clean
            .apply_log(&encoded(&all[..2]), &two_three, &none)
            .unwrap();
        let expected = terms_after_reopen(&clean_dir, clean);
        assert_eq!(held, expected);
    }

    /// An answer whose revisions run past u64 is refused, not added up (SYN-69 r3 sol-loop
    /// N1): a replica at revision u64::MAX - 1 (as a restored image can be; here its
    /// published revision is raised in memory) given records at revisions u64::MAX and 0,
    /// `last` 0. Refused as corruption with the replica as it was; nothing reaches its log
    /// (the reopened replica is at its logged revision, its dictionary unchanged).
    #[test]
    fn an_answer_whose_revisions_run_past_u64_is_refused() {
        let (primary, image) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let batch = primary_and_image(primary.path(), image.path());
        let replica = Engine::open(image.path(), durable()).unwrap();
        let logged = (replica.stats().revision, replica.stats().dictionary.terms);
        let high = u64::MAX - 1;
        replica
            .inner
            .shared
            .versions
            .publish(|current| super::super::Version {
                asserted: current.asserted.clone(),
                inferred: current.inferred.clone(),
                revision: high,
                committed_micros: current.committed_micros,
                dictionary_len: current.dictionary_len,
                equality: std::sync::Arc::clone(&current.equality),
                content: current.content,
                asserted_content: current.asserted_content,
                canonical: Default::default(),
                own: Default::default(),
            });
        let mut records = decoded(&batch.frames);
        records.truncate(2);
        records[0].revision = u64::MAX;
        records[1].revision = 0;
        let expected = super::LogExpectation {
            after: high,
            records: 2,
            last: 0,
        };
        let stop = nrese_exec::stop::StopSource::none();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            replica.apply_log(&encoded(&records), &expected, &stop)
        }));
        assert!(
            matches!(result, Ok(Err(EngineError::Corruption(_)))),
            "{:?}",
            result.as_ref().map_err(|_| "panicked")
        );
        assert_eq!(replica.stats().revision, high);
        assert_eq!(replica.stats().dictionary.terms, logged.1);
        drop(replica);
        let reopened = Engine::open(image.path(), durable()).unwrap();
        assert_eq!(
            (reopened.stats().revision, reopened.stats().dictionary.terms),
            logged
        );
    }
}
