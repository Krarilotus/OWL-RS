//! The commit point of a bulk load and the log's rotation (EEV-01, B-301): a load or
//! rotation that fails at a durable step never costs a commit acknowledged after it.

use std::cell::Cell;
use std::collections::HashSet;
use std::thread::LocalKey;

use nrese_rdf::{GraphName, NamedNode, Quad};

use super::fault;
use crate::{
    BulkMode, CommitSummary, DurabilityConfig, Engine, EngineConfig, EngineError, EngineResult,
    QuadPattern, SyncPolicy,
};

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            sync: SyncPolicy::EveryCommit,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    }
}

fn quad(n: u64) -> Quad {
    let node = |name: String| NamedNode::new_unchecked(format!("http://example.com/{name}"));
    Quad::new(
        node(format!("s{n}")),
        node("p".into()),
        node(format!("o{n}")),
        GraphName::DefaultGraph,
    )
}

fn contents(engine: &Engine) -> HashSet<Quad> {
    let snapshot = engine.snapshot();
    snapshot
        .quads_for_pattern(&QuadPattern::all())
        .map(|q| snapshot.decode_quad(q).unwrap())
        .collect()
}

/// One row of [`a_failed_bulk_load_never_costs_a_later_acknowledged_commit`]: what went
/// wrong, if anything.
fn bulk_load_row(point: &'static LocalKey<Cell<bool>>, mode: BulkMode) -> Vec<String> {
    let mut wrong = Vec::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    let mut tx = engine.transaction();
    tx.insert(quad(1).as_ref());
    tx.commit().unwrap();

    let load = engine.bulk_load(mode);
    load.add(&[quad(10), quad(11)]);
    point.set(true);
    let loaded = load.finish();
    point.set(false);
    let mut tx = engine.transaction();
    tx.insert(quad(2).as_ref());
    let later = tx.commit();
    drop(engine);

    let engine = Engine::open(dir.path(), config()).unwrap();
    let held = contents(&engine);
    if later.is_ok() && !held.contains(&quad(2)) {
        wrong.push(format!(
            "the acknowledged commit is lost (load: {:?})",
            loaded.as_ref().err()
        ));
    }
    match (&loaded, &later) {
        (Ok(_), _) if !held.contains(&quad(10)) => wrong.push("the load is lost".into()),
        // Reported failed while the engine went on taking commits: it must not be there.
        (Err(error), Ok(_)) if held.contains(&quad(10)) => wrong.push(format!(
            "a load that failed ({error}) is there after a restart"
        )),
        _ => {}
    }
    wrong
}

