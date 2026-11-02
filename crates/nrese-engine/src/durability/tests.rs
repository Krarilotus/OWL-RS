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
