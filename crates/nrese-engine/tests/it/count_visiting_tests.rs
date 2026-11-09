//! `Snapshot::count_in_visiting` (REAS-2 review r1 F2): a count that tells its caller about
//! the work as it goes, so a reasoner's stop sees a long count; and `count_in`, which is its
//! case that never stops, answering as before.

use nrese_engine::quad::Permutation;
use nrese_engine::{
    CompactionPolicy, Count, EncodedTriple, Engine, EngineConfig, GraphSelector, QuadPattern,
    ReadModel, Snapshot, Stopped,
};
use nrese_rdf::{GraphName, NamedNode, Quad};

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

/// Under equality by representatives (expanded reads): `rep` a class of `size` identities
/// (each other one `alias sameAs rep`, inferred), and `facts` statements `rep p o_i` in the
/// default graph, plus as many `x_i p rep` in a named graph. Its count of `? p ?` scans
/// the stored matches and weighs each by its copies.
fn compact(size: usize, facts: usize) -> Snapshot {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(node("rep").as_ref().into());
    let same_as = tx.intern(node("sameAs").as_ref().into());
    for i in 1..size {
        let alias = tx.intern(node(&format!("alias{i}")).as_ref().into());
        tx.insert_inferred(EncodedTriple {
            subject: alias,
            predicate: same_as,
            object: rep,
        });
    }
    for i in 0..facts {
        tx.insert(
            Quad::new(
                node("rep"),
                node("p"),
                node(&format!("o{i}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        tx.insert(
            Quad::new(
                node(&format!("x{i}")),
                node("p"),
                node("rep"),
                GraphName::NamedNode(node("g")),
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    engine.snapshot()
}

/// The patterns asked: every quad, `? p ?`, `rep p ?`, `? p rep`, and each in the named
/// graphs only.
fn patterns(snapshot: &Snapshot) -> Vec<QuadPattern> {
    let id = |name: &str| snapshot.lookup(node(name).as_ref().into()).unwrap();
    let (rep, p) = (id("rep"), id("p"));
    let mut out = Vec::new();
    for graph in [GraphSelector::Any, GraphSelector::AnyNamed] {
        for (s, o) in [(None, None), (Some(rep), None), (None, Some(rep))] {
            out.push(QuadPattern {
                subject: s,
                predicate: Some(p),
                object: o,
                graph,
            });
        }
        out.push(QuadPattern {
            graph,
            ..QuadPattern::all()
        });
    }
    out
}

/// A count that is stopped part-way ends then: its visitor told to stop after `stop_at`
/// units of a count far longer, it is asked for no more than one more step of work (each
/// step at most a stored quad, a copy, or a run's range) and answers `None`.
#[test]
fn a_visited_count_stops_within_a_step_of_its_visitor() {
    let snapshot = compact(50, 4_000);
    let all = QuadPattern::all();
    let mut whole = 0u64;
    let full = snapshot
        .count_in_visiting(ReadModel::Materialised, &all, &mut |n| {
            whole += n;
            true
        })
        .expect("never stopped");
    assert!(
        whole > 4_000,
        "{whole} units visited: the count scans the stored matches"
    );
    assert_eq!(full, snapshot.count_in(ReadModel::Materialised, &all));
    let stop_at = whole / 2;
    let (mut seen, mut after) = (0u64, 0u64);
    let stopped = snapshot.count_in_visiting(ReadModel::Materialised, &all, &mut |n| {
        if seen >= stop_at {
            after += 1;
            return false;
        }
        seen += n;
        true
    });
    assert_eq!(stopped, None, "a count its visitor stopped answers nothing");
    assert_eq!(after, 1, "asked once more after the stop, then it ended");
}

/// `count_in` answers what reading the matches gives, on a compact snapshot (expanded
/// counts) and on the same data without equality (held counts), for every pattern asked.
#[test]
fn count_in_answers_as_the_matches_read() {
    let snapshot = compact(50, 400);
    for pattern in patterns(&snapshot) {
        for model in [
            ReadModel::Materialised,
            ReadModel::Asserted,
            ReadModel::Inferred,
        ] {
            let read = snapshot.quads_for_pattern_in(model, &pattern).count() as u64;
            assert_eq!(
                snapshot.count_in(model, &pattern),
                Count::Exact(read),
                "{model:?} {pattern:?}"
            );
            assert_eq!(
                snapshot.count_in_visiting(model, &pattern, &mut |_| true),
                Some(Count::Exact(read)),
                "{model:?} {pattern:?}, visited"
            );
        }
    }
}

/// Without equality, over several runs with deletions: `facts` statements `rep p o_i_r` in
/// the default graph in each of three commits, `x_i p rep` in a named graph, and every other
/// statement of the first commit deleted by a fourth; no run merged (a policy that forms no
/// window), so reads merge four runs and pass tombstones.
fn layered(facts: usize) -> Snapshot {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        compaction: CompactionPolicy {
            fanout: 0,
            ..CompactionPolicy::default()
        },
        ..EngineConfig::default()
    })
    .unwrap();
    let statement = |i: usize, round: usize| {
        Quad::new(
            node("rep"),
            node("p"),
            node(&format!("o{i}_{round}")),
            GraphName::DefaultGraph,
        )
    };
    for round in 0..3 {
        let mut tx = engine.transaction();
        for i in 0..facts {
            tx.insert(statement(i, round).as_ref());
            tx.insert(
                Quad::new(
                    node(&format!("x{i}")),
                    node("p"),
                    node("rep"),
                    GraphName::NamedNode(node("g")),
                )
                .as_ref(),
            );
        }
        tx.commit().unwrap();
    }
    let mut tx = engine.transaction();
    for i in (0..facts).step_by(2) {
        tx.remove(statement(i, 0).as_ref());
    }
    tx.commit().unwrap();
    engine.snapshot()
}

/// REAS-3 (design note, attack list C8, C4). Spec: a counted read answers exactly what its
/// uncounted twin answers, in the same order, on a compact snapshot (expanded reads: class
/// build, copies, visibility) and on one of several runs with tombstones and a named graph;
/// and once its visitor has stopped it answers `Stopped`, never a value or the end of the
/// data (REAS-2's rule 2).
#[test]
fn counted_reads_answer_as_the_uncounted_ones() {
    let go = |_: u64| true;
    let halt = |_: u64| false;
    for snapshot in [compact(50, 400), layered(400)] {
        let counted = snapshot.counted();
        for pattern in patterns(&snapshot) {
            for model in [
                ReadModel::Materialised,
                ReadModel::Asserted,
                ReadModel::Inferred,
            ] {
                let at = format!("{model:?} {pattern:?}");
                let plain: Vec<_> = snapshot.quads_for_pattern_in(model, &pattern).collect();
                let read: Result<Vec<_>, Stopped> = counted.quads(model, &pattern, &go).collect();
                assert_eq!(read.as_ref(), Ok(&plain), "quads, {at}");
                assert_eq!(
                    counted
                        .scan_sorted(model, &pattern, Permutation::Spog, &go)
                        .map(|quads| quads.collect::<Result<Vec<_>, Stopped>>()),
                    snapshot
                        .scan_sorted_in(model, &pattern, Permutation::Spog)
                        .map(|quads| Ok(quads.collect::<Vec<_>>())),
                    "scan_sorted, {at}"
                );
                assert_eq!(
                    counted.count(model, &pattern, &go),
                    Ok(snapshot.count_in(model, &pattern)),
                    "count, {at}"
                );
                assert_eq!(
                    counted.exists(model, &pattern, &go),
                    Ok(snapshot.exists_in(model, &pattern)),
                    "exists, {at}"
                );
                assert_eq!(
                    counted.estimate(model, &pattern, &go),
                    Ok(snapshot.estimate_in(model, &pattern)),
                    "estimate, {at}"
                );
                for quad in plain.iter().take(40) {
                    assert_eq!(
                        counted.contains(model, quad, &go),
                        Ok(snapshot.contains_in(model, quad)),
                        "contains {quad:?}, {at}"
                    );
                }
                // Stopped from the start: every read says so.
                let stopped: Result<Vec<_>, Stopped> =
                    counted.quads(model, &pattern, &halt).collect();
                assert_eq!(stopped, Err(Stopped), "stopped quads, {at}");
                assert_eq!(counted.count(model, &pattern, &halt), Err(Stopped), "{at}");
                assert_eq!(counted.exists(model, &pattern, &halt), Err(Stopped), "{at}");
                assert_eq!(
                    counted.estimate(model, &pattern, &halt),
                    Err(Stopped),
                    "{at}"
                );
                if let Some(quad) = plain.first() {
                    assert_eq!(counted.contains(model, quad, &halt), Err(Stopped), "{at}");
                }
            }
        }
    }
}

/// REAS-3a, sol-loop r2 S6. Spec (`nrese_engine::visit`: a union is counted by the members
/// it moves): a counted read's cold equality class build counts in proportion to its input.
/// An asserted chain of `n` identities (`s_i sameAs s_i+1`, one class) and materialisation's
/// inferred star over it (`s_i sameAs s_0`, each pair already in that class, which a union
/// leaves as it is): a count's visitor is told of at most a small multiple of `n` units.
/// Charging a union by the smaller side's size whatever it does, each star statement counted
/// the whole class: about `n` squared.
#[test]
fn a_counted_class_build_counts_in_proportion_to_its_statements() {
    let n = 2 * nrese_exec::POLL_UNITS as usize;
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    let same_as = tx.intern(node("sameAs").as_ref().into());
    let ids: Vec<_> = (0..n)
        .map(|i| tx.intern(node(&format!("s{i}")).as_ref().into()))
        .collect();
    for i in 1..n {
        tx.insert(
            Quad::new(
                node(&format!("s{}", i - 1)),
                node("sameAs"),
                node(&format!("s{i}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    for &identity in &ids[1..] {
        tx.insert_inferred(EncodedTriple {
            subject: identity,
            predicate: same_as,
            object: ids[0],
        });
    }
    tx.insert(Quad::new(node("s0"), node("p"), node("o"), GraphName::DefaultGraph).as_ref());
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let snapshot = engine.snapshot();
    let p = snapshot.lookup(node("p").as_ref().into()).unwrap();
    let pattern = QuadPattern {
        predicate: Some(p),
        ..QuadPattern::all()
    };
    let units = std::cell::Cell::new(0u64);
    let counted = snapshot
        .counted()
        .count(ReadModel::Materialised, &pattern, &|k| {
            units.set(units.get() + k);
            true
        });
    assert_eq!(
        counted,
        Ok(Count::Exact(n as u64)),
        "the statement's copy for each identity"
    );
    let (units, bound) = (units.get(), 16 * n as u64);
    assert!(
        units <= bound,
        "{units} units counted for {n} identities: at most {bound}"
    );
}