/// A store at revision 1 with its record in the log; a bulk load fails at a durable step (an
/// append builds its version in memory, a replacement streams it into the checkpoint); then
/// a transaction commits. After a restart, the transaction is there if it was acknowledged;
/// the load is there if it succeeded, and absent if it failed while the engine went on
/// taking commits (a failure whose outcome is unknown takes no more: then either is right).
/// Before the fix, the load's checkpoint of revision 2 was in place when it failed, the
/// transaction was acknowledged as revision 2 too, and the restart skipped it as covered by
/// that checkpoint. Each row is judged on its own. The mapping rows are guards, right on
/// the base too: a built load is then served from memory, and a streamed one removed its
/// checkpoint (the base fails only if that removal fails as well).
#[test]
fn a_failed_bulk_load_never_costs_a_later_acknowledged_commit() {
    let points: [(&'static LocalKey<Cell<bool>>, &str); 3] = [
        (&fault::FAIL_ROTATE, "rotation"),
        (&fault::FAIL_DIR_SYNC, "directory sync"),
        (&fault::FAIL_MAP, "mapping (guard)"),
    ];
    let mut failures = Vec::new();
    for (point, step) in points {
        for mode in [BulkMode::Append, BulkMode::Replace] {
            let wrong = bulk_load_row(point, mode);
            let verdict = if wrong.is_empty() { "ok" } else { "FAILED" };
            eprintln!("row {step}, {mode:?}: {verdict}");
            failures.extend(wrong.into_iter().map(|w| format!("{step}, {mode:?}: {w}")));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

fn small_segments() -> EngineConfig {
    EngineConfig {
        durability: DurabilityConfig {
            // Every commit after the first starts a segment.
            wal_segment_bytes: 1,
            ..config().durability
        },
        ..config()
    }
}

fn commit(engine: &Engine, n: u64) -> EngineResult<CommitSummary> {
    let mut tx = engine.transaction();
    tx.insert(quad(n).as_ref());
    tx.commit()
}

/// Guard (right on the base too): a rotation that fails before it changes anything leaves
/// the log as it was. The commit fails, the next succeeds, and no file is left behind.
#[test]
fn a_rotation_that_fails_before_its_segment_is_placed_leaves_the_log_usable() {
    let files = |dir: &std::path::Path| {
        let mut names: Vec<String> = std::fs::read_dir(super::wal::wal_dir(dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), small_segments()).unwrap();
    commit(&engine, 1).unwrap();
    let before = files(dir.path());
    fault::FAIL_ROTATE.set(true);
    assert!(commit(&engine, 2).is_err());
    fault::FAIL_ROTATE.set(false);
    assert_eq!(files(dir.path()), before, "nothing left behind");
    commit(&engine, 2).unwrap();
    drop(engine);
    let engine = Engine::open(dir.path(), small_segments()).unwrap();
    assert_eq!(contents(&engine), [quad(1), quad(2)].into_iter().collect());
}

/// One time-restore row: commits of `quad(0)`, `quad(1)`, … made at the clock times given, a
/// checkpoint after the first `checkpointed` of them (0: none), then an open restoring the
/// commits made up to `until`. What the restored store holds, or `None` if the open refused.
fn time_restore(times: &[u64], checkpointed: usize, until: u64) -> Option<HashSet<Quad>> {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        for (n, &at) in times.iter().enumerate() {
            fault::CLOCK.set(Some(at));
            commit(&engine, n as u64).unwrap();
            if n + 1 == checkpointed {
                engine.checkpoint().unwrap();
            }
        }
        fault::CLOCK.set(None);
    }
    let restore = EngineConfig {
        durability: DurabilityConfig {
            recover_until_micros: Some(until),
            ..config().durability
        },
        ..config()
    };
    Engine::open(dir.path(), restore)
        .ok()
        .map(|engine| contents(&engine))
}

/// A time restore never serves a commit made after its time (EEV-06): it holds exactly the
/// commits made up to it, or refuses. Before the fix, a checkpoint made after the time was
/// used whole: its commits after the time came back. Rows:
/// - an image after the time, the log empty: must refuse (it can't hold less);
/// - a clock stepped back before the image (commits at 1000, then 500): the second commit
///   was made after the first, so the image is after 750 too, though its own clock said
///   500; commit times never going back makes it refuse;
/// - guards (right on the base too): an image before the time with the log across it, and
///   an image whose last commit is at the time exactly, both restore what was made by then.
///
/// Each row is judged on its own.
#[test]
fn a_time_restore_never_serves_commits_made_after_its_time() {
    let held = |n: u64| (0..n).map(quad).collect::<HashSet<_>>();
    // (row, times, checkpointed after, until, what it may hold: None = refuse only)
    type Row<'a> = (&'a str, &'a [u64], usize, u64, Option<HashSet<Quad>>);
    let rows: [Row; 4] = [
        ("an image after the time", &[1000, 2000], 2, 1500, None),
        (
            "a clock stepped back before the image",
            &[1000, 500],
            2,
            750,
            None,
        ),
        (
            "an image before the time, the log across it (guard)",
            &[1000, 2000, 3000],
            1,
            2500,
            Some(held(2)),
        ),
        (
            "an image at the time exactly (guard)",
            &[1000, 2000],
            2,
            2000,
            Some(held(2)),
        ),
    ];
    let mut failures = Vec::new();
    for (row, times, checkpointed, until, expected) in rows {
        let restored = time_restore(times, checkpointed, until);
        let ok = restored == expected;
        eprintln!("row {row}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{row}: held {restored:?}, expected {expected:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A store's commit times never go back and are never 0 (a checkpoint's "unknown"), whatever
/// the clock does: commits under a clock reading 1000, then 500, then 0 (before 1970) are
/// logged at 1000, 1000 and 1000. Before the fix they were logged at the clock's times.
#[test]
fn commit_times_never_go_back() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        for (n, at) in [1000, 500, 0].into_iter().enumerate() {
            fault::CLOCK.set(Some(at));
            commit(&engine, n as u64).unwrap();
        }
        fault::CLOCK.set(None);
    }
    let mut times = Vec::new();
    for (_, path) in super::wal::list_segments(&super::wal::wal_dir(dir.path())).unwrap() {
        let contents = super::wal::read_segment(&path).unwrap();
        times.extend(
            contents
                .records
                .iter()
                .map(|record| record.committed_micros),
        );
    }
    assert_eq!(times, [Some(1000), Some(1000), Some(1000)]);
}

/// Opens `dir` restoring the commits made up to `until`: what it holds, or `None` if it
/// refused.
fn restored(dir: &std::path::Path, until: u64) -> Option<HashSet<Quad>> {
    let restore = EngineConfig {
        durability: DurabilityConfig {
            recover_until_micros: Some(until),
            ..config().durability
        },
        ..config()
    };
    Engine::open(dir, restore)
        .ok()
        .map(|engine| contents(&engine))
}

/// The commit times of the log's records in `dir`, in order.
fn logged_times(dir: &std::path::Path) -> Vec<Option<u64>> {
    let mut times = Vec::new();
    for (_, path) in super::wal::list_segments(&super::wal::wal_dir(dir)).unwrap() {
        let contents = super::wal::read_segment(&path).unwrap();
        times.extend(
            contents
                .records
                .iter()
                .map(|record| record.committed_micros),
        );
    }
    times
}

/// A copy of the store in `dir` (a restore changes what it opens).
fn copied(dir: &std::path::Path) -> tempfile::TempDir {
    fn copy(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let target = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy(&path, &target);
            } else if path.file_name().is_some_and(|name| name != "LOCK") {
                std::fs::copy(&path, &target).unwrap();
            }
        }
    }
    let copy_dir = tempfile::tempdir().unwrap();
    copy(dir, copy_dir.path());
    copy_dir
}

/// A transaction is stamped when its record goes to the log, its preparation done (review
/// r1, R1-F1): the clock reads 1000 when it commits and 2000 once its runs are built, its
/// layout converted or the log rotated (`fault::CLOCK_AT_APPEND`). Its record and its version
/// take 2000: a restore at 1500 leaves it out, and a checkpoint after it is refused at 1999,
/// used at 2000. Before the fix both took 1000, so a restore at 1500 kept a commit made at
/// 2000. Rows, judged apart: an insert; a store's first named graph (its layout converted);
/// a commit that starts a segment.
#[test]
fn a_transaction_is_stamped_at_its_log_append() {
    let named = |n: u64| {
        let node = |name: String| NamedNode::new_unchecked(format!("http://example.com/{name}"));
        Quad::new(
            node(format!("s{n}")),
            node("p".into()),
            node(format!("o{n}")),
            node("g".into()),
        )
    };
    let rows: [(&str, Quad, EngineConfig); 3] = [
        ("an insert", quad(1), config()),
        ("a first named graph", named(1), config()),
        ("a rotation", quad(1), small_segments()),
    ];
    let mut failures = Vec::new();
    for (row, inserted, open) in rows {
        let dir = tempfile::tempdir().unwrap();
        {
            let engine = Engine::open(dir.path(), open.clone()).unwrap();
            fault::CLOCK.set(Some(500));
            commit(&engine, 0).unwrap();
            engine.checkpoint().unwrap();
            fault::CLOCK.set(Some(1000));
            fault::CLOCK_AT_APPEND.set(Some(2000));
            let mut tx = engine.transaction();
            tx.insert(inserted.as_ref());
            tx.commit().unwrap();
            fault::CLOCK_AT_APPEND.set(None);
            fault::CLOCK.set(None);
        }
        let mut wrong = Vec::new();
        let logged = logged_times(dir.path());
        if logged.last() != Some(&Some(2000)) {
            wrong.push(format!("logged at {logged:?}"));
        }
        let held = restored(copied(dir.path()).path(), 1500);
        if held != Some([quad(0)].into_iter().collect()) {
            wrong.push(format!("restored at 1500: {held:?}"));
        }
        // The version's time, as the checkpoint after it keeps it.
        Engine::open(dir.path(), open)
            .unwrap()
            .checkpoint()
            .unwrap();
        if restored(copied(dir.path()).path(), 1999).is_some() {
            wrong.push("its checkpoint used at 1999".into());
        }
        if restored(copied(dir.path()).path(), 2000).is_none() {
            wrong.push("its checkpoint refused at 2000".into());
        }
        eprintln!(
            "row {row}: {}",
            if wrong.is_empty() { "ok" } else { "FAILED" }
        );
        failures.extend(wrong.into_iter().map(|w| format!("{row}: {w}")));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Commits of unknown time leave the store's clock where it was (review r1, R1-F2): a version
/// holding them has no known time, but the commits after them still never go back. Each row
/// starts from such a history and steps the clock back from 2000 to 1000:
/// - two commits after a format 11 checkpoint are logged at 2000 and 2000;
/// - a replacement after a commit at 2000 is stamped no earlier, so a restore at 1500 refuses
///   its checkpoint;
/// - the clock survives a checkpoint of unknown time and a reopen: the commit after is logged
///   at 2000;
/// - an untimed record replayed, a commit at 2000, then a replacement: refused at 1500.
///
/// Before the fix the clock was the version's time, unknown there, so it started again.
#[test]
fn the_store_clock_never_goes_back_over_an_unknown_history() {
    // A store whose only content is of unknown time: a format 11 checkpoint of a commit.
    let unknown = || {
        let dir = tempfile::tempdir().unwrap();
        {
            let engine = Engine::open(dir.path(), config()).unwrap();
            fault::CLOCK.set(Some(3000));
            commit(&engine, 0).unwrap();
            engine.checkpoint().unwrap();
            fault::CLOCK.set(None);
        }
        as_format_11(dir.path());
        dir
    };
    let at = |engine: &Engine, clock: u64, n: u64| {
        fault::CLOCK.set(Some(clock));
        commit(engine, n).unwrap();
        fault::CLOCK.set(None);
    };
    let replace = |engine: &Engine, clock: u64| {
        fault::CLOCK.set(Some(clock));
        let load = engine.bulk_load(BulkMode::Replace);
        load.add(&[quad(9)]);
        load.finish().unwrap();
        fault::CLOCK.set(None);
    };
    let mut failures = Vec::new();
    let mut check = |row: &str, ok: bool, got: String| {
        eprintln!("row {row}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{row}: {got}"));
        }
    };

    let dir = unknown();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        at(&engine, 2000, 1);
        at(&engine, 1000, 2);
    }
    let logged = logged_times(dir.path());
    check(
        "two commits after a format 11 checkpoint",
        logged == [Some(2000), Some(2000)],
        format!("logged at {logged:?}"),
    );

    let dir = unknown();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        at(&engine, 2000, 1);
        replace(&engine, 1000);
    }
    let held = restored(dir.path(), 1500);
    check(
        "a replacement after them",
        held.is_none(),
        format!("restored at 1500: {held:?}"),
    );

    let dir = unknown();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        at(&engine, 2000, 1);
        engine.checkpoint().unwrap();
    }
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        at(&engine, 1000, 2);
    }
    let logged = logged_times(dir.path());
    check(
        "across a checkpoint and a reopen",
        logged.last() == Some(&Some(2000)),
        format!("logged at {logged:?}"),
    );

    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        at(&engine, 3000, 0);
    }
    for (_, path) in super::wal::list_segments(&super::wal::wal_dir(dir.path())).unwrap() {
        let file = std::fs::read(&path).unwrap();
        let contents = super::wal::read_segment(&path).unwrap();
        let mut bytes = file[..8].to_vec();
        for record in contents.records {
            let record = super::codec::CommitRecord {
                committed_micros: None,
                ..record
            };
            super::codec::encode_record(&record, &mut bytes).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
    }
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        at(&engine, 2000, 1);
        replace(&engine, 1000);
    }
    let held = restored(dir.path(), 1500);
    check(
        "an untimed record replayed",
        held.is_none(),
        format!("restored at 1500: {held:?}"),
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A primary's log of one commit, `quad(1)`, its record's time replaced by `at`: what a
/// replica is sent ([`Engine::apply_log`]).
fn primary_log(at: u64) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    log_after(dir.path(), at)
}

