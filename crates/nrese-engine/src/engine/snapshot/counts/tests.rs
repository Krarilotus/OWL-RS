use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nrese_exec::{Budget, Scratch, Stop, measure};
use nrese_rdf::{NamedNode, Quad};

use super::*;
use crate::engine::Version;
use crate::index::{IndexEncoding, IndexVersion, Layout};
use crate::{EncodedQuad, EncodedTriple, Engine, EngineConfig, TermKind};

fn id(n: u64) -> TermId {
    TermId::new(TermKind::Iri, n)
}

fn snapshot(n: u64) -> Snapshot {
    let seed = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap()
    .snapshot();
    let quads: Vec<_> = (0..n)
        .map(|i| {
            EncodedQuad::new(
                id(100 + i / 3),
                id(1),
                id(100 + i),
                if i % 3 == 0 {
                    TermId::DEFAULT_GRAPH
                } else {
                    id(2 + i % 2)
                },
            )
        })
        .collect();
    let deletes: Vec<_> = quads.iter().step_by(7).copied().collect();
    let mut version = Version::empty(IndexEncoding::Fast);
    version.asserted = IndexVersion::from_quads(Layout::Quads, IndexEncoding::Fast, quads)
        .with_delta(&[], &deletes);
    version.inferred = IndexVersion::from_quads(
        Layout::DefaultGraph,
        IndexEncoding::Fast,
        vec![EncodedQuad::new(
            id(9),
            id(1),
            id(10),
            TermId::DEFAULT_GRAPH,
        )],
    );
    seed.with_version(version)
}

fn oracle(snapshot: &Snapshot, model: ReadModel, graphs: GraphSelector) -> u64 {
    let pattern = QuadPattern {
        graph: graphs,
        ..QuadPattern::all()
    };
    let mut nodes = BTreeSet::new();
    for quad in snapshot.quads_for_pattern_in(model, &pattern) {
        nodes.insert(quad.subject);
        nodes.insert(quad.object);
    }
    nodes.len() as u64
}

