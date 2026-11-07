//! SYN-22 (O11 storage progress): a Rayon pool thread never waits for the engine's writer
//! slot, so a holder's parallel work always has its pool. The cycle this guards, reproduced
//! on 74ea9101 at pool widths 1 and 2 (review-locksweep-r1): an ordinary thread holds a
//! transaction, every pool thread waits for the writer slot, and the commit's parallel run
//! build waits for those threads for good.
//!
//! Specified behaviour (approved 19 Oct; `Engine::transaction`'s docs): a pool thread that
//! asks for a held writer slot is refused at once, through a panic naming
//! `Engine::try_transaction`, and the holder's commit completes. A deadlock blocks
//! (brief v3 §2).
//!
//! Its own binary: it sizes the global pool (`NRESE_TEST_POOL_WIDTH`, default 2).

use std::sync::mpsc;
use std::time::Duration;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

/// Far above what any step here takes on any host: reaching it means a wait cycle.
const DEADLINE: Duration = Duration::from_secs(60);

/// A commit this large builds its runs in parallel (`index::run`).
const LARGE: u64 = 20_000;

fn width() -> usize {
    std::env::var("NRESE_TEST_POOL_WIDTH")
        .ok()
        .and_then(|w| w.parse().ok())
        .unwrap_or(2)
}

fn quad(i: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("urn:s{i}")),
        NamedNode::new_unchecked("urn:p"),
        NamedNode::new_unchecked(format!("urn:o{i}")),
        GraphName::DefaultGraph,
    )
}

#[test]
fn pool_threads_asking_for_a_held_writer_are_refused_and_the_commit_completes() {
    let width = width();
    rayon::ThreadPoolBuilder::new()
        .num_threads(width)
        .build_global()
        .expect("the global pool is this binary's");
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();

    let mut tx = engine.transaction();
    for i in 0..LARGE {
        tx.insert(quad(i).as_ref());
    }
    // Every pool thread asks for the writer slot while this thread holds it.
    let (answered, answers) = mpsc::channel();
    for _ in 0..width {
        let (engine, answered) = (engine.clone(), answered.clone());
        rayon::spawn(move || {
            let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(engine.transaction());
            }));
            let refused = match asked {
                Ok(()) => None,
                Err(panic) => Some(
                    panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default(),
                ),
            };
            // Not unwrapped: after a failed test the receiver is gone, and a panic in a
            // spawned job aborts the process before the failure is reported.
            let _ = answered.send(refused);
        });
    }
    // Before the commit: each answers while the slot is still held (the parent tree's
    // waiters never do: they wait for this thread, which waits for them below).
    for _ in 0..width {
        let refused = answers.recv_timeout(DEADLINE).unwrap_or_else(|_| {
            panic!(
                "a pool thread still waits for the held writer slot after {DEADLINE:?}: \
                 it would block the commit's parallel work (a wait cycle)"
            )
        });
        let message = refused.expect("the slot is held: the pool thread can't have taken it");
        assert!(
            message.contains("try_transaction"),
            "the refusal names the fallible form: {message}"
        );
    }

    // The holder's parallel commit has the pool to itself (the job's step deadline bounds it).
    let summary = tx.commit().expect("commit");
    assert_eq!((summary.revision, summary.inserted), (1, LARGE));
    assert_eq!(engine.stats().quads, LARGE);
}