/// The log of one commit, `quad(1)`, made on the store in `dir`, its record's time replaced
/// by `at`.
fn log_after(dir: &std::path::Path, at: u64) -> Vec<u8> {
    let engine = Engine::open(dir, config()).unwrap();
    let after = engine.stats().revision;
    commit(&engine, 1).unwrap();
    let batch = engine.log_since(after, usize::MAX).unwrap();
    let (mut frames, mut pos) = (Vec::new(), 0);
    while let super::codec::Frame::Record(record, used) =
        super::codec::decode_frame(&batch.frames[pos..])
    {
        pos += used;
        let record = super::codec::CommitRecord {
            committed_micros: Some(at),
            ..record
        };
        super::codec::encode_record(&record, &mut frames).unwrap();
    }
    frames
}

/// Applies `frames`, one record after `replica`'s revision ([`log_after`]), as a replica
/// does ([`Engine::apply_log`]).
fn apply_one(replica: &Engine, frames: &[u8]) -> EngineResult<crate::Applied> {
    let after = replica.stats().revision;
    let expected = crate::LogExpectation {
        after,
        records: 1,
        last: after + 1,
    };
    replica.apply_log(frames, &expected, &nrese_exec::stop::StopSource::none())
}

/// A replicated commit's time moves the store's clock in memory as on disk (sol-loop, review
/// fix S1): an in-memory replica applies a primary's commit made at 2000, then replaces its
/// data under a clock reading 1000. The replacement is stamped no earlier than 2000, so an
/// image of it is refused by a restore at 1500. Before the fix it was stamped 1000.
#[test]
fn a_replicated_time_moves_the_clock_in_memory_too() {
    let replica = Engine::new(config()).unwrap();
    apply_one(&replica, &primary_log(2000)).unwrap();
    fault::CLOCK.set(Some(1000));
    let load = replica.bulk_load(BulkMode::Replace);
    load.add(&[quad(9)]);
    load.finish().unwrap();
    fault::CLOCK.set(None);
    let image = tempfile::tempdir().unwrap();
    replica.write_image(image.path()).unwrap();
    let held = restored(image.path(), 1500);
    assert!(
        held.is_none(),
        "the replacement's image restored at 1500: {held:?}"
    );
}

