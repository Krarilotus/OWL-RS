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

fn decided(_: &Config, answer: Answer) -> Outcome {
    let mut out = Outcome::stopped("unused");
    out.answer = answer;
    out
}

#[test]
fn nested_islands_share_the_owner_and_partition_only_concurrent_capacity() {
    let physical = Workers::new(4);
    // allowance, batches, capacity, child allowance, child capacity
    for (allowance, count, memory, child_width, child_memory) in [
        (1, 5, Some(1), 1, Some(1)),
        (2, 5, Some(1), 1, Some(0)),
        (2, 5, Some(101), 1, Some(50)),
        (4, 2, None, 2, None),
        (4, 2, Some(101), 2, Some(50)),
        (4, 1, Some(101), 2, Some(101)),
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
            decide(&batches(count), &config, |_, child| {
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
            })
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
        workers: Some(Workers::new(1)),
        ..Config::default()
    };
    let calls = AtomicUsize::new(0);
    let out = decide(&batches(3), &config, |_, child| {
        calls.fetch_add(1, Ordering::Relaxed);
        cancel.cancel();
        decided(child, Answer::Consistent)
    })
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
    let out = decide(&batches(2), &config.started(), |_, _| {
        panic!("expired island entered the solver")
    })
    .unwrap();
    assert!(matches!(out.answer, Answer::GaveUp(_)));

    let config = Config {
        workers: Some(Workers::serial()),
        ..Config::default()
    };
    let calls = AtomicUsize::new(0);
    let out = decide(&batches(2), &config, |_, child| {
        let answer = if calls.fetch_add(1, Ordering::Relaxed) == 0 {
            Answer::GaveUp("one island exceeded its capacity".into())
        } else {
            Answer::Inconsistent
        };
        decided(child, answer)
    })
    .unwrap();
    assert_eq!(out.answer, Answer::Inconsistent);
}

/// The cross-batch range case of `tests/islands`, as its budget splits it: a node budget of
/// 64 gives batches of 16 assertions, so `a`'s island and `b`'s are decided apart, and
/// `b`'s batch carries the edge's incoming `∃r⁻.⊤(b)` beside `E(b)`, not the edge.
#[test]
fn a_budgeted_split_gives_the_target_its_incoming_stub() {
    let (r, a, b) = (10, 300, 301);
    let mut o = Ontology::default();
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let (cc, ce) = (class(&mut o, 1), class(&mut o, 2));
    o.axioms
        .push(Axiom::ObjectPropertyRange(ObjProp::Named(r), cc));
    o.axioms.push(Axiom::DisjointClasses(vec![cc, ce]));
    o.axioms.push(Axiom::ObjectPropertyAssertion(r, a, b));
    o.axioms.push(Axiom::ClassAssertion(ce, b));
    for f in 20..35 {
        let cf = class(&mut o, f);
        o.axioms.push(Axiom::ClassAssertion(cf, a));
    }
    o.sources = vec![Vec::new(); o.axioms.len()];
    let config = Config {
        max_nodes: Some(64),
        ..Config::default()
    };
    let Split::Islands(split) = split_into(&o, batch_for(&config), None) else {
        panic!("decided whole");
    };
    assert_eq!(split.batches.len(), 2);
    let target = split
        .batches
        .iter()
        .find(|o| o.axioms.contains(&Axiom::ClassAssertion(ce, b)))
        .expect("a batch holds E(b)");
    let incoming = target.axioms.iter().any(|x| match x {
        Axiom::ClassAssertion(e, y) if *y == b => matches!(
            target.classes.get(e.0),
            ClassExpr::Some(ObjProp::Inverse(p), t)
                if *p == r && matches!(target.classes.get(t.0), ClassExpr::Thing)
        ),
        _ => false,
    });
    assert!(incoming, "{:?}", target.axioms);
    assert!(
        !target
            .axioms
            .contains(&Axiom::ObjectPropertyAssertion(r, a, b))
    );
}
