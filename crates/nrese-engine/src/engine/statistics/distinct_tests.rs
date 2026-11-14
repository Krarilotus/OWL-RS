//! The distinct-value statistic made safe inside a query (D16-576, the design note
//! `2026-11-14-distinct-statistic-life-design.md` r6, row C): its walk and the count that gates
//! it stop with the query's stop, its cache is charged at its allocation and bounded, a view
//! keeps nothing, and the runs' walks are merged to today's numbers.

use std::cell::Cell;

use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

use super::*;
use crate::engine::{InferredSubset, Stack};
use crate::{EncodedTriple, Engine, EngineConfig};

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

/// No window ever merges (an older run of any size is more than 0 times a newer one), and no
/// maintenance: each commit's quads stay a run of their own.
fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        compaction: crate::CompactionPolicy {
            fanout: 0,
            inline_entry_limit: 0,
        },
        ..EngineConfig::default()
    }
}

/// `s{i} predicate "o{i}{tag}"`.
fn quad(predicate: &str, i: u32, tag: &str) -> Quad {
    Quad::new(
        iri(&format!("s{i}")),
        iri(predicate),
        Literal::new_simple_literal(format!("o{i}{tag}")),
        GraphName::DefaultGraph,
    )
}

/// `s{i} predicate "o{i}{tag}"` for each `i` in `subjects`, in one commit.
fn commit_tagged(engine: &Engine, predicate: &str, subjects: impl Iterator<Item = u32>, tag: &str) {
    let mut tx = engine.transaction();
    for i in subjects {
        assert!(tx.insert(quad(predicate, i, tag).as_ref()));
    }
    tx.commit().unwrap();
}

fn commit(engine: &Engine, predicate: &str, subjects: impl Iterator<Item = u32>) {
    commit_tagged(engine, predicate, subjects, "");
}

/// The default graph's quads of `predicate`, subject and object free.
fn of(snapshot: &Snapshot, predicate: &str) -> QuadPattern {
    QuadPattern {
        predicate: snapshot.lookup(iri(predicate).as_ref().into()),
        graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        ..QuadPattern::all()
    }
}

/// A visitor that answers yes until `limit` units have been asked for, then no for good, and
/// the units it was asked for.
fn refusing_after(limit: u64) -> (impl Fn(u64) -> bool, std::rc::Rc<Cell<u64>>) {
    let asked = std::rc::Rc::new(Cell::new(0u64));
    let seen = std::rc::Rc::clone(&asked);
    let refused = Cell::new(false);
    let visit = move |n: u64| {
        if refused.get() {
            return false;
        }
        seen.set(seen.get() + n);
        if seen.get() > limit {
            refused.set(true);
            return false;
        }
        true
    };
    (visit, asked)
}

