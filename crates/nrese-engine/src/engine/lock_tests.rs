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

/// The writer forms on a pool thread: a held writer slot is `SlotBusy` (the infallible forms'
/// panic is in `tests/storage_waiters.rs`, the deliberately misused fixture), a free one is
/// taken by every form, infallible ones included (refusing or panicking there would be a
/// false refusal).
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
        let fallible = asking.try_transaction().map(drop).is_ok();
        let infallible = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(asking.transaction());
            drop(asking.bulk_load(super::BulkMode::Append));
            drop(asking.rematerialisation());
        }))
        .is_ok();
        (fallible, infallible)
    });
    assert_eq!(
        free,
        (true, true),
        "a free slot is taken on a pool thread, by the fallible and the infallible forms"
    );
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

/// IB0024 sol-loop r3 S1: dropping the last Engine off the pool, with maintenance requested,
/// while every thread of the global pool waits for a mutex that is let go only after the drop:
/// the drop returns. The task needs no pool thread to run (here it has nothing to merge), so
/// nothing that can't start may hold it up. The pool is held busy for this test only.
#[test]
fn the_last_engine_dropped_off_the_pool_returns_while_every_pool_thread_waits() {
    use std::sync::atomic::Ordering::SeqCst;
    use std::sync::{Arc, Mutex};

    let engine = Engine::new(EngineConfig::default()).unwrap();
    let lock = Arc::new(Mutex::new(()));
    let held = lock.lock().unwrap();
    let width = rayon::current_num_threads();
    let (started, waiting) = mpsc::channel();
    for _ in 0..width {
        let (lock, started) = (Arc::clone(&lock), started.clone());
        rayon::spawn(move || {
            let _ = started.send(());
            drop(lock.lock());
        });
    }
    for _ in 0..width {
        waiting
            .recv_timeout(DEADLINE)
            .expect("every pool thread waits for the mutex");
    }
    // A request with nothing to do: maintenance has work to start, none of it parallel.
    engine.inner.shared.wants_compaction.store(true, SeqCst);
    engine.inner.worker.as_ref().expect("maintenance on").wake();
    let (dropped, returned) = mpsc::channel();
    std::thread::spawn(move || {
        drop(engine);
        let _ = dropped.send(());
    });
    let answer = returned.recv_timeout(DEADLINE);
    drop(held);
    answer.unwrap_or_else(|_| {
        panic!(
            "the last engine's drop off the pool still waits after {DEADLINE:?}: its \
             maintenance task waits for a pool thread, and every one waits for the mutex"
        )
    });
}

/// IB0024 review C1: dropping the last Engine on a pool thread while a maintenance task is
/// in progress runs no job of that pool. Here the pool's one queued job wants a mutex the
/// dropping job holds: run beneath the drop, it would wait for good. The test holds the
/// compaction slot, so the maintenance merge that has started can't finish until the drop has
/// begun; then it lets it finish. The drop returns, the dropping job lets go of the mutex, and
/// only then does the queued job run.
#[test]
fn the_last_engine_dropped_on_a_pool_thread_runs_no_job_while_maintenance_is_in_progress() {
    use std::sync::atomic::Ordering::SeqCst;
    use std::sync::{Arc, Mutex};

    let engine = Engine::new(EngineConfig::default()).unwrap();
    let shared = Arc::clone(&engine.inner.shared);
    let slot = shared.versions.compaction_slot.lock();
    // Two runs of 20,000: their 40,000-entry window is above the inline limit, so the commit
    // hands it to maintenance, whose merge then waits for the held slot to publish.
    for (from, to) in [(0u64, 20_000u64), (20_000, 40_000)] {
        let mut tx = engine.transaction();
        for i in from..to {
            let quad = nrese_rdf::Quad::new(
                nrese_rdf::NamedNode::new_unchecked(format!("urn:s{i}")),
                nrese_rdf::NamedNode::new_unchecked("urn:p"),
                nrese_rdf::NamedNode::new_unchecked(format!("urn:o{i}")),
                nrese_rdf::GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
    }
    // The maintenance merge holds its claim: it has started, and it can't finish until the
    // slot is let go (sol-loop r3 S3: `maintaining` alone is set before a task starts).
    let started = std::time::Instant::now();
    while !*shared.versions.merging.lock() {
        assert!(
            started.elapsed() < DEADLINE,
            "the maintenance merge never started"
        );
        std::thread::yield_now();
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("a pool of one");
    let (dropping, entered) = mpsc::channel();
    let (dropped, returned) = mpsc::channel();
    let lock = Arc::new(Mutex::new(()));
    let ran_under_the_drop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (lock, ran) = (Arc::clone(&lock), Arc::clone(&ran_under_the_drop));
        pool.spawn(move || {
            let held = lock.lock().unwrap();
            let in_drop = Arc::new(std::sync::atomic::AtomicBool::new(true));
            {
                let (wants, in_drop, ran) = (Arc::clone(&lock), Arc::clone(&in_drop), ran);
                rayon::spawn(move || {
                    ran.store(in_drop.load(SeqCst), SeqCst);
                    drop(wants.lock());
                });
            }
            let _ = dropping.send(());
            drop(engine);
            in_drop.store(false, SeqCst);
            drop(held);
            let _ = dropped.send(());
        });
    }
    entered
        .recv_timeout(DEADLINE)
        .expect("the dropping job starts");
    // A margin for the drop to reach its wait (the maintenance merge can't end before the
    // slot is let go); the outcome is checked by the answer below, not by time.
    std::thread::sleep(Duration::from_millis(200));
    drop(slot);
    returned.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        panic!(
            "the last engine's drop on a pool thread still waits after {DEADLINE:?}: it ran a \
             job of its pool that waits for the caller's mutex (a wait cycle)"
        )
    });
    assert!(
        !ran_under_the_drop.load(SeqCst),
        "the pool's queued job ran beneath the drop"
    );
}
