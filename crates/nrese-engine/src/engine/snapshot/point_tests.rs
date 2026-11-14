//! A point read answers what a pattern read answers (D16-543, design note
//! `2026-11-14-d16-543-point-reads-design.md`, test 1): [`CountedSnapshot::point_quads`] against
//! [`CountedSnapshot::quads`] over random commits. A small universe makes inserts, deletes and
//! re-inserts collide in the default and named graphs, inferred statements come and go, and a
//! policy that merges no window keeps each commit's run, so reads meet tombstones across runs.
//! First, a statement in every graph loses one graph's copy, then every copy. Varied by
//! `NRESE_FUZZ_SEED`.

use nrese_rdf::NamedNode;

use crate::quad::{AccessPlan, GraphSelector, QuadPattern};
use crate::{
    CompactionPolicy, EncodedQuad, EncodedTriple, Engine, EngineConfig, ReadModel, Snapshot, TermId,
};

const TERMS: u64 = 3;
const GRAPHS: u64 = 3; // the default graph and two named ones
const COMMITS: usize = 24;

/// `seed` varied by `NRESE_FUZZ_SEED` (a number); without it, the same cases on every run.
fn fuzz(seed: u64) -> u64 {
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    seed ^ fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
}

#[test]
fn a_point_read_answers_the_quads_a_pattern_read_answers() {
    for seed in 0..4 {
        let mut rng = Rng(fuzz(seed));
        let compaction = match seed % 2 {
            0 => CompactionPolicy {
                fanout: 0,
                ..CompactionPolicy::default()
            },
            _ => CompactionPolicy::default(),
        };
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            compaction,
            ..EngineConfig::default()
        })
        .unwrap();
        let tx = engine.transaction();
        let iri = |name: String| tx.intern(NamedNode::new_unchecked(name).as_ref().into());
        let terms: Vec<TermId> = (0..TERMS).map(|i| iri(format!("urn:t{i}"))).collect();
        let graphs: Vec<TermId> = std::iter::once(TermId::DEFAULT_GRAPH)
            .chain((1..GRAPHS).map(|g| iri(format!("urn:g{g}"))))
            .collect();
        tx.commit().unwrap();
        // A statement in every graph, then one graph's copy deleted, then every copy.
        let point = [terms[0]; 3];
        let copies: Vec<_> = graphs
            .iter()
            .map(|&graph| EncodedQuad::new(point[0], point[1], point[2], graph))
            .collect();
        for (step, change) in [&copies[..], &copies[1..2], &copies[..]]
            .into_iter()
            .enumerate()
        {
            let mut tx = engine.transaction();
            for &copy in change {
                match step {
                    0 => tx.insert_encoded(copy),
                    _ => tx.remove_encoded(copy),
                };
            }
            tx.commit().unwrap();
            let case = format!("seed {seed}, deletion step {step}");
            let snapshot = engine.snapshot();
            same_answers(&snapshot, point, &case);
            let held = snapshot
                .counted()
                .point_quads(ReadModel::Asserted, point, &|_| true)
                .count();
            assert_eq!(held, [3, 2, 0][step], "{case}: the copies left");
        }
        for commit in 0..COMMITS {
            let mut tx = engine.transaction();
            for _ in 0..=rng.below(6) {
                let [s, p, o] = [(); 3].map(|()| terms[rng.below(TERMS) as usize]);
                let graph = graphs[rng.below(GRAPHS) as usize];
                match rng.below(4) {
                    0 => tx.insert_inferred(EncodedTriple::new(s, p, o)),
                    1 => tx.remove_inferred(EncodedTriple::new(s, p, o)),
                    2 => tx.remove_encoded(EncodedQuad::new(s, p, o, graph)),
                    _ => tx.insert_encoded(EncodedQuad::new(s, p, o, graph)),
                };
            }
            tx.commit().unwrap();
            let snapshot = engine.snapshot();
            for s in &terms {
                for p in &terms {
                    for o in &terms {
                        let case = format!("seed {seed}, commit {commit}");
                        same_answers(&snapshot, [*s, *p, *o], &case);
                    }
                }
            }
        }
    }
}

/// The point read of `point` in each read model: its quads in the pattern read's order, read
/// on the point path (making its scans fits one visit, as these few runs' does).
fn same_answers(snapshot: &Snapshot, point: [TermId; 3], case: &str) {
    let [s, p, o] = point.map(Some);
    let pattern = QuadPattern {
        subject: s,
        predicate: p,
        object: o,
        graph: GraphSelector::Any,
    };
    let plan = AccessPlan::for_pattern(&pattern);
    let going = |_: u64| true;
    let counted = snapshot.counted();
    for model in [
        ReadModel::Materialised,
        ReadModel::Asserted,
        ReadModel::Inferred,
    ] {
        let cost = snapshot.point_cost(model, &plan);
        assert!(cost.is_some(), "{case}, {model:?}: the point path");
        let read: Vec<_> = counted.quads(model, &pattern, &going).collect();
        let point_read: Vec<_> = counted.point_quads(model, point, &going).collect();
        assert_eq!(point_read, read, "{case}, {model:?}, {point:?}");
    }
}