#[test]
fn cold_and_warm_nodes_match_visible_union_without_warm_preparation() {
    let snapshot = snapshot(1000);
    let budget = Budget::new(4 * 1024 * 1024);
    for model in [
        ReadModel::Asserted,
        ReadModel::Inferred,
        ReadModel::Materialised,
    ] {
        for graph in [
            GraphSelector::Any,
            GraphSelector::Exact(TermId::DEFAULT_GRAPH),
            GraphSelector::Exact(id(2)),
            GraphSelector::Exact(id(999_999)),
        ] {
            let expected = oracle(&snapshot, model, graph);
            assert_eq!(
                snapshot.node_count_in_within(model, graph, &budget, &|| false),
                Ok(Some(expected))
            );
            assert_eq!(budget.used(), 0);
            let before = snapshot
                .statistics
                .group_preparations
                .load(Ordering::Relaxed);
            let zero = Budget::new(0);
            assert_eq!(
                snapshot.node_count_in_within(model, graph, &zero, &|| false),
                Ok(Some(expected))
            );
            assert_eq!(
                snapshot
                    .statistics
                    .group_preparations
                    .load(Ordering::Relaxed),
                before
            );
            assert_eq!(zero.peak(), 0);
        }
    }
    let before = snapshot.statistics.node_entries();
    assert_eq!(
        snapshot.node_count_in_within(
            ReadModel::Asserted,
            GraphSelector::AnyNamed,
            &budget,
            &|| false
        ),
        Ok(None)
    );
    assert_eq!(snapshot.statistics.node_entries(), before);
    // Unsupported prefix and a supported empty group stream are different results.
    let pattern = QuadPattern {
        object: Some(id(10)),
        ..QuadPattern::all()
    };
    assert!(
        snapshot
            .group_counts_in_within(
                ReadModel::Asserted,
                &pattern,
                Permutation::Spog,
                &budget,
                &|| false
            )
            .unwrap()
            .is_none()
    );
    let pattern = QuadPattern::in_graph(id(999_999));
    assert!(
        snapshot
            .group_counts_in_within(
                ReadModel::Asserted,
                &pattern,
                Permutation::Gspo,
                &budget,
                &|| false
            )
            .unwrap()
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn refused_and_cancelled_counts_do_not_publish_or_touch_other_credit() {
    let snapshot = snapshot(20_000);
    let model = ReadModel::Materialised;
    // Keep an unrelated successful scalar while later attempts fail.
    let warm = Budget::new(4 * 1024 * 1024);
    snapshot
        .node_count_in_within(model, GraphSelector::Exact(id(999_999)), &warm, &|| false)
        .unwrap();
    let entries = snapshot.statistics.node_entries();
    for limit in [0, 128, 1024, 4096, 32768] {
        let budget = Budget::new(limit + 37);
        let mut prior = Scratch::with_capacity(&budget, 37).unwrap();
        prior.extend_from_slice(&[7u8; 37]).unwrap();
        assert!(matches!(
            snapshot.node_count_in_within(model, GraphSelector::Any, &budget, &|| false),
            Err(Stop::Memory(_))
        ));
        assert_eq!(budget.used(), 37);
        assert_eq!(&*prior, &[7u8; 37]);
        assert_eq!(snapshot.statistics.node_entries(), entries);
        drop(prior);
        assert_eq!(budget.used(), 0);
    }
    let calls = AtomicUsize::new(0);
    let stop = || calls.fetch_add(1, Ordering::Relaxed) >= 12;
    assert_eq!(
        snapshot.node_count_in_within(model, GraphSelector::Any, &warm, &stop),
        Err(Stop::Cancelled)
    );
    assert_eq!(warm.used(), 0);
    assert_eq!(snapshot.statistics.node_entries(), entries);
    assert_eq!(
        snapshot.node_count_in_within(model, GraphSelector::Any, &warm, &|| false),
        Ok(Some(oracle(&snapshot, model, GraphSelector::Any)))
    );
    // Sticky refusal wins even on a warm hit or an unsupported selector.
    let reads = Arc::new(Budget::new(1024));
    let bounded = snapshot.reading_within(reads).unwrap();
    assert!(!bounded.reads.as_ref().unwrap().charge(2048));
    for graph in [GraphSelector::Any, GraphSelector::AnyNamed] {
        assert!(matches!(
            bounded.node_count_in_within(model, graph, &warm, &|| false),
            Err(Stop::Memory(_))
        ));
    }
}

#[test]
fn the_last_cold_stop_check_precedes_cache_publication() {
    let first = snapshot(100);
    let budget = Budget::new(1024 * 1024);
    let checks = AtomicUsize::new(0);
    let count_checks = || {
        checks.fetch_add(1, Ordering::Relaxed);
        false
    };
    first
        .node_count_in_within(
            ReadModel::Materialised,
            GraphSelector::Any,
            &budget,
            &count_checks,
        )
        .unwrap();
    let n = checks.load(Ordering::Relaxed);
    assert!(n > 2);
    let second = snapshot(100);
    let checks = AtomicUsize::new(0);
    let stop_at_last = || checks.fetch_add(1, Ordering::Relaxed) + 1 == n;
    assert_eq!(
        second.node_count_in_within(
            ReadModel::Materialised,
            GraphSelector::Any,
            &budget,
            &stop_at_last
        ),
        Err(Stop::Cancelled)
    );
    assert_eq!(second.statistics.node_entries(), 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn admitted_parts_and_scalar_hold_only_their_own_actual_allocations() {
    if !measure::alone() {
        return;
    }
    let snapshot = snapshot(140_000);
    let budget = Budget::new(32 * 1024 * 1024);
    let stop = || false;
    let pattern = QuadPattern::all();
    let ((), excess) = measure::measure_within(&budget, || {
        let mut prior = Scratch::with_capacity(&budget, 37).unwrap();
        prior.extend_from_slice(&[11u8; 37]).unwrap();
        let mut groups = snapshot
            .group_counts_in_within(
                ReadModel::Materialised,
                &pattern,
                Permutation::Ospg,
                &budget,
                &stop,
            )
            .unwrap()
            .unwrap();
        let held = budget.used();
        assert!(held > 37);
        assert!(groups.next().unwrap().is_ok());
        drop(groups);
        assert_eq!(budget.used(), 37);
        assert_eq!(&*prior, &[11u8; 37]);
        drop(prior);
    });
    assert_eq!(
        excess, 0,
        "part preparation/drop exceeded admission by {excess}"
    );
    assert_eq!(budget.used(), 0);
    eprintln!("group-allocation peak={} excess={excess}", budget.peak());
    let entries = snapshot.statistics.node_entries();
    for limit in [37, 2085, 32805] {
        let refused = Budget::new(limit);
        let ((), excess) = measure::measure_within(&refused, || {
            let mut prior = Scratch::with_capacity(&refused, 37).unwrap();
            prior.extend_from_slice(&[17u8; 37]).unwrap();
            assert!(matches!(
                snapshot.node_count_in_within(
                    ReadModel::Materialised,
                    GraphSelector::Any,
                    &refused,
                    &stop
                ),
                Err(Stop::Memory(_))
            ));
            assert_eq!(refused.used(), 37);
            assert_eq!(&*prior, &[17u8; 37]);
            drop(prior);
        });
        assert_eq!(
            excess, 0,
            "refused preparation/drop exceeded admission by {excess}"
        );
        assert_eq!(refused.used(), 0);
        assert_eq!(snapshot.statistics.node_entries(), entries);
    }
    // Cache capacity belongs to Statistics. Prepare its existing bounded metadata outside
    // this request measurement, then measure a cold different key with room in that Vec.
    snapshot
        .node_count_in_within(
            ReadModel::Asserted,
            GraphSelector::Exact(id(999_999)),
            &budget,
            &stop,
        )
        .unwrap();
    let expected = oracle(&snapshot, ReadModel::Materialised, GraphSelector::Any);
    let (answer, excess) = measure::measure_within(&budget, || {
        let mut prior = Scratch::<u8>::with_capacity(&budget, 37).unwrap();
        prior.extend_from_slice(&[13u8; 37]).unwrap();
        let answer = snapshot.node_count_in_within(
            ReadModel::Materialised,
            GraphSelector::Any,
            &budget,
            &stop,
        );
        assert_eq!(budget.used(), 37);
        drop(prior);
        answer
    });
    assert_eq!(answer, Ok(Some(expected)));
    assert_eq!(
        excess, 0,
        "scalar preparation/drop exceeded admission by {excess}"
    );
    assert_eq!(budget.used(), 0);
    let before = snapshot
        .statistics
        .group_preparations
        .load(Ordering::Relaxed);
    let (answer, allocated) = measure::measure(|| {
        snapshot.node_count_in_within(
            ReadModel::Materialised,
            GraphSelector::Any,
            &Budget::new(0),
            &stop,
        )
    });
    assert_eq!(answer, Ok(Some(expected)));
    assert_eq!(allocated, 0);
    assert_eq!(
        snapshot
            .statistics
            .group_preparations
            .load(Ordering::Relaxed),
        before
    );
    eprintln!(
        "scalar-allocation peak={} excess={excess}; warm-allocation={allocated}",
        budget.peak()
    );
}

#[test]
fn equality_domains_keep_the_existing_optimized_eligibility() {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let node = |s| NamedNode::new_unchecked(format!("urn:{s}"));
    let mut tx = engine.transaction();
    let same_as = tx.intern(node("sameAs").as_ref().into());
    let alias = tx.intern(node("alias").as_ref().into());
    let rep = tx.intern(node("rep").as_ref().into());
    tx.insert_inferred(EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: rep,
    });
    tx.insert(Quad::new(node("alias"), node("p"), node("o"), node("g")).as_ref());
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let expanded = engine.snapshot();
    let budget = Budget::new(1024 * 1024);
    assert_eq!(
        expanded.node_count_in_within(
            ReadModel::Materialised,
            GraphSelector::Any,
            &budget,
            &|| false
        ),
        Ok(None)
    );
    let canonical = expanded.with_canonical_equality();
    assert_eq!(
        canonical.node_count_in_within(
            ReadModel::Materialised,
            GraphSelector::Any,
            &budget,
            &|| false
        ),
        Ok(None)
    );
    let graph = GraphSelector::Exact(expanded.lookup(node("g").as_ref().into()).unwrap());
    for snapshot in [&expanded, &canonical] {
        let expected = oracle(snapshot, ReadModel::Asserted, graph);
        assert_eq!(
            snapshot.node_count_in_within(ReadModel::Asserted, graph, &budget, &|| false),
            Ok(Some(expected))
        );
    }
}
