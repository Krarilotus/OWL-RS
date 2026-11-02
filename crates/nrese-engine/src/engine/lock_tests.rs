//! SYN-22 storage slots: what a Rayon pool thread gets when a slot is held (design note
//! `2026-11-02-p2-c6a-storage-locks-design.md`). A pool thread never waits for the writer or
//! checkpoint slot: their holders fork onto the caller's pool, which the waiting thread may
//! be part of. Each test asks from a pool of its own, so it can't hold up other tests' pools.

use std::sync::mpsc;
use std::time::Duration;

use super::{Engine, EngineConfig};

/// Far above any answer's time: reaching it means the pool thread waits for the slot.
const DEADLINE: Duration = Duration::from_secs(60);

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    }
}

/// Runs `ask` on a thread of a pool of one and returns its answer, or fails the test if it
/// is still waiting at the deadline.
fn ask_from_the_pool<T: Send + 'static>(what: &str, ask: impl FnOnce() -> T + Send + 'static) -> T {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("a pool of one");
    let (answer, answered) = mpsc::channel();
    pool.spawn(move || {
        let _ = answer.send(ask());
    });
    answered.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        panic!("{what} from a pool thread still waits for the held slot after {DEADLINE:?}")
    })
}

/// Engine::checkpoint's contract: on a pool thread, a held checkpoint slot is an error
/// saying so, not a wait (the holder writes the checkpoint on its caller's pool).
#[test]
fn a_checkpoint_asked_on_a_pool_thread_while_one_runs_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    let durable = engine.inner.shared.durable.as_ref().expect("durable");
    let held = durable.checkpoint_slot.lock();
    let asking = engine.clone();
    let answer = ask_from_the_pool("a checkpoint", move || {
        asking.checkpoint().map_err(|error| error.to_string())
    });
    drop(held);
    let error = answer.expect_err("the slot is held: no checkpoint was written");
    assert!(error.contains("busy"), "says the slot is busy: {error}");
}

/// The fallible writer forms on a pool thread: a held writer slot is `SlotBusy`, a free one
/// is taken (refusing it there would be a false refusal).
#[test]
fn the_fallible_writer_forms_on_a_pool_thread_are_refused_only_while_it_is_held() {
    use crate::EngineError::SlotBusy;
    let engine = Engine::new(config()).unwrap();
    let busy = |result: Result<(), crate::EngineError>| matches!(result, Err(SlotBusy("writer")));

    let held = engine.transaction();
    let asking = engine.clone();
    let answers = ask_from_the_pool("the writer's try forms", move || {
        [
            asking.try_transaction().map(drop),
            asking.try_bulk_load(super::BulkMode::Append).map(drop),
            asking.try_rematerialisation().map(drop),
            asking.apply_log(&[]).map(drop),
        ]
    });
    drop(held);
    assert!(
        answers.into_iter().all(busy),
        "all four are refused while it is held"
    );

    let asking = engine.clone();
    let free = ask_from_the_pool("a free writer slot", move || {
        let taken = asking.try_transaction().map(drop);
        taken.is_ok()
    });
    assert!(free, "a free slot is taken on a pool thread");
}

/// A compaction on a pool thread defers to a running merge instead of waiting for it (that
/// thread may hold a part of the merge under its wait); the merge goes on until the policy is
/// satisfied, so nothing is left undone.
#[test]
fn a_compaction_on_a_pool_thread_defers_to_a_running_merge() {
    use super::CompactionOutcome;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let versions = &engine.inner.shared.versions;
    let merging = versions.claim(false).expect("no merge runs yet");
    // Runs a merge would combine, committed while the claim is held: the inline compaction
    // after each commit defers, and so does the background worker's, which waits.
    for i in 0..8u64 {
        let mut tx = engine.transaction();
        let quad = nrese_rdf::Quad::new(
            nrese_rdf::NamedNode::new_unchecked(format!("urn:s{i}")),
            nrese_rdf::NamedNode::new_unchecked("urn:p"),
            nrese_rdf::NamedNode::new_unchecked("urn:o"),
            nrese_rdf::GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
        tx.commit().unwrap();
    }
    assert_eq!(versions.compact(true), CompactionOutcome::Deferred);
    let runs = engine.stats().runs;
    let asking = engine.clone();
    ask_from_the_pool("a compaction", move || asking.compact());
    assert_eq!(engine.stats().runs, runs, "the pool thread merged nothing");
    assert_eq!(engine.stats().compactions, 0);

    drop(merging);
    engine.settle();
    assert!(
        engine.stats().compactions > 0,
        "the waiting merge ran once the claim ended"
    );
    assert!(engine.stats().runs < runs);
}
