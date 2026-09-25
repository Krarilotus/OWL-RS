//! Differential test: random operation sequences against a `BTreeSet` model (E2 gate).
//!
//! The universe is kept small so inserts and deletes collide constantly, which exercises
//! tombstones, re-inserts and cancellation in merges. Every step checks the count, point
//! lookups and every pattern shape (8 bound/unbound combinations x 4 graph selectors).

use std::collections::BTreeSet;
use std::sync::Arc;

use super::IndexVersion;
use super::compaction::{CompactionPolicy, merge_runs};
use super::run::Run;
use crate::quad::{EncodedQuad, GraphSelector, QuadPattern};
use crate::term::{TermId, TermKind};

/// SplitMix64: tiny, deterministic, good enough for test-case generation.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const TERMS: u64 = 5;
const GRAPHS: u64 = 3; // 0 = default graph

fn term(n: u64) -> TermId {
    TermId::new(TermKind::Iri, n + 1)
}

fn graph(n: u64) -> TermId {
    if n == 0 {
        TermId::DEFAULT_GRAPH
    } else {
        term(10 + n)
    }
}

fn random_quad(rng: &mut Rng) -> EncodedQuad {
    EncodedQuad::new(
        term(rng.below(TERMS)),
        term(rng.below(TERMS)),
        term(rng.below(TERMS)),
        graph(rng.below(GRAPHS)),
    )
}

/// Applies a random batch as an exact delta, the way the transaction layer will.
fn random_commit(rng: &mut Rng, model: &mut BTreeSet<EncodedQuad>) -> Run {
    let mut inserts = BTreeSet::new();
    let mut deletes = BTreeSet::new();
    for _ in 0..=rng.below(12) {
        let quad = random_quad(rng);
        let present =
            (model.contains(&quad) || inserts.contains(&quad)) && !deletes.contains(&quad);
        if rng.below(2) == 0 {
            // Re-inserting a quad deleted earlier in the batch cancels the delete.
            if !present && !deletes.remove(&quad) {
                inserts.insert(quad);
            }
        } else if present && !inserts.remove(&quad) {
            deletes.insert(quad);
        }
    }
    for quad in &inserts {
        model.insert(*quad);
    }
    for quad in &deletes {
        model.remove(quad);
    }
    let inserts: Vec<_> = inserts.into_iter().collect();
    let deletes: Vec<_> = deletes.into_iter().collect();
    Run::from_delta(&inserts, &deletes)
}

fn all_patterns() -> Vec<QuadPattern> {
    let choices = |n: u64| [None, Some(term(n))];
    let graphs = [
        GraphSelector::Any,
        GraphSelector::AnyNamed,
        GraphSelector::Exact(graph(0)),
        GraphSelector::Exact(graph(1)),
    ];
    let mut patterns = Vec::new();
    for subject in choices(1) {
        for predicate in choices(2) {
            for object in choices(0) {
                for graph in graphs {
                    patterns.push(QuadPattern {
                        subject,
                        predicate,
                        object,
                        graph,
                    });
                }
            }
        }
    }
    patterns
}

fn assert_matches_model(version: &IndexVersion, model: &BTreeSet<EncodedQuad>, context: &str) {
    assert_eq!(version.len(), model.len() as u64, "{context}: len");
    let all: BTreeSet<_> = version.scan(&QuadPattern::all()).collect();
    assert_eq!(&all, model, "{context}: full scan");
    for pattern in all_patterns() {
        let mut got: Vec<_> = version.scan(&pattern).collect();
        let before = got.len();
        got.sort();
        got.dedup();
        assert_eq!(before, got.len(), "{context}: duplicates for {pattern:?}");
        let expected: Vec<_> = model
            .iter()
            .filter(|q| pattern.matches(q))
            .copied()
            .collect();
        assert_eq!(got, expected, "{context}: pattern {pattern:?}");
    }
    let graphs: Vec<_> = std::iter::successors(version.next_named_graph(None), |&g| {
        version.next_named_graph(Some(g))
    })
    .collect();
    let expected: BTreeSet<_> = model
        .iter()
        .map(|q| q.graph)
        .filter(|g| !g.is_default_graph())
        .collect();
    assert_eq!(
        graphs,
        expected.into_iter().collect::<Vec<_>>(),
        "{context}: named graphs"
    );
}

#[test]
fn random_commits_with_policy_compaction_match_the_model() {
    let policy = CompactionPolicy {
        fanout: 2,
        ..CompactionPolicy::default()
    };
    for seed in 0..40 {
        let mut rng = Rng(seed);
        let mut model = BTreeSet::new();
        let mut version = IndexVersion::default();
        for step in 0..60 {
            version = version.with_run(random_commit(&mut rng, &mut model));
            if let Some(plan) = policy.plan(version.runs()) {
                let merged = merge_runs(&version.runs()[plan.window.clone()]);
                version = version.with_compacted(plan.window, merged);
            }
            assert_matches_model(&version, &model, &format!("seed {seed} step {step}"));
            let probe = random_quad(&mut rng);
            assert_eq!(version.contains(&probe), model.contains(&probe));
        }
    }
}

#[test]
fn arbitrary_compaction_windows_preserve_contents() {
    for seed in 100..140 {
        let mut rng = Rng(seed);
        let mut model = BTreeSet::new();
        let mut version = IndexVersion::default();
        for step in 0..40 {
            version = version.with_run(random_commit(&mut rng, &mut model));
            let runs = version.runs().len() as u64;
            if runs >= 2 && rng.below(3) == 0 {
                let start = rng.below(runs - 1) as usize;
                let end = start + 2 + rng.below(runs - start as u64 - 1) as usize;
                let merged = merge_runs(&version.runs()[start..end]);
                version = version.with_compacted(start..end, merged);
            }
            assert_matches_model(&version, &model, &format!("seed {seed} step {step}"));
        }
    }
}

#[test]
fn old_versions_are_unaffected_by_later_writes() {
    let mut rng = Rng(7);
    let mut model = BTreeSet::new();
    let mut version = IndexVersion::default();
    let mut history = Vec::new();
    for _ in 0..30 {
        version = version.with_run(random_commit(&mut rng, &mut model));
        history.push((version.clone(), model.clone()));
    }
    let merged = merge_runs(version.runs());
    let compacted = version.with_compacted(0..version.runs().len(), merged);
    assert_eq!(compacted.runs().len(), 1);
    assert_matches_model(&compacted, &model, "fully compacted");
    for (i, (old, old_model)) in history.iter().enumerate() {
        assert_matches_model(old, old_model, &format!("version {i}"));
    }
}

#[test]
fn base_run_from_quads_deduplicates() {
    let quad = EncodedQuad::new(term(0), term(1), term(2), graph(0));
    let version = IndexVersion::from_quads(vec![quad, quad]);
    assert_eq!(version.len(), 1);
    assert_eq!(Arc::strong_count(&version.runs()[0]), 1);
}
