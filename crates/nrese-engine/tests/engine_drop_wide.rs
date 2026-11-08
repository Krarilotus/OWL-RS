//! IB0024 review C1 on the global pool at width 2: the last Engine dropped on a global worker,
//! with maintenance requested, while that worker holds a mutex that a job it queued wants. The
//! drop runs no job of its pool, so it returns, and the queued job runs once the mutex is let
//! go. A hang blocks (brief v3 §2).
//!
//! Its own binary: a global pool of two threads. This guards the repaired drop; it is red on
//! the previous design only when the dropping worker takes its queued job before the other
//! worker steals it, so it is not claimed as that design's red test (the forced one is
//! `engine::lock_tests`).

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

/// Far above a 40,000-entry merge on any host: reaching it means the drop waits for good.
const DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn the_last_engine_dropped_on_a_global_worker_holding_a_lock_returns() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build_global()
        .expect("the global pool is this binary's");
    let (dropped, answer) = mpsc::channel();
    rayon::spawn(move || {
        let engine = Engine::new(EngineConfig::default()).expect("engine");
        for (from, to) in [(0u64, 20_000u64), (20_000, 40_000)] {
            let mut tx = engine.transaction();
            for i in from..to {
                let quad = Quad::new(
                    NamedNode::new_unchecked(format!("urn:s{i}")),
                    NamedNode::new_unchecked("urn:p"),
                    NamedNode::new_unchecked(format!("urn:o{i}")),
                    GraphName::DefaultGraph,
                );
                tx.insert(quad.as_ref());
            }
            tx.commit().expect("commit");
        }
        let lock = Arc::new(Mutex::new(()));
        let held = lock.lock().unwrap();
        let wants = Arc::clone(&lock);
        rayon::spawn(move || drop(wants.lock()));
        drop(engine);
        drop(held);
        // Not unwrapped: after a failed test the receiver is gone, and a panic in a spawned
        // job aborts the process before the failure is reported.
        let _ = dropped.send(());
    });
    answer.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        panic!("the last engine's drop on a global worker still waits after {DEADLINE:?}")
    });
}
