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
/// Each row is judged on its own.
#[test]
fn a_time_restore_never_serves_commits_made_after_its_time() {
    let held = |n: u64| (0..n).map(quad).collect::<HashSet<_>>();
    // (row, times, checkpointed after, until, what it may hold: None = refuse only)
    let rows: [(&str, &[u64], usize, u64, Option<HashSet<Quad>>); 4] = [
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
