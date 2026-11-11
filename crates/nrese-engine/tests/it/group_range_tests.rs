//! `Snapshot::group_counts_in_range`: the group counts of a pattern within a range of its
//! key, as the whole pattern's group counts within that range, charged as they are.

use nrese_engine::quad::Permutation;
use nrese_engine::{
    EncodedTriple, Engine, EngineConfig, GraphSelector, QuadPattern, ReadModel, TermId,
};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

/// The groups of `groups` whose value lies in `low..=high`, in id order.
fn within(groups: &[(TermId, u64)], low: TermId, high: TermId) -> Vec<(TermId, u64)> {
    groups
        .iter()
        .copied()
        .filter(|&(value, _)| (low.raw()..=high.raw()).contains(&value.raw()))
        .collect()
}

/// For every range of the key (empty, reversed, one value, part, all, past the data), in
/// every graph scope and in both stacks, the ranged walk answers the whole walk's groups
/// within the range; and a budget too small for the groups refuses the walk holding
/// nothing.
#[test]
fn a_ranged_group_count_is_the_whole_count_within_the_range() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let age = tx.intern(node("age").as_ref().into());
    for i in 0..2000i64 {
        let graph = match i % 3 {
            0 => GraphName::NamedNode(node("g")),
            _ => GraphName::DefaultGraph,
        };
        tx.insert(
            Quad::new(
                node(&format!("s{i}")),
                node("age"),
                Literal::from((i * 7) % 100),
                graph,
            )
            .as_ref(),
        );
    }
    // Inferred statements, in the other stack: some on values the asserted ones hold too.
    for i in 0..300i64 {
        let subject = tx.intern(node(&format!("t{i}")).as_ref().into());
        let value = tx.intern(Literal::from(i % 13 + 90).as_ref().into());
        tx.insert_inferred(EncodedTriple {
            subject,
            predicate: age,
            object: value,
        });
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let int = |v: i64| TermId::inline_integer(v).expect("inline integer");
    let ranges = [
        (int(10), int(20)),
        (int(20), int(10)),
        (int(42), int(42)),
        (int(-5), int(3)),
        (int(95), int(1000)),
        (int(200), int(300)),
        (int(i64::from(i32::MIN)), int(i64::from(i32::MAX))),
    ];
    let unlimited = nrese_exec::Budget::unlimited();
    for (graph, permutation) in [
        (GraphSelector::Any, Permutation::Posg),
        (
            GraphSelector::Exact(TermId::DEFAULT_GRAPH),
            Permutation::Gpos,
        ),
    ] {
        let pattern = QuadPattern {
            predicate: Some(age),
            graph,
            ..QuadPattern::all()
        };
        for model in [ReadModel::Asserted, ReadModel::Materialised] {
            let whole = snapshot
                .group_counts_in(model, &pattern, permutation, &unlimited)
                .unwrap()
                .expect("the walk answers the whole pattern")
                .to_vec();
            for (low, high) in ranges {
                let ranged = snapshot
                    .group_counts_in_range(model, &pattern, permutation, low, high, &unlimited)
                    .unwrap()
                    .expect("the walk answers a range");
                assert_eq!(
                    ranged.to_vec(),
                    within(&whole, low, high),
                    "{model:?} {permutation:?} {}..={}",
                    low.raw(),
                    high.raw()
                );
            }
        }
    }
    // Charged: a budget short of the groups refuses, and holds nothing after.
    let pattern = QuadPattern {
        predicate: Some(age),
        ..QuadPattern::all()
    };
    let small = nrese_exec::Budget::new(64);
    let refused = snapshot.group_counts_in_range(
        ReadModel::Materialised,
        &pattern,
        Permutation::Posg,
        int(0),
        int(99),
        &small,
    );
    assert!(refused.is_err(), "100 groups don't fit 64 bytes");
    assert_eq!(small.used(), 0);
    let roomy = nrese_exec::Budget::new(1 << 20);
    let held = snapshot
        .group_counts_in_range(
            ReadModel::Materialised,
            &pattern,
            Permutation::Posg,
            int(0),
            int(99),
            &roomy,
        )
        .unwrap()
        .expect("the walk answers a range");
    assert_eq!(held.len(), 100);
    assert!(roomy.used() > 0, "the groups held are charged");
    drop(held);
    assert_eq!(roomy.used(), 0);
}
