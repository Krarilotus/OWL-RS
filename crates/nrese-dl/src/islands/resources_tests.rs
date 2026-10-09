//! Resource guards at the island dispatch boundary, without solver search variance.

use super::*;
use nrese_exec::workers::Workers;
use std::sync::atomic::{AtomicUsize, Ordering};
use tableau::{Answer, Cancel, Config, Outcome};

fn batches(n: usize) -> Islands {
    Islands {
        sizes: vec![1; n],
        split_edges: 0,
        batches: vec![Ontology::default(); n],
    }
}

fn decided(config: &Config, answer: Answer) -> Outcome {
    let mut out = RunBudget::new(config).stopped("unused");
    out.answer = answer;
    out
}

#[test]
fn nested_islands_share_the_owner_and_partition_only_concurrent_capacity() {
    let physical = Workers::pooled(4).unwrap();
    // allowance, batches, capacity, child allowance, child capacity
    for (allowance, count, memory, child_width, child_memory) in [
        (1, 5, 1, 1, 1),
        (2, 5, 1, 1, 0),
        (2, 5, 101, 1, 50),
        (4, 2, usize::MAX, 2, usize::MAX),
        (4, 2, 101, 2, 50),
        (4, 1, 101, 2, 101),
    ] {
        let workers = physical.limited(allowance);
        let config = Config {
            workers: Some(workers.clone()),
            max_memory: memory,
            ..Config::default()
        };
        let calls = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let out = workers.install(|| {
            decide(
                &batches(count),
                &config,
                &RunBudget::new(&config),
                |_, child| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    assert_eq!(child.max_memory, child_memory);
                    let child_workers = child.workers.as_ref().unwrap();
                    // Equality compares the physical owner too, not just its size.
                    assert_eq!(child_workers, &physical.limited(child_width));
                    child_workers.map(&[0; 8], |_| {
                        let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(n, Ordering::SeqCst);
                        std::thread::yield_now();
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                    decided(child, Answer::Consistent)
                },
            )
            .unwrap()
        });
        assert_eq!(out.answer, Answer::Consistent);
        assert_eq!(calls.load(Ordering::Relaxed), count);
        assert!(peak.load(Ordering::SeqCst) <= allowance);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn queued_islands_observe_parent_cancellation_before_entering_the_solver() {
    let cancel = Cancel::default();
    let config = Config {
        cancel: Some(cancel.clone()),
        workers: Some(Workers::pooled(1).unwrap()),
        ..Config::default()
    };
    let calls = AtomicUsize::new(0);
    let out = decide(
        &batches(3),
        &config,
        &RunBudget::new(&config),
        |_, child| {
            calls.fetch_add(1, Ordering::Relaxed);
            cancel.cancel();
            decided(child, Answer::Consistent)
        },
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(matches!(out.answer, Answer::GaveUp(_)));
}

#[test]
fn expired_islands_start_no_solver_and_a_refuted_island_still_wins() {
    let config = Config {
        timeout: Some(std::time::Duration::ZERO),
        ..Config::default()
    };
    let out = decide(&batches(2), &config, &RunBudget::new(&config), |_, _| {
        panic!("expired island entered the solver")
    })
    .unwrap();
    assert!(matches!(out.answer, Answer::GaveUp(_)));

    let config = Config {
        workers: Some(Workers::serial()),
        ..Config::default()
    };
    let calls = AtomicUsize::new(0);
    let out = decide(
        &batches(2),
        &config,
        &RunBudget::new(&config),
        |_, child| {
            let answer = if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                Answer::GaveUp("one island exceeded its capacity".into())
            } else {
                Answer::Inconsistent
            };
            decided(child, answer)
        },
    )
    .unwrap();
    assert_eq!(out.answer, Answer::Inconsistent);
}
