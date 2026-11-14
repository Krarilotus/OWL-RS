//! The planner's shared-values read ([`Context::overlaps`]): a count comes only from two
//! lists read in full and charged to the query's budget, for patterns whose own counts are
//! small; any other read leaves the plain estimate (D16-575).

use super::join_order_tests::scan;
use super::*;
use nrese_engine::{EncodedTriple, Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

fn e(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://e/{name}"))
}

/// An engine that merges no runs: each commit stays a run of the index (a policy that forms
/// no window).
fn engine() -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        compaction: nrese_engine::CompactionPolicy {
            fanout: 0,
            ..nrese_engine::CompactionPolicy::default()
        },
        ..EngineConfig::default()
    })
    .unwrap()
}

/// `a p oI` and `b q oI` for I below `n`: the two patterns share all `n` values. Written in
/// `commits` commits, each a run of the index.
fn shared(n: usize, commits: usize) -> Engine {
    let engine = engine();
    for c in 0..commits {
        let mut tx = engine.transaction();
        for i in (c..n).step_by(commits) {
            for (s, p) in [("a", "p"), ("b", "q")] {
                let quad = Quad::new(e(s), e(p), e(&format!("o{i}")), GraphName::DefaultGraph);
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
    }
    engine
}

/// A query's options with `limit` bytes of memory (`None`: unlimited).
fn within(limit: Option<usize>) -> QueryOptions {
    QueryOptions {
        resources: crate::Resources {
            memory: nrese_exec::Envelope::new(limit, None),
            ..crate::Resources::default()
        },
        ..QueryOptions::default()
    }
}

/// The shared values `ctx` records for `a p ?x` and `b q ?x`, planned with `counts`: per
/// pattern, the other and the count.
fn recorded(ctx: &Context<'_>, counts: [u64; 2]) -> Vec<Vec<(usize, u64)>> {
    let scans = [scan(&ctx.snapshot, "a p ?x"), scan(&ctx.snapshot, "b q ?x")];
    ctx.plan_inputs(&scans, &counts, &[], &mut Vec::new())
        .into_iter()
        .map(|input| input.overlaps)
        .collect()
}

fn counted(n: u64) -> Vec<Vec<(usize, u64)>> {
    vec![vec![(1, n)], vec![(0, n)]]
}

fn none() -> Vec<Vec<(usize, u64)>> {
    vec![Vec::new(), Vec::new()]
}

/// D16-575 E2 (doubt review r1 F2): the read holds its lists inside the query's memory
/// envelope.
///
/// - Input: `a p ?x` and `b q ?x` sharing all of their 1,000 values, written in one commit
///   (one run: read a column at a time, charged before it decodes) or in two (two runs:
///   merged only quad by quad); a query of 4 KiB, less than either list's 8,000 bytes, and
///   an unlimited one.
/// - Expected: within 4 KiB no count in either store (a list it held would be past the
///   envelope); unlimited, all 1,000 in one run and none in two, whose lists aren't held at
///   all, so they can't be held outside the envelope.
#[test]
fn shared_values_lists_stay_inside_the_query_envelope() {
    for (commits, limit, expected) in [
        (1, None, counted(1_000)),
        (1, Some(4 << 10), none()),
        (2, Some(4 << 10), none()),
        (2, None, none()),
    ] {
        let engine = shared(1_000, commits);
        let snapshot = engine.snapshot();
        let options = within(limit);
        let ctx = Context::new(&snapshot, &options, None, None);
        assert_eq!(
            recorded(&ctx, [1_000, 1_000]),
            expected,
            "{commits} commits, limit {limit:?}"
        );
    }
}

/// D16-575 E2: a pair is read where its patterns' own counts are small, not where the
/// counts it is planned with are: those may be narrowed by a filter or a range, while the
/// read decodes the whole pattern.
///
/// - Input: `a p ?x` and `b q ?x` in one commit, sharing all their `n` values, planned with
///   counts of 1 each: `n` = 100 (200 together, within [`plan::OVERLAP_VALUES`]) and 5,000
///   (10,000, past it).
/// - Expected: 100 values counted; for 5,000 none, and nothing charged: no list decoded.
#[test]
fn pairs_are_read_by_their_patterns_own_counts_not_narrowed_ones() {
    for (n, expected) in [(100, counted(100)), (5_000, none())] {
        let engine = shared(n, 1);
        let snapshot = engine.snapshot();
        let options = QueryOptions::default();
        let ctx = Context::new(&snapshot, &options, None, None);
        let peak = ctx.budget.peak();
        assert_eq!(recorded(&ctx, [1, 1]), expected, "{n} values");
        if n > 100 {
            assert_eq!(ctx.budget.peak(), peak, "{n} values: a list decoded");
        }
    }
}

/// D16-575 E1 (doubt review r1 F1): a read the budget stops part way is no count.
///
/// - Input: `a p rep` and `b q rep` stated, and 999 aliases `owl:sameAs`-equal to `rep`
///   (inferred), read with early equality expansion: `a p ?x` and `b q ?x` each match the
///   class's 1,000 terms and share all of them. Query memory from 1 KiB to 16 MiB: an
///   expanding read holds its copies until they are in order, and a limit stops it.
/// - Expected: at every limit, no count or all 1,000 values; never the count of what was
///   read before a refusal.
#[test]
fn a_refused_expanding_read_gives_no_shared_values_count() {
    let engine = engine();
    let mut tx = engine.transaction();
    let rep = tx.intern(e("rep").as_ref().into());
    let same_as = tx.intern(e("sameAs").as_ref().into());
    for i in 1..1_000 {
        let alias = tx.intern(e(&format!("alias{i}")).as_ref().into());
        tx.insert_inferred(EncodedTriple {
            subject: alias,
            predicate: same_as,
            object: rep,
        });
    }
    for (s, p) in [("a", "p"), ("b", "q")] {
        tx.insert(Quad::new(e(s), e(p), e("rep"), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let snapshot = engine.snapshot();
    for shift in 10..=24 {
        let options = QueryOptions {
            equality_early_expansion: true,
            ..within(Some(1 << shift))
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let overlaps = recorded(&ctx, [1, 1]);
        assert!(
            overlaps == none() || overlaps == counted(1_000),
            "{} bytes: {overlaps:?}",
            1 << shift
        );
    }
}