/// A commit time is below 2^63 (sol-loop, review fix S2): a checkpoint keeps it in 63 bits.
/// A replicated record at 2^63 or `u64::MAX` is refused, nothing applied; one at 2^63 - 1 is
/// applied, and its checkpoint keeps the time exactly (refused by a restore at 2^63 - 2,
/// used at 2^63 - 1; a guard). Before the fix the top bit was dropped when checkpointed,
/// making a commit look 2^63 earlier.
#[test]
fn commit_times_past_their_range_are_refused() {
    let top = 1u64 << 63;
    let mut failures = Vec::new();
    for (row, at, applied) in [
        ("u64::MAX", u64::MAX, false),
        ("2^63", top, false),
        ("2^63 - 1 (guard)", top - 1, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut wrong = Vec::new();
        {
            let replica = Engine::open(dir.path(), config()).unwrap();
            let result = apply_one(&replica, &primary_log(at));
            if result.is_ok() != applied {
                wrong.push(format!("applied: {result:?}"));
            }
            if replica.stats().revision != u64::from(applied) {
                wrong.push(format!("at revision {}", replica.stats().revision));
            }
            if applied {
                replica.checkpoint().unwrap();
            }
        }
        if applied {
            if restored(copied(dir.path()).path(), top - 2).is_some() {
                wrong.push("its checkpoint used at 2^63 - 2".into());
            }
            if restored(copied(dir.path()).path(), top - 1).is_none() {
                wrong.push("its checkpoint refused at 2^63 - 1".into());
            }
        }
        eprintln!(
            "row {row}: {}",
            if wrong.is_empty() { "ok" } else { "FAILED" }
        );
        failures.extend(wrong.into_iter().map(|w| format!("{row}: {w}")));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The newest checkpoint in `dir`, rewritten as format 11 (no commit time), as an earlier
/// binary would have written it: format 12 is format 11 with the time before the checksum.
fn as_format_11(dir: &std::path::Path) {
    let path = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "nck"))
        .max()
        .unwrap();
    let file = std::fs::read(&path).unwrap();
    assert_eq!(&file[..8], b"NRESECKC");
    let mut old = b"NRESECKB".to_vec();
    old.extend_from_slice(&file[8..file.len() - 12]);
    let crc = crc32fast::hash(&old);
    old.extend_from_slice(&crc.to_le_bytes());
    std::fs::write(&path, old).unwrap();
}

/// A version's time bounds every commit in it, whatever history it was recovered from
/// (sol-loop r1 S1, S2). Rows, each a restore at 1500 that must refuse, judged apart:
/// - a log an earlier binary wrote under a clock that stepped back (commits logged at 2000,
///   then 1000), replayed and checkpointed: the checkpoint's commits include one at 2000;
/// - a format 11 checkpoint (no time) of a commit made at 2000, then a commit at 1000 and a
///   checkpoint: what it holds from before is still of unknown time;
/// - a bulk replacement at 3000 after that, restored at 3500: it holds nothing from before,
///   but a commit of unknown time may have been made after any time this store would stamp
///   (review r2, R1-F2: a legacy log loses its times), so no later time is certified. This
///   row expected the replacement's time known until that review.
///
/// And a guard: a bulk replacement after a history of known time keeps its time, and a
/// restore after it opens.
#[test]
fn a_version_time_bounds_every_commit_in_it() {
    let mut failures = Vec::new();
    let mut check = |row: &str, got: Option<HashSet<Quad>>, expected: Option<HashSet<Quad>>| {
        let ok = got == expected;
        eprintln!("row {row}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{row}: held {got:?}, expected {expected:?}"));
        }
    };

    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit(&engine, 0).unwrap();
        commit(&engine, 1).unwrap();
    }
    for (_, path) in super::wal::list_segments(&super::wal::wal_dir(dir.path())).unwrap() {
        let file = std::fs::read(&path).unwrap();
        let contents = super::wal::read_segment(&path).unwrap();
        let mut bytes = file[..8].to_vec();
        for (record, at) in contents.records.into_iter().zip([2000, 1000]) {
            let record = super::codec::CommitRecord {
                committed_micros: Some(at),
                ..record
            };
            super::codec::encode_record(&record, &mut bytes).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
    }
    Engine::open(dir.path(), config())
        .unwrap()
        .checkpoint()
        .unwrap();
    check(
        "a stepped-back log replayed and checkpointed",
        restored(dir.path(), 1500),
        None,
    );

    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        fault::CLOCK.set(Some(2000));
        commit(&engine, 0).unwrap();
        engine.checkpoint().unwrap();
    }
    as_format_11(dir.path());
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        fault::CLOCK.set(Some(1000));
        commit(&engine, 1).unwrap();
        engine.checkpoint().unwrap();
    }
    check(
        "a commit after a format 11 checkpoint",
        restored(dir.path(), 1500),
        None,
    );
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        fault::CLOCK.set(Some(3000));
        let load = engine.bulk_load(BulkMode::Replace);
        load.add(&[quad(7)]);
        load.finish().unwrap();
    }
    fault::CLOCK.set(None);
    check("a replacement after it", restored(dir.path(), 3500), None);

    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        fault::CLOCK.set(Some(2000));
        commit(&engine, 0).unwrap();
        fault::CLOCK.set(Some(3000));
        let load = engine.bulk_load(BulkMode::Replace);
        load.add(&[quad(7)]);
        load.finish().unwrap();
    }
    fault::CLOCK.set(None);
    check(
        "a replacement after a known history (guard)",
        restored(dir.path(), 3500),
        Some([quad(7)].into_iter().collect()),
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A legacy store (integer-derived literals in its dictionary): the format 11 checkpoint an
/// earlier binary wrote of one commit, flagged so, its log released. New segments are then
/// version 4, which keep no commit times (`format_4_checkpoints_are_read` in `tests/it`).
fn legacy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit(&engine, 0).unwrap();
        engine.checkpoint().unwrap();
    }
    as_format_11(dir.path());
    let path = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "nck"))
        .max()
        .unwrap();
    let mut file = std::fs::read(&path).unwrap();
    // `magic | revision | flags`: flag bit 0, integers in the dictionary.
    file[16] |= 1;
    let body = file.len() - 4;
    let crc = crc32fast::hash(&file[..body]);
    file[body..].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(&path, file).unwrap();
    std::fs::remove_dir_all(super::wal::wal_dir(dir.path())).unwrap();
    dir
}

