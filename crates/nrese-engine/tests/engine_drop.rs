//! IB0024-da9f: dropping the last `Engine` on a Rayon pool thread returns, though background
//! maintenance has parallel work queued for that pool. Dropping the last handle waits for
//! maintenance to end (its docs: the worker exits "after finishing the task in progress"); the
//! dropping thread must not be the pool thread that work waits for. A hang blocks
//! (brief v3 §2).
//!
//! Its own binary: a global pool of one thread, so the dropping thread is the only one that can
//! run the merge's parallel parts.

use std::sync::mpsc;
use std::time::Duration;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

/// Far above a 40,000-entry merge on any host: reaching it means the drop waits for good.
const DEADLINE: Duration = Duration::from_secs(60);

fn commit(engine: &Engine, from: u64, to: u64) {
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

#[test]
fn the_last_engine_dropped_on_the_pools_only_thread_returns_while_maintenance_merges() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .expect("the global pool is this binary's");
    let (dropped, answer) = mpsc::channel();
    rayon::spawn(move || {
        // Background maintenance on (the default): the second commit's 40,000-entry window is
        // above the inline limit, so it is handed to maintenance, which merges it in parallel.
        let engine = Engine::new(EngineConfig::default()).expect("engine");
        commit(&engine, 0, 20_000);
        commit(&engine, 20_000, 40_000);
        drop(engine);
        // Not unwrapped: after a failed test the receiver is gone, and a panic in a spawned job
        // aborts the process before the failure is reported.
        let _ = dropped.send(());
    });
    answer.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        panic!(
            "dropping the last engine on the pool's only thread still waits after {DEADLINE:?}: \
             maintenance's merge needs that thread (a wait cycle)"
        )
    });
}
