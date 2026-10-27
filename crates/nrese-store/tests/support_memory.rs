//! Guard of the support graph sets' memory (`inferred = "supported"`): what the sets keep
//! for readers (the interned sets, the facts' entries, the cap's marks, the axioms and the
//! schema) is charged before it is allocated, released when they go, and grows with the
//! facts kept. The evaluation the sets come from is made before the measurement: its
//! working set is watched, not charged before it is allocated (the W1 exemption,
//! `nrese_reasoner::graph_sets`). Measured against a budget (`nrese_exec::measure`), so this
//! file is a test binary of its own.

use std::sync::Arc;

use hashbrown::{HashMap, HashSet};
use nrese_engine::TermId;
use nrese_exec::{Budget, ChargedVec, measure};
use nrese_reasoner::graph_sets::{Annotation, GraphSet};
use nrese_store::support::SupportSets;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// Graphs the facts are spread over.
const GRAPHS: u32 = 8;

/// An evaluation's sets by fact, and each asserted fact with a graph that holds it.
type Evaluation = (HashMap<[u64; 3], Annotation>, Vec<([u64; 3], u32)>);

/// An evaluation's sets for `facts` inferred facts, each with three alternatives of two
/// graphs (every fourth past a cap of two, so marked), and as many asserted facts in one
/// graph each; with the asserted facts' graphs, sorted.
fn evaluation(facts: u64) -> Evaluation {
    let mut by_fact = HashMap::new();
    let mut asserted = Vec::new();
    for i in 0..facts {
        let g = (i % u64::from(GRAPHS)) as u32;
        let pair = |a: u32, b: u32| GraphSet::of(a).union(&GraphSet::of(b));
        by_fact.insert(
            [1, 2, 1_000 + i],
            Annotation {
                sets: vec![
                    pair(g, (g + 1) % GRAPHS),
                    pair(g, (g + 2) % GRAPHS),
                    pair(g, (g + 3) % GRAPHS),
                ],
                truncated: i % 4 == 0,
                beyond_own: i % 4 == 0,
            },
        );
        let fact = [1, 3, 1_000 + i];
        by_fact.insert(
            fact,
            Annotation {
                sets: vec![GraphSet::of(g)],
                truncated: false,
                beyond_own: false,
            },
        );
        asserted.push((fact, g));
    }
    asserted.sort_unstable();
    (by_fact, asserted)
}

/// The sets an evaluation gives are kept within their charge, at 500 and at eight times
/// as many facts: only the inferred facts' entries (the asserted ones have the sets they
/// start with), and the charge follows them.
#[test]
fn kept_sets_are_charged_before_they_are_made() {
    if !measure::alone() {
        return;
    }
    let mut charged = Vec::new();
    for facts in [500, 4_000] {
        let (by_fact, asserted) = evaluation(facts);
        let schema: HashSet<[u64; 3]> = HashSet::new();
        let budget = Arc::new(Budget::unlimited());
        let ((sets, used), excess) = measure::measure_within(&budget, || {
            let graphs = ChargedVec::collect(
                Arc::clone(&budget),
                GRAPHS as usize,
                (0..u64::from(GRAPHS)).map(TermId::from_raw),
            )
            .unwrap();
            let sets =
                SupportSets::retain(graphs, &by_fact, &asserted, &[], &schema, 3, &budget).unwrap();
            let used = budget.used();
            (sets, used)
        });
        assert_eq!(excess, 0, "{facts} facts: {excess} bytes beyond the charge");
        let (kept, entries, marked) = sets.len();
        assert_eq!(
            (kept, entries, marked),
            (
                facts as usize,
                3 * facts as usize,
                facts.div_ceil(4) as usize
            )
        );
        drop(sets);
        assert_eq!(budget.used(), 0, "{facts} facts: still charged");
        charged.push(used);
    }
    assert!(charged[1] > 6 * charged[0], "{charged:?}");
}