/// A time a legacy store's log couldn't keep is never certified by a later commit (review r2,
/// R1-F2): its version 4 segments keep no times, so a reopen replays its commits untimed.
/// Rows, judged apart, each refused by a restore at 1500 for its time (`Configuration`), the
/// store opening plainly with the replacement:
/// - a bulk replacement at 500, a transaction at 2000, a reopen, a replacement at 1000;
/// - the replica route: a primary's commit made at 2000 applied, a reopen, a replacement at
///   1000.
///
/// Before the fix the reopened clock was the checkpoint's (500, or 0), the last replacement
/// was stamped 1000 and certified, and the restore at 1500 served it.
#[test]
fn a_time_a_legacy_log_lost_is_never_certified() {
    let replace = |engine: &Engine, clock: u64, n: u64| {
        fault::CLOCK.set(Some(clock));
        let load = engine.bulk_load(BulkMode::Replace);
        load.add(&[quad(n)]);
        load.finish().unwrap();
        fault::CLOCK.set(None);
    };
    let mut failures = Vec::new();
    let mut check = |row: &str, dir: &std::path::Path, replacement: u64| {
        let mut wrong = Vec::new();
        let segments = super::wal::list_segments(&super::wal::wal_dir(dir)).unwrap();
        let newest = &segments.last().unwrap().1;
        if std::fs::read(newest).unwrap()[..8] != *b"NRESEWL4" {
            wrong.push("its log isn't of version 4".to_string());
        }
        let restore = EngineConfig {
            durability: DurabilityConfig {
                recover_until_micros: Some(1500),
                ..config().durability
            },
            ..config()
        };
        match Engine::open(copied(dir).path(), restore) {
            Err(EngineError::Configuration(_)) => {}
            Err(error) => wrong.push(format!("restore at 1500 failed otherwise: {error}")),
            Ok(engine) => wrong.push(format!("restored at 1500: {:?}", contents(&engine))),
        }
        let held = contents(&Engine::open(dir, config()).unwrap());
        if held != [quad(replacement)].into_iter().collect() {
            wrong.push(format!("opened holding {held:?}"));
        }
        eprintln!(
            "row {row}: {}",
            if wrong.is_empty() { "ok" } else { "FAILED" }
        );
        failures.extend(wrong.into_iter().map(|w| format!("{row}: {w}")));
    };

    let dir = legacy();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        replace(&engine, 500, 10);
        fault::CLOCK.set(Some(2000));
        commit(&engine, 11).unwrap();
        fault::CLOCK.set(None);
    }
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        replace(&engine, 1000, 12);
    }
    check("a transaction at 2000", dir.path(), 12);

    let dir = legacy();
    let frames = log_after(copied(dir.path()).path(), 2000);
    {
        let replica = Engine::open(dir.path(), config()).unwrap();
        apply_one(&replica, &frames).unwrap();
    }
    {
        let replica = Engine::open(dir.path(), config()).unwrap();
        replace(&replica, 1000, 12);
    }
    check("a replicated commit at 2000", dir.path(), 12);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A bulk publication is stamped when it commits, at its checkpoint's end, not when its
