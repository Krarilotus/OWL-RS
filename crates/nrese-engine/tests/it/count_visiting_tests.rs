//! `Snapshot::count_in_visiting` (REAS-2 review r1 F2): a count that tells its caller about
//! the work as it goes, so a reasoner's stop sees a long count; and `count_in`, which is its
//! case that never stops, answering as before.

use nrese_engine::{
    Count, EncodedTriple, Engine, EngineConfig, GraphSelector, QuadPattern, ReadModel, Snapshot,
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