/// The units a read asks its visitor for when nothing stops it.
fn units_of(read: impl FnOnce(Visitor<'_>)) -> u64 {
    let (visit, asked) = refusing_after(u64::MAX);
    read(&visit);
    asked.get()
}

const MODEL: ReadModel = ReadModel::Asserted;

/// - Spec: a planner's distinct count stops with its query: once the query's visitor answers
///   no during the walk, the read answers `Stopped`, does at most one more poll's work, and
///   keeps nothing.
/// - Input: 20,000 subjects of `p`, published; a visitor that refuses 2,000 units past what the
///   gating match count asks for (the walk over 20,000 keys asks for about 20,000).
/// - Expected: `Stopped`; the units asked stay within the limit plus one poll's
///   (`POLL_UNITS`), well short of the whole walk; no entry kept. Then, unstopped, the count
///   is 20,000.
#[test]
fn a_stop_during_the_walk_ends_it_and_keeps_nothing() {
    let engine = Engine::new(config()).unwrap();
    commit(&engine, "p", 0..20_000);
    let snapshot = engine.snapshot();
    let pattern = of(&snapshot, "p");
    let statistics = snapshot.statistics();
    let gate = units_of(|visit| {
        let _ = snapshot.count_visited(MODEL, &pattern, visit).unwrap();
    });
    let whole = units_of(|visit| {
        snapshot
            .distinct_in(MODEL, &pattern, Permutation::Gpso, visit)
            .unwrap();
    });
    let limit = gate + 2_000;
    assert!(whole > gate + 10_000, "the walk is long: {whole} units");
    let (visit, asked) = refusing_after(limit);
    assert_eq!(
        snapshot.distinct_estimate_in(MODEL, &pattern, Permutation::Gpso, &visit),
        Err(Stopped)
    );
    assert!(
        asked.get() <= limit + u64::from(nrese_exec::POLL_UNITS),
        "asked for {} units after a refusal at {limit}",
        asked.get()
    );
    assert_eq!(statistics.distinct_entries(), 0, "nothing kept");
    assert_eq!(
        snapshot.distinct_estimate_in(MODEL, &pattern, Permutation::Gpso, &|_| true),
        Ok(Some(20_000))
    );
}

/// - Spec: a refusal at any of the read's visits, the gate's count and the merge of several
///   runs included, answers `Stopped`, never a value, and leaves nothing behind: the next read,
///   unstopped, gives today's number.
/// - Input: subjects `0..3000` of `p` over three commits, interleaved (`i % 3`), and a fourth
///   commit that deletes subjects `0..300` (a run of tombstones); the visits the unstopped read
///   makes, counted; then, for each visit `k` in turn, a read whose visitor refuses from the
///   `k`-th call on, followed by an unstopped walk.
/// - Expected: every refused read answers `Stopped` and keeps no entry; every unstopped walk
///   after it answers 3000 (the deleted subjects' values still held by the older runs); at the
///   end an unstopped estimate answers 2700, the walk capped at the live matches, and is kept.
#[test]
fn a_refusal_at_any_visit_leaves_nothing_behind() {
    let engine = Engine::new(config()).unwrap();
    for part in 0..3 {
        commit(&engine, "p", (0..3000).filter(move |i| i % 3 == part));
    }
    let mut tx = engine.transaction();
    for i in 0..300 {
        assert!(tx.remove(quad("p", i, "").as_ref()));
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let pattern = of(&snapshot, "p");
    let statistics = snapshot.statistics();
    // The estimate's visits: the gate's count, then the walk.
    let calls = Cell::new(0u32);
    let counting = |_| {
        calls.set(calls.get() + 1);
        true
    };
    let _ = snapshot.count_visited(MODEL, &pattern, &counting).unwrap();
    assert_eq!(
        snapshot.distinct_in(MODEL, &pattern, Permutation::Gpso, &counting),
        Ok(Some(3000))
    );
    let visits = calls.get();
    assert!(visits > 4, "the read visits often: {visits}");
    for k in 1..=visits {
        let calls = Cell::new(0u32);
        let refusing = |_| {
            calls.set(calls.get() + 1);
            calls.get() < k
        };
        assert_eq!(
            snapshot.distinct_estimate_in(MODEL, &pattern, Permutation::Gpso, &refusing),
            Err(Stopped),
            "refused at visit {k} of {visits}"
        );
        assert_eq!(statistics.distinct_entries(), 0, "visit {k}: nothing kept");
        assert_eq!(
            snapshot.distinct_in(MODEL, &pattern, Permutation::Gpso, &|_| true),
            Ok(Some(3000)),
            "visit {k}: the next read"
        );
    }
    assert_eq!(
        snapshot.distinct_estimate_in(MODEL, &pattern, Permutation::Gpso, &|_| true),
        Ok(Some(2700))
    );
    assert_eq!(statistics.distinct_entries(), 1, "a finished walk is kept");
}

/// - Spec: the match count that gates the walk stops with the query too.
/// - Input: 20,000 subjects of `p` over four commits (four runs to search); a visitor that
///   refuses halfway through the units the gating count asks for.
/// - Expected: `Stopped`, no more units asked than the count's own, nothing kept.
#[test]
fn a_stop_during_the_gate_count_ends_it() {
    let engine = Engine::new(config()).unwrap();
    for part in 0..4 {
        commit(&engine, "p", (0..20_000).filter(move |i| i % 4 == part));
    }
    let snapshot = engine.snapshot();
    let pattern = of(&snapshot, "p");
    let gate = units_of(|visit| {
        let _ = snapshot.count_visited(MODEL, &pattern, visit).unwrap();
    });
    assert!(gate >= 2, "the count does work: {gate} units");
    let (visit, asked) = refusing_after(gate / 2);
    assert_eq!(
        snapshot.distinct_estimate_in(MODEL, &pattern, Permutation::Gpso, &visit),
        Err(Stopped)
    );
    assert!(asked.get() <= gate, "asked for {} of {gate}", asked.get());
    assert_eq!(snapshot.statistics().distinct_entries(), 0);
}

/// - Spec: the cache is charged to the statistics' budget at its real allocation, from its
///   first entry, and holds a fixed number of slots however many patterns are asked for;
///   where the budget can't take it, nothing is kept and the count is still answered.
/// - Input: 40 predicates `p0..p39`, 50 subjects each; a budget of 64 MiB, then one of 1 KiB.
/// - Expected: after the first count the budget's use grew by exactly the cache's charge, at
///   least `DISTINCT_SLOTS` slots; after 39 more patterns, the same charge and 40 entries.
///   Under 1 KiB: each count answered (50), nothing charged or kept.
#[test]
fn the_cache_is_charged_and_bounded() {
    let engine = Engine::new(config()).unwrap();
    for p in 0..40u32 {
        commit(&engine, &format!("p{p}"), 0..50);
    }
    let budget = Arc::new(Budget::new(64 << 20));
    engine.charge_statistics_to(Arc::clone(&budget));
    let snapshot = engine.snapshot();
    let statistics = snapshot.statistics();
    let count = |p: u32| {
        snapshot.distinct_estimate_in(
            MODEL,
            &of(&snapshot, &format!("p{p}")),
            Permutation::Gpso,
            &|_| true,
        )
    };
    let before = budget.used();
    assert_eq!(count(0), Ok(Some(50)));
    let charge = budget.used() - before;
    assert!(
        charge >= DISTINCT_SLOTS * size_of::<Slot>(),
        "charged {charge} bytes for {DISTINCT_SLOTS} slots"
    );
    assert_eq!(statistics.distinct_bytes(), Some(charge));
    for p in 1..40 {
        assert_eq!(count(p), Ok(Some(50)));
    }
    assert_eq!(budget.used() - before, charge, "a fixed charge");
    assert_eq!(statistics.distinct_entries(), 40);

    let small = Arc::new(Budget::new(1024));
    engine.charge_statistics_to(Arc::clone(&small));
    for p in 0..40 {
        assert_eq!(count(p), Ok(Some(50)));
    }
    assert_eq!(small.used(), 0, "nothing charged");
    assert_eq!(statistics.distinct_entries(), 0, "nothing kept");
}

/// - Spec: a view of a published snapshot (a restriction, which admits) counts for its own
///   query and keeps nothing; the engine's cache gets nothing from it either.
/// - Input: 300 subjects of `p`; a published snapshot's view without one inferred statement.
/// - Expected: the view's walk counts 301 (the hidden statement's subject still held by the
///   inferred stack's run, today's upper bound) and its estimate 300, capped at its 300 visible
///   matches; no entry in the view's statistics or the engine's.
#[test]
fn a_view_keeps_no_distinct_count() {
    let engine = Engine::new(config()).unwrap();
    commit(&engine, "p", 0..300);
    let mut tx = engine.transaction();
    let c = tx.intern(iri("c").as_ref().into());
    let p = tx.intern(iri("p").as_ref().into());
    let o = tx.intern(Literal::new_simple_literal("c1").as_ref().into());
    assert!(tx.insert_inferred(EncodedTriple::new(c, p, o)));
    tx.commit().unwrap();
    let published = engine.snapshot();
    let inferred: Vec<_> = published
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .collect();
    let view = published.with_inferred_subset(InferredSubset::Without(inferred));
    assert!(view.shared_state().is_some(), "the view admits");
    let pattern = of(&view, "p");
    let model = ReadModel::Materialised;
    assert_eq!(
        view.distinct_estimate_in(model, &pattern, Permutation::Gpso, &|_| true),
        Ok(Some(300))
    );
    assert_eq!(
        view.distinct_in(model, &pattern, Permutation::Gpso, &|_| true),
        Ok(Some(301))
    );
    assert_eq!(
        view.statistics().distinct_entries(),
        0,
        "the view keeps none"
    );
    assert_eq!(
        published.statistics().distinct_entries(),
        0,
        "nor the engine's for it"
    );
}

/// - Spec: over several runs and stacks the count is exact, each value once however many
///   runs and stacks hold it; where deletions still shadow values in unmerged runs, it counts
///   every value an unmerged run holds (today's upper bound), never fewer than the visible
///   ones.
/// - Input: subjects `0..3000` of `p` over three commits, each subject in two of them
///   (interleaved: commit k holds `s{i} p "o{i}-{k}"` for the `i` with `i % 3 != k`); the
///   inferred stack holds
///   `p` for subjects `2500..3500`; a fourth commit deletes every asserted quad of subjects
///   `0..500`. No maintenance, so no run is merged (checked).
/// - Expected: before the deletion 3000 asserted and 3500 materialised; after it 3000 and
///   3500 again (the deleted subjects' values are still held by the older runs), with 2500
///   and 3000 subjects visible.
#[test]
fn several_runs_and_deletions_give_todays_numbers() {
    let engine = Engine::new(config()).unwrap();
    for k in 0..3 {
        commit_tagged(
            &engine,
            "p",
            (0..3000).filter(move |i| i % 3 != k),
            &format!("-{k}"),
        );
    }
    let mut tx = engine.transaction();
    let p = tx.intern(iri("p").as_ref().into());
    for i in 2500..3500 {
        let s = tx.intern(iri(&format!("s{i}")).as_ref().into());
        let o = tx.intern(Literal::new_simple_literal("i").as_ref().into());
        assert!(tx.insert_inferred(EncodedTriple::new(s, p, o)));
    }
    tx.commit().unwrap();
    let counts = |snapshot: &Snapshot| {
        let pattern = of(snapshot, "p");
        [ReadModel::Asserted, ReadModel::Materialised].map(|model| {
            snapshot
                .distinct_in(model, &pattern, Permutation::Gpso, &|_| true)
                .unwrap()
                .unwrap()
        })
    };
    let visible = |snapshot: &Snapshot, model: ReadModel| {
        let pattern = of(snapshot, "p");
        let subjects: std::collections::HashSet<_> = snapshot
            .quads_for_pattern_in(model, &pattern)
            .map(|quad| quad.subject)
            .collect();
        subjects.len() as u64
    };
    let snapshot = engine.snapshot();
    assert!(
        snapshot.data(MODEL).stack(Stack::Asserted).runs().len() >= 3,
        "the asserted subjects lie in several runs"
    );
    assert_eq!(counts(&snapshot), [3000, 3500]);

    let mut tx = engine.transaction();
    for i in 0..500 {
        for k in (0..3).filter(|k| i % 3 != *k) {
            assert!(tx.remove(quad("p", i, &format!("-{k}")).as_ref()));
        }
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let runs = snapshot.data(MODEL).stack(Stack::Asserted).runs().len();
    assert!(
        runs >= 4,
        "the deletions lie in a run of their own: {runs} runs"
    );
    assert_eq!(visible(&snapshot, ReadModel::Asserted), 2500);
    assert_eq!(visible(&snapshot, ReadModel::Materialised), 3000);
    assert_eq!(counts(&snapshot), [3000, 3500]);
}