/// version was built before the (long) write (sol-loop r1 S3). The clock reads 1000 while the
/// load is built and 2000 once its checkpoint is written (`fault::CLOCK_AT_TRAILER`); a
/// restore at 1500 must refuse it. Rows, judged apart: an append (built in memory), a
/// replacement (streamed into the checkpoint) and a rematerialisation.
#[test]
fn a_bulk_publication_is_stamped_when_it_commits() {
    type Publish = fn(&Engine);
    let rows: [(&str, Publish); 3] = [
        ("an append", |engine| {
            let load = engine.bulk_load(BulkMode::Append);
            load.add(&[quad(10)]);
            load.finish().unwrap();
        }),
        ("a replacement", |engine| {
            let load = engine.bulk_load(BulkMode::Replace);
            load.add(&[quad(10)]);
            load.finish().unwrap();
        }),
        ("a rematerialisation", |engine| {
            let rematerialisation = engine.rematerialisation();
            let id = |n: &str| {
                rematerialisation.intern(nrese_rdf::TermRef::NamedNode(
                    NamedNode::new_unchecked(format!("http://example.com/{n}")).as_ref(),
                ))
            };
            let inferred = vec![crate::EncodedTriple {
                subject: id("s0"),
                predicate: id("q"),
                object: id("o0"),
            }];
            rematerialisation.finish(inferred).unwrap();
        }),
    ];
    let mut failures = Vec::new();
    for (row, publish) in rows {
        let dir = tempfile::tempdir().unwrap();
        {
            let engine = Engine::open(dir.path(), config()).unwrap();
            fault::CLOCK.set(Some(500));
            commit(&engine, 0).unwrap();
            fault::CLOCK.set(Some(1000));
            fault::CLOCK_AT_TRAILER.set(Some(2000));
            publish(&engine);
            fault::CLOCK_AT_TRAILER.set(None);
            fault::CLOCK.set(None);
        }
        let got = restored(dir.path(), 1500);
        let ok = got.is_none();
        eprintln!("row {row}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{row}: held {got:?}, expected a refusal"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The bytes a torn tail's cut removes are kept under a durable name before the cut (sol-loop
/// r2 S3): an open whose `wal-cut/` can't be synced after the copy is placed fails, the log
/// uncut; so does the next open while that sync still fails, though it finds the copy
/// already there (it must sync it before cutting). Once the sync works, the open cuts.
#[test]
fn a_cut_waits_for_its_copy_to_be_durable() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit(&engine, 1).unwrap();
        commit(&engine, 2).unwrap();
    }
    let (_, segment) = super::wal::list_segments(&super::wal::wal_dir(dir.path()))
        .unwrap()
        .pop()
        .unwrap();
    let full = std::fs::read(&segment).unwrap();
    // Record 2 cut short: a torn tail.
    let torn = &full[..full.len() - 10];
    std::fs::write(&segment, torn).unwrap();
    fault::FAIL_CUT_DIR_SYNC.set(true);
    let first = Engine::open(dir.path(), config()).err();
    let again = Engine::open(dir.path(), config()).err();
    fault::FAIL_CUT_DIR_SYNC.set(false);
    assert!(first.is_some(), "opened with the copy's name not synced");
    assert!(
        again.is_some(),
        "opened again with the found copy's name not synced"
    );
    assert_eq!(std::fs::read(&segment).unwrap(), torn, "the log uncut");
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), [quad(1)].into_iter().collect());
}

