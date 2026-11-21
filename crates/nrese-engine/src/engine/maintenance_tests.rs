use std::sync::mpsc;
use std::time::{Duration, Instant};

use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

use super::{CompactionPolicy, DurabilityConfig, Engine, EngineConfig};
use crate::fault_points::MaintenancePanic;

const DEADLINE: Duration = Duration::from_secs(10);

fn config(checkpoint_after_wal_bytes: u64) -> EngineConfig {
    EngineConfig {
        compaction: CompactionPolicy {
            inline_entry_limit: 0,
            ..CompactionPolicy::default()
        },
        durability: DurabilityConfig {
            checkpoint_after_wal_bytes,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    }
}

fn commit(engine: &Engine, n: u32) {
    let mut tx = engine.transaction();
    let quad = Quad::new(
        NamedNode::new_unchecked(format!("http://example.org/s{n}")),
        NamedNode::new_unchecked("http://example.org/p"),
        Literal::new_simple_literal("value"),
        GraphName::DefaultGraph,
    );
    tx.insert(quad.as_ref());
    tx.commit().expect("commit");
}

fn bounded_settle(engine: &Engine) -> String {
    let (answer, answered) = mpsc::sync_channel(1);
    let engine = engine.clone();
    std::thread::spawn(move || {
        let _ = answer.send(format!("{:?}", engine.settle()));
    });
    answered
        .recv_timeout(DEADLINE)
        .expect("settle gave no answer after maintenance ended")
}

fn stops_after_panic(step: &'static str, checkpoint_threshold: u64, commits: u32) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config(checkpoint_threshold)).unwrap();
    let fault = MaintenancePanic::arm(dir.path(), step);
    for n in 0..commits {
        commit(&engine, n);
    }
    fault.wait(DEADLINE);

    let answer = bounded_settle(&engine);
    assert!(answer.starts_with("Err("), "settle returned {answer}");
    assert!(answer.contains(step), "settle omitted {step}: {answer}");
    let worker = engine
        .inner
        .worker
        .as_ref()
        .unwrap()
        .thread
        .as_ref()
        .unwrap();
    let deadline = Instant::now() + DEADLINE;
    while !worker.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(worker.is_finished(), "the worker survived the panic");

    commit(&engine, commits);
    assert_eq!(bounded_settle(&engine), answer);
    assert_eq!(engine.stats().checkpoints, 0);
    assert_eq!(engine.stats().compactions, 0);
}

/// Spec: the maintenance worker contract says a checkpoint panic ends the worker and
/// settle reports that checkpoint, including after another commit.
#[test]
fn maintenance_checkpoint_panic_is_reported_and_the_worker_stops() {
    stops_after_panic("checkpoint", 1, 1);
}

/// Spec: the maintenance worker contract says a compaction panic ends the worker and
/// settle reports that compaction, including after another commit.
#[test]
fn maintenance_compaction_panic_is_reported_and_the_worker_stops() {
    stops_after_panic("compaction", u64::MAX, 2);
}

/// Spec: settle waits for requested maintenance, so a successful background checkpoint
/// is written before settle returns.
#[test]
fn maintenance_settle_waits_for_a_successful_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config(1)).unwrap();
    commit(&engine, 0);
    bounded_settle(&engine);
    assert_eq!(engine.stats().checkpoints, 1);
}
