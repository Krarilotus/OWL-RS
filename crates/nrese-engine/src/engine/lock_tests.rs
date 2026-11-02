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
