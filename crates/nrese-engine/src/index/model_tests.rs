//! Differential test: random operation sequences against a `BTreeSet` model (E2 gate).
//!
//! The universe is kept small so inserts and deletes collide constantly, which exercises
//! tombstones, re-inserts and cancellation in merges. Every step checks the count, point
//! lookups and every pattern shape (8 bound/unbound combinations x 4 graph selectors).
//! Every test runs for both layouts; the inferred stack's layout draws default-graph quads
//! only, and is still queried with all graph selectors.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::compaction::{CompactionPolicy, merge_runs};
use super::run::Run;
use super::{IndexVersion, Layout};
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};
use crate::term::{TermId, TermKind};

/// `seed` varied by `NRESE_FUZZ_SEED` (a number) for bug hunts over many seeds; without
/// it, the same cases on every run.
fn fuzz(seed: u64) -> u64 {
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    seed ^ fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}
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

const LAYOUTS: [Layout; 2] = [Layout::Quads, Layout::DefaultGraph];

fn random_quad(layout: Layout, rng: &mut Rng) -> EncodedQuad {
    let graphs = match layout {
        Layout::Quads => GRAPHS,
        Layout::DefaultGraph => 1,
    };
    EncodedQuad::new(
        term(rng.below(TERMS)),
        term(rng.below(TERMS)),
        term(rng.below(TERMS)),
        graph(rng.below(graphs)),
    )
}

/// Applies a random batch as an exact delta, the way the transaction layer does.
fn random_commit(layout: Layout, rng: &mut Rng, model: &mut BTreeSet<EncodedQuad>) -> Run {
    let (inserts, deletes) = random_delta(layout, rng, model);
    Run::from_delta(layout, &inserts, &deletes)
}