/// A rotation that fails once its new segment is in place under its name (its directory
/// not synced) takes no more commits before a reopen: records appended to the old segment
/// would sit behind a later-named one, which replication and segment release read as
/// covering them. Every commit acknowledged before survives. This is the contract the
/// change sets; the base goes on committing (no loss is reproduced here).
#[test]
fn a_rotation_whose_segment_may_be_placed_takes_no_more_commits() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), small_segments()).unwrap();
    commit(&engine, 1).unwrap();
    fault::FAIL_SEGMENT_PLACED.set(true);
    assert!(commit(&engine, 2).is_err());
    fault::FAIL_SEGMENT_PLACED.set(false);
    let next = commit(&engine, 3);
    assert!(matches!(next, Err(EngineError::WalPoisoned)), "{next:?}");
    drop(engine);
    let engine = Engine::open(dir.path(), small_segments()).unwrap();
    assert_eq!(contents(&engine), [quad(1)].into_iter().collect());
    commit(&engine, 4).unwrap();
}

/// A directory sync is a real one on every platform (F2): syncing a directory that isn't
/// there fails. Before the fix it was a no-op on Windows, so a checkpoint's or segment's
/// rename there wasn't durable when its commit was acknowledged. (A guard on Unix, where the
/// sync was always real; red on Windows.)
#[test]
fn a_directory_sync_is_real() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("not-there");
    assert!(
        super::sync_dir(&missing).is_err(),
        "synced a missing directory"
    );
    super::sync_dir(dir.path()).unwrap();
}

