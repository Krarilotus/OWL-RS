//! IB0024-da9f: dropping the last `Engine` on a Rayon pool thread. The drop waits for
//! background maintenance to end, so the store is released when it returns (its directory
//! lock: a reopen succeeds at once). It must neither hang because that thread is the one
//! maintenance's parallel work needs, nor run unrelated jobs of the pool while there is no
//! such work (a caller may hold a lock one of them wants). A hang blocks (brief v3 §2).
//!
//! Its own binary: a global pool of one thread, so the dropping thread is the only one that can
//! run maintenance's parallel parts. The tests run one at a time.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

/// Far above a 40,000-entry merge on any host: reaching it means the drop waits for good.
const DEADLINE: Duration = Duration::from_secs(60);

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn setup() -> std::sync::MutexGuard<'static, ()> {
    static POOL: Once = Once::new();
    POOL.call_once(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build_global()
            .expect("the global pool is this binary's");
    });
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

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

/// Runs `job` on the pool's only thread and waits for its answer up to the deadline.
fn on_the_pool<T: Send + 'static>(what: &str, job: impl FnOnce() -> T + Send + 'static) -> T {
    let (answer, answered) = mpsc::channel();
    rayon::spawn(move || {
        // Not unwrapped: after a failed test the receiver is gone, and a panic in a spawned
        // job aborts the process before the failure is reported.
        let _ = answer.send(job());
    });
    answered.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        panic!("{what} on the pool's only thread still waits after {DEADLINE:?} (a wait cycle)")
    })
}

/// Background maintenance on (the default): the second commit's 40,000-entry window is above
/// the inline limit, so it is handed to maintenance, which merges it in parallel. Dropped on
/// the pool's only thread, the engine returns, and has released its directory.
#[test]
fn the_last_engine_dropped_on_the_pools_only_thread_returns_and_releases_the_store() {
    let _one = setup();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_owned();
    let reopened = on_the_pool("dropping the last engine", move || {
        let engine = Engine::open(&path, EngineConfig::default()).expect("open");
        commit(&engine, 0, 20_000);
        commit(&engine, 20_000, 40_000);
        drop(engine);
        Engine::open(&path, EngineConfig::default()).map(|engine| engine.stats().quads)
    });
    assert_eq!(
        reopened.expect("the dropped engine released its directory"),
        40_000
    );
}

/// An idle engine's drop runs no job of the pool: here the pool's one queued job wants a
/// mutex the dropping job holds, and running it under the drop would wait for good.
#[test]
fn an_idle_engines_drop_on_a_pool_thread_runs_no_job_of_the_pool() {
    let _one = setup();
    let lock = Arc::new(Mutex::new(()));
    on_the_pool("dropping an idle engine", {
        let lock = Arc::clone(&lock);
        move || {
            let engine = Engine::new(EngineConfig::default()).expect("engine");
            let held = lock.lock().unwrap();
            let wants = Arc::clone(&lock);
            rayon::spawn(move || drop(wants.lock()));
            drop(engine);
            drop(held);
        }
    });
}