/// A random exact delta of quads `layout` can hold, applied to `model`: inserts, deletes.
fn random_delta(
    layout: Layout,
    rng: &mut Rng,
    model: &mut BTreeSet<EncodedQuad>,
) -> (Vec<EncodedQuad>, Vec<EncodedQuad>) {
    let mut inserts = BTreeSet::new();
    let mut deletes = BTreeSet::new();
    for _ in 0..=rng.below(12) {
        let quad = random_quad(layout, rng);
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
    (inserts.into_iter().collect(), deletes.into_iter().collect())
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
        let plan = AccessPlan::for_pattern(&pattern);
        assert_eq!(
            version.count_plan(&plan),
            expected.len() as u64,
            "{context}: exact count for {pattern:?}"
        );
        // Every supported permutation whose order the pattern binds a prefix of yields the
        // same quads, sorted by that permutation's key.
        for permutation in Permutation::ALL {
            let Some(plan) = AccessPlan::in_permutation(&pattern, permutation) else {
                continue;
            };
            if !version.layout.supports(permutation) {
                continue;
            }
            let got: Vec<_> = version.scan_plan(&plan).collect();
            let mut sorted = expected.clone();
            sorted.sort_by_key(|q| permutation.to_key(q));
            assert_eq!(got, sorted, "{context}: {pattern:?} in {permutation:?}");
            assert_eq!(
                version.count_plan(&plan),
                expected.len() as u64,
                "{context}: count of {pattern:?} in {permutation:?}"
            );
            // Group counts by the first free key position equal grouping the model.
            let bound = plan
                .low
                .iter()
                .zip(&plan.high)
                .take_while(|(l, h)| l == h)
                .count();
            if bound < 4 && !plan.exclude_default_graph {
                let mut counts = Vec::new();
                version.group_counts(&plan, bound, &mut counts);
                let mut totals = std::collections::BTreeMap::new();
                for (value, count) in counts {
                    *totals.entry(value).or_insert(0i64) += count;
                }
                totals.retain(|_, count| *count != 0);
                let mut grouped = std::collections::BTreeMap::new();
                for quad in &expected {
                    *grouped
                        .entry(permutation.to_key(quad)[bound])
                        .or_insert(0i64) += 1;
                }
                assert_eq!(
                    totals, grouped,
                    "{context}: group counts of {pattern:?} in {permutation:?}"
                );
            }
        }
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
    for layout in LAYOUTS {
        for seed in 0..40 {
            let mut rng = Rng(fuzz(seed));
            let mut model = BTreeSet::new();
            let mut version = IndexVersion::empty(layout);
            for step in 0..60 {
                version = version.with_run(random_commit(layout, &mut rng, &mut model));
                if let Some(plan) = policy.plan(version.runs()) {
                    let merged = merge_runs(&version.runs()[plan.window.clone()]);
                    version = version.with_compacted(plan.window, merged);
                }
                let context = format!("{layout:?} seed {seed} step {step}");
                assert_matches_model(&version, &model, &context);
                // Probes include named-graph quads, which the default-graph layout never holds.
                let probe = random_quad(Layout::Quads, &mut rng);
                assert_eq!(
                    version.contains(&probe),
                    model.contains(&probe),
                    "{context}"
                );
            }
        }
    }
}

#[test]
fn arbitrary_compaction_windows_preserve_contents() {
    for layout in LAYOUTS {
        for seed in 100..140 {
            let mut rng = Rng(fuzz(seed));
            let mut model = BTreeSet::new();
            let mut version = IndexVersion::empty(layout);
            for step in 0..40 {
                version = version.with_run(random_commit(layout, &mut rng, &mut model));
                let runs = version.runs().len() as u64;
                if runs >= 2 && rng.below(3) == 0 {
                    let start = rng.below(runs - 1) as usize;
                    let end = start + 2 + rng.below(runs - start as u64 - 1) as usize;
                    let merged = merge_runs(&version.runs()[start..end]);
                    version = version.with_compacted(start..end, merged);
                }
                let context = format!("{layout:?} seed {seed} step {step}");
                assert_matches_model(&version, &model, &context);
            }
        }
    }
}

#[test]
fn old_versions_are_unaffected_by_later_writes() {
    let mut rng = Rng(fuzz(7));
    let mut model = BTreeSet::new();
    let mut version = IndexVersion::default();
    let mut history = Vec::new();
    for _ in 0..30 {
        version = version.with_run(random_commit(Layout::Quads, &mut rng, &mut model));
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
    let version = IndexVersion::from_quads(Layout::Quads, vec![quad, quad]);
    assert_eq!(version.len(), 1);
    assert_eq!(Arc::strong_count(&version.runs()[0]), 1);
}

#[test]
fn default_graph_layout_stores_four_of_seven_permutations() {
    let quads: Vec<_> = (0..4)
        .map(|n| EncodedQuad::new(term(n), term(1), term(2), graph(0)))
        .collect();
    let quads_bytes =
        IndexVersion::from_quads(Layout::Quads, quads.clone()).runs()[0].memory_bytes();
    let triples = IndexVersion::from_quads(Layout::DefaultGraph, quads);
    assert_eq!(triples.runs()[0].memory_bytes() * 7, quads_bytes * 4);
}

/// A default-graph version turns into a quad version at its first quad in a named graph:
/// the same answers before and after in every permutation, tombstones and all, and its
/// runs (converted and new) still compact.
#[test]
fn default_graph_versions_turn_into_quad_versions_at_a_named_graph() {
    let policy = CompactionPolicy {
        fanout: 2,
        ..CompactionPolicy::default()
    };
    for seed in 200..230 {
        let mut rng = Rng(fuzz(seed));
        let mut model = BTreeSet::new();
        let mut version = IndexVersion::empty(Layout::DefaultGraph);
        let mut named = false;
        for step in 0..50 {
            // Default-graph quads first, then any.
            let layout = match step < 20 {
                true => Layout::DefaultGraph,
                false => Layout::Quads,
            };
            let (inserts, deletes) = random_delta(layout, &mut rng, &mut model);
            named |= Layout::holding(&inserts) == Layout::Quads;
            version = version.with_delta(&inserts, &deletes);
            let expected = match named {
                true => Layout::Quads,
                false => Layout::DefaultGraph,
            };
            assert_eq!(version.layout(), expected, "seed {seed} step {step}");
            if step % 3 == 0
                && let Some(plan) = policy.plan(version.runs())
            {
                let merged = merge_runs(&version.runs()[plan.window.clone()]);
                version = version.with_compacted(plan.window, merged);
            }
            assert_matches_model(&version, &model, &format!("seed {seed} step {step}"));
        }
    }
}

/// Group counts over ranges large enough to be walked in parallel parts: groups that span
/// the parts' cuts are added up once, and deletions in a later run still count.
#[test]
fn group_counts_of_large_ranges_add_up_across_parts() {
    // Subject s has 2s + 1 quads: groups of every size, many across the cuts.
    let quads: Vec<EncodedQuad> = (0..200_000u64)
        .map(|i| {
            let s = (i as f64).sqrt() as u64;
            EncodedQuad::new(term(1000 + s), term(1), term(10_000 + i), graph(0))
        })
        .collect();
    let deletes: Vec<EncodedQuad> = quads.iter().step_by(7).copied().collect();
    let version =
        IndexVersion::from_quads(Layout::DefaultGraph, quads.clone()).with_delta(&[], &deletes);
    let pattern = QuadPattern {
        subject: None,
        predicate: Some(term(1)),
        object: None,
        graph: GraphSelector::Any,
    };
    let plan = AccessPlan::in_permutation(&pattern, Permutation::Psog).unwrap();
    let mut counts = Vec::new();
    version.group_counts(&plan, 1, &mut counts);
    let mut totals = std::collections::BTreeMap::new();
    for (value, count) in counts {
        *totals.entry(value).or_insert(0i64) += count;
    }
    totals.retain(|_, count| *count != 0);
    let deleted: BTreeSet<EncodedQuad> = deletes.into_iter().collect();
    let mut expected = std::collections::BTreeMap::new();
    for quad in quads.iter().filter(|q| !deleted.contains(q)) {
        *expected.entry(quad.subject.raw()).or_insert(0i64) += 1;
    }
    assert_eq!(totals, expected);
}