/// The directories an open creates for its store are durably named before it takes commits
/// (review r1, R1-F2): each new one's entry in the directory holding it is synced, so a power
/// loss can't take the store's name with every commit acknowledged in it. Here the sync of
/// that holding directory fails (`fault::FAIL_DIR_SYNC_OF`), and the open must refuse. Before
/// the fix only the store's own directory was synced, so it opened. Rows, judged apart:
/// - two missing levels, the sync of the existing directory above them failing;
/// - one missing level, the same;
/// - a relative single directory, the sync of `.` failing (made in the working directory,
///   removed after);
/// - the same open tried again, the sync still failing (sol-loop on review r1, S1): the first
///   refusal mustn't leave a directory the retry takes for durably named;
/// - a directory that exists, made by someone else (another attempt, another process) and not
///   known to be named durably: its holder's sync failing, the open refuses;
/// - guards: an existing directory whose holder can't be opened for the sync (no permission)
///   opens, as nothing more can be done for its name there; two missing levels open with every
///   sync working.
#[test]
fn an_open_names_every_directory_it_creates() {
    use std::io::ErrorKind;
    use std::path::PathBuf;
    let base = tempfile::tempdir().unwrap();
    let relative = PathBuf::from(format!("dur3-relative-{}", std::process::id()));
    std::fs::create_dir(base.path().join("made")).unwrap();
    // (row, store directory, the directory whose sync fails and how, attempts, whether the
    // last must open)
    type Row<'a> = (&'a str, PathBuf, Option<(PathBuf, ErrorKind)>, usize, bool);
    let failing = |dir: &std::path::Path| Some((dir.to_path_buf(), ErrorKind::Other));
    let rows: [Row; 7] = [
        (
            "two missing levels",
            base.path().join("a").join("b"),
            failing(base.path()),
            1,
            false,
        ),
        (
            "one missing level",
            base.path().join("c"),
            failing(base.path()),
            1,
            false,
        ),
        (
            "a relative single directory",
            relative.clone(),
            failing(std::path::Path::new(".")),
            1,
            false,
        ),
        (
            "tried again",
            base.path().join("r").join("s"),
            failing(base.path()),
            2,
            false,
        ),
        (
            "a directory someone else made",
            base.path().join("made"),
            failing(base.path()),
            1,
            false,
        ),
        (
            "an existing directory, its holder not to be opened (guard)",
            base.path().to_path_buf(),
            base.path()
                .parent()
                .map(|parent| (parent.to_path_buf(), ErrorKind::PermissionDenied)),
            1,
            true,
        ),
        (
            "two missing levels, every sync working (guard)",
            base.path().join("d").join("e"),
            None,
            1,
            true,
        ),
    ];
    let mut failures = Vec::new();
    for (row, dir, failing, attempts, opens) in rows {
        fault::FAIL_DIR_SYNC_OF.set(failing);
        let mut opened = Err(EngineError::Configuration("not tried".to_owned()));
        for _ in 0..attempts {
            opened = Engine::open(&dir, config()).map(|engine| commit(&engine, 0).map(drop));
        }
        fault::FAIL_DIR_SYNC_OF.set(None);
        let ok = match opens {
            true => matches!(opened, Ok(Ok(()))),
            false => opened.is_err(),
        };
        eprintln!("row {row}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{row}: {:?}", opened.map(|_| "opened")));
        }
    }
    let _ = std::fs::remove_dir_all(&relative);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A directory is made only where its name can be made durable (sol-loop round 2 on review r1,
/// S1): made first and its holder's sync then refused (`PermissionDenied`), it is in sight of
/// another process opening the same path, which finds it, can't sync its holder either, takes
/// it for one provisioned beforehand and commits into a name never made durable. Here the
/// holder's sync is refused that way: when it is tried, the new directory must not exist yet,
/// and the open refuses. Before the fix it was made first. Guard: once the sync works, it is
/// made and opens.
#[test]
fn a_directory_is_made_only_where_its_name_can_be_made_durable() {
    use std::cell::Cell;
    use std::rc::Rc;
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("q");
    let in_sight = Rc::new(Cell::new(false));
    let (seen, holder, level) = (Rc::clone(&in_sight), base.path().to_path_buf(), dir.clone());
    fault::ON_DIR_SYNC.set(Some(Box::new(move |synced: &std::path::Path| {
        if synced == holder && level.exists() {
            seen.set(true);
        }
    })));
    fault::FAIL_DIR_SYNC_OF.set(Some((
        base.path().to_path_buf(),
        std::io::ErrorKind::PermissionDenied,
    )));
    let opened = Engine::open(&dir, config());
    fault::FAIL_DIR_SYNC_OF.set(None);
    fault::ON_DIR_SYNC.set(None);
    let mut wrong = Vec::new();
    if opened.is_ok() {
        wrong.push("opened though its holder can't be synced".to_owned());
    }
    drop(opened);
    if in_sight.get() {
        wrong.push("the new directory was in sight while its holder couldn't be synced".to_owned());
    }
    if let Err(error) = Engine::open(&dir, config()) {
        wrong.push(format!(
            "guard: once its holder syncs, the open fails: {error}"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Every directory a store, its log or archive, a restore, a backup or a replica's bootstrap
/// places what recovery reads in is made through the placement owner (sol-loop round 3 on review
/// r1, S1): a directory made elsewhere, then found by `place::create_dir_all`, is taken for one
/// provisioned beforehand where its holder can't be synced, and its name is never made durable
/// (replica bootstrap made its data directory that way). Files recovery reads are renamed into
/// place through it too (round 5, S3: the quarantine's, and the reasoning marker). A source scan
/// of those callers, their test modules left out.
#[test]
fn store_directories_are_made_through_the_placement_owner() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let files = [
        "nrese-engine/src/durability/mod.rs",
        "nrese-engine/src/durability/wal.rs",
        "nrese-engine/src/durability/tail.rs",
        "nrese-engine/src/durability/checkpoint.rs",
        "nrese-store/src/image_backup.rs",
        "nrese-store/src/catalog.rs",
        "nrese-store/src/metadata.rs",
        "nrese-server/src/replication.rs",
        "nrese-server/src/http/admin_dataset.rs",
        // DUR-3 sol-loop round 5, S3, and the sweep after it: the quarantine and the reasoning
        // marker are read at open too.
        "nrese-store/src/quarantine.rs",
        "nrese-store/src/service.rs",
    ];
    // A rename recovery doesn't read: the catalogue moves a removed repository aside to delete
    // it (a lost move leaves it to its tombstone, which is synced).
    let exempt = ["std::fs::rename(&dir, &trash)"];
    let mut found = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(crates.join(file)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            // An inline test module ends the file's code (an out-of-line `mod tests;` doesn't:
            // sol-loop round 4, S2).
            let tests_begin = line.trim() == "#[cfg(test)]"
                && lines
                    .get(n + 1)
                    .is_some_and(|next| next.trim().ends_with("mod tests {"));
            if tests_begin {
                break;
            }
            let makes = ["create_dir_all(", "create_dir(", "fs::rename("]
                .into_iter()
                .any(|call| {
                    line.contains(call)
                        && !line.contains(&format!("place::{}", call.trim_start_matches("fs::")))
                });
            if makes && !exempt.iter().any(|exempt| line.contains(exempt)) {
                found.push(format!("{file}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "made outside durability::place: {found:#?}"
    );
}

/// What an open replayed is durable before it takes new commits (F5): the segments it
/// replayed records from are synced. Before the fix nothing was, so under `OsBuffered` (or an
/// `EveryCommit` record written but not synced when the process died) a power loss after the
/// reopen could take the replayed tail, though commits acknowledged after it were in a new
/// segment.
#[test]
fn an_open_syncs_what_it_replayed() {
    let buffered = || EngineConfig {
        durability: DurabilityConfig {
            sync: SyncPolicy::OsBuffered,
            ..config().durability
        },
        ..config()
    };
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), buffered()).unwrap();
        commit(&engine, 1).unwrap();
        commit(&engine, 2).unwrap();
    }
    let segments: Vec<std::path::PathBuf> =
        super::wal::list_segments(&super::wal::wal_dir(dir.path()))
            .unwrap()
            .into_iter()
            .map(|(_, path)| path)
            .collect();
    fault::SYNCED.with_borrow_mut(Vec::clear);
    let engine = Engine::open(dir.path(), config()).unwrap();
    let synced = fault::SYNCED.take();
    assert_eq!(contents(&engine), [quad(1), quad(2)].into_iter().collect());
    for segment in &segments {
        assert!(
            synced.contains(segment),
            "{} replayed but not synced (synced: {synced:?})",
            segment.display()
        );
    }
}
