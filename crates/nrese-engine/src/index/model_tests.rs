//! Differential test: random operation sequences against a `BTreeSet` model (E2 gate).
//!
//! The universe is kept small so inserts and deletes collide constantly, which exercises
//! tombstones, re-inserts and cancellation in merges. Every step checks the count, point
//! lookups and every pattern shape (8 bound/unbound combinations x 4 graph selectors).
//! Every test runs for both layouts; the inferred stack's layout draws default-graph quads
//! only, and is still queried with all graph selectors.

use std::collections::BTreeSet;
use std::sync::Arc;

use nrese_exec::ChargedVec;

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
                let budget = nrese_exec::Budget::unlimited();
                let mut counts = ChargedVec::new(&budget);
                version
                    .group_counts(&plan, bound, &mut counts, &budget)
                    .expect("an unlimited budget");
                let mut totals = std::collections::BTreeMap::new();
                for &(value, count) in counts.iter() {
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
    let budget = nrese_exec::Budget::unlimited();
    let mut counts = ChargedVec::new(&budget);
    version
        .group_counts(&plan, 1, &mut counts, &budget)
        .expect("an unlimited budget");
    let mut totals = std::collections::BTreeMap::new();
    for &(value, count) in counts.iter() {
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

/// `rows` quads of one predicate, the `i`-th with subject `subject(i)`, and the plan of their
/// group walk by subject.
fn subject_walk(rows: u64, subject: impl Fn(u64) -> u64) -> (IndexVersion, AccessPlan) {
    let quads: Vec<EncodedQuad> = (0..rows)
        .map(|i| EncodedQuad::new(term(subject(i)), term(1), term(10_000 + i), graph(0)))
        .collect();
    let version = IndexVersion::from_quads(Layout::DefaultGraph, quads);
    let pattern = QuadPattern {
        subject: None,
        predicate: Some(term(1)),
        object: None,
        graph: GraphSelector::Any,
    };
    let plan = AccessPlan::in_permutation(&pattern, Permutation::Psog).unwrap();
    (version, plan)
}

/// A group walk's answer: each group's count, or its refusal.
type WalkAnswer = Result<Vec<(u64, i64)>, nrese_exec::BudgetExceeded>;

/// The group walk of [`subject_walk`]'s quads under a budget of `limit` bytes: its answer,
/// what the budget still holds once the walk's groups are dropped, and whether it recorded a
/// refusal.
fn walk_by_subject(
    rows: u64,
    subject: impl Fn(u64) -> u64,
    limit: usize,
) -> (WalkAnswer, usize, bool) {
    let (version, plan) = subject_walk(rows, subject);
    let budget = nrese_exec::Budget::new(limit);
    let answer = {
        let mut counts = ChargedVec::new(&budget);
        version
            .group_counts(&plan, 1, &mut counts, &budget)
            .map(|()| counts.to_vec())
    };
    (answer, budget.used(), budget.refused())
}

/// A walk that fits its budget is answered, however its parts start: two parallel parts of
/// one group each, whose merge appends that one group, need a few hundred bytes, and under
/// 32 KiB they are answered with the group's whole count. The room a part asks for ahead is
/// optional (D16-039, package review r1 SC-R1-01): none of it may refuse the merge.
#[test]
fn a_walk_in_parts_that_fits_its_budget_is_answered() {
    let rows = 2 * super::GROUP_PART as u64;
    let (answer, ..) = walk_by_subject(rows, |_| 1000, 32 << 10);
    let counts = answer.expect("one group fits 32 KiB");
    assert_eq!(counts, [(term(1000).raw(), rows as i64)]);
}

/// Parts of uneven size, as the parts of a real walk are: one of a single group, one of 513.
/// Grown as the groups come, they peak near 24.7 KiB with their merge, and under 32 KiB the
/// walk is answered: no part's room asked ahead and left unfilled may take the merge's
/// (package fix check r2: a part held 16 KiB for one group, and the merge of 514 was refused).
#[test]
fn a_walk_in_uneven_parts_that_fits_its_budget_is_answered() {
    let part = super::GROUP_PART as u64;
    let subject = |i: u64| match i < part {
        true => 1000,
        false => 2000 + (i - part) * 513 / part,
    };
    let (answer, ..) = walk_by_subject(2 * part, subject, 32 << 10);
    let counts = answer.expect("514 groups and their parts fit 32 KiB");
    let mut expected = std::collections::BTreeMap::new();
    for i in 0..2 * part {
        *expected.entry(term(subject(i)).raw()).or_insert(0i64) += 1;
    }
    assert_eq!(counts.len(), 514);
    assert_eq!(counts, expected.into_iter().collect::<Vec<_>>());
}

/// A walk leaves nothing charged once its groups are dropped, answered or refused: its parts,
/// their growth and the merge's are all released (package fix check r2).
#[test]
fn a_walk_releases_every_charge_answered_or_refused() {
    let part = super::GROUP_PART as u64;
    // Groups of two quads: 65,536 of them, about 1 MiB, past 64 KiB and within 8 MiB.
    let (answered, held, _) = walk_by_subject(2 * part, |i| 1000 + i / 2, 8 << 20);
    assert_eq!(
        answered.ok().map(|counts| counts.len()),
        Some(part as usize)
    );
    assert_eq!(held, 0, "an answered walk's charges");
    let (refused, held, _) = walk_by_subject(2 * part, |i| 1000 + i / 2, 64 << 10);
    assert!(refused.is_err(), "65,536 groups need more than 64 KiB");
    assert_eq!(held, 0, "a refused walk's charges");
}

/// A walk is refused only where the serial walk would be (package fix check r3). Two parts of
/// 16,385 disjoint groups each, under 1.5 MiB: grown in parallel, each holds capacity 32,768
/// (1 MiB together) and the merge's 32,770 groups need 32 bytes more than is left; the serial
/// walk, one buffer doubling to 65,536 beside its 32,768, peaks at exactly 1.5 MiB and fits.
/// Answered with every count, no refusal recorded, nothing held once dropped.
#[test]
fn a_walk_the_serial_walk_fits_is_answered() {
    let part = super::GROUP_PART as u64;
    let subject = |i: u64| 1000 + i * 16_385 / part;
    let (answer, held, refused) = walk_by_subject(2 * part, subject, 1_572_864);
    let counts = answer.expect("the serial walk fits 1.5 MiB");
    let mut expected = std::collections::BTreeMap::new();
    for i in 0..2 * part {
        *expected.entry(term(subject(i)).raw()).or_insert(0i64) += 1;
    }
    assert_eq!(counts.len(), 32_770);
    assert_eq!(counts, expected.into_iter().collect::<Vec<_>>());
    assert!(!refused, "no refusal recorded on the budget");
    assert_eq!(held, 0);
}

/// The path is the whole walk's (package fix check r4). Two runs under 6,291,584 bytes: the
/// first, 131,072 keys in two parts of 32,769 disjoint groups each, fits its parts' peak
/// exactly; its merge leaves the output at capacity 65,538, from which the second run's
/// 131,075 groups, walked serially, double to a growth 16 bytes past the limit. The fully
/// serial walk, one buffer doubling to 262,144, peaks 128 bytes under it and answers.
/// Answered with every count, no refusal recorded, nothing held once dropped.
#[test]
fn a_walk_over_runs_the_serial_walk_fits_is_answered() {
    let part = super::GROUP_PART as u64;
    let first: Vec<EncodedQuad> = (0..2 * part)
        .map(|i| {
            let subject = 1000 + i * 32_769 / part;
            EncodedQuad::new(term(subject), term(1), term(10_000 + i), graph(0))
        })
        .collect();
    let second: Vec<EncodedQuad> = (0..2 * part + 3)
        .map(|i| EncodedQuad::new(term(1_000_000 + i), term(1), term(2_000_000 + i), graph(0)))
        .collect();
    let mut expected = std::collections::BTreeMap::new();
    for quad in first.iter().chain(&second) {
        *expected.entry(quad.subject.raw()).or_insert(0i64) += 1;
    }
    let version = IndexVersion::from_quads(Layout::DefaultGraph, first).with_delta(&second, &[]);
    let pattern = QuadPattern {
        subject: None,
        predicate: Some(term(1)),
        object: None,
        graph: GraphSelector::Any,
    };
    let plan = AccessPlan::in_permutation(&pattern, Permutation::Psog).unwrap();
    let budget = nrese_exec::Budget::new(6_291_584);
    let answer = {
        let mut counts = ChargedVec::new(&budget);
        version
            .group_counts(&plan, 1, &mut counts, &budget)
            .map(|()| counts.to_vec())
    };
    let counts = answer.expect("the fully serial walk fits 6,291,584 bytes");
    assert_eq!(counts.len(), 196_613);
    assert_eq!(counts, expected.into_iter().collect::<Vec<_>>());
    assert!(!budget.refused(), "no refusal recorded on the budget");
    assert_eq!(budget.used(), 0);
}

/// Under an ample budget the walks above still run in parallel parts, so the normal case
/// keeps its speed; under their budgets they are walked serially, the whole walk.
#[test]
fn a_walk_with_room_for_its_parts_peak_runs_in_parts() {
    let keys = 2 * super::GROUP_PART;
    let ample = nrese_exec::Budget::new(64 << 20);
    assert!(super::walks_in_parts(&ample, keys, 2));
    assert!(
        super::walks_in_parts(&ample, 2 * keys + 3, 5),
        "both runs in parts"
    );
    let tight = nrese_exec::Budget::new(1_572_864);
    assert!(!super::walks_in_parts(&tight, keys, 2));
    let tight = nrese_exec::Budget::new(6_291_584);
    assert!(
        !super::walks_in_parts(&tight, 2 * keys + 3, 5),
        "neither run in parts"
    );
    assert!(
        !super::walks_in_parts(&ample, keys - 1, 0),
        "a walk without a run of two parts is walked serially"
    );
}

/// A walk never charges more than its bound, in parts or serially (`group_walk_peak`): a node
/// count runs its two walks at once only where the room covers both bounds, so a bound short
/// of a walk's peak would let the pair be refused where the walks one after the other fit.
#[test]
fn a_walk_charges_at_most_its_bound() {
    const PART: u64 = super::GROUP_PART as u64;
    // Rows, each row's subject, and whether the range is walked in parts.
    type Shape = (u64, fn(u64) -> u64, bool);
    let shapes: [Shape; 4] = [
        (2 * PART, |_| 1000, true),
        (2 * PART, |i| 1000 + i, true),
        (2 * PART, |i| 1000 + i * 16_385 / PART, true),
        (2 * PART - 1, |i| 1000 + i, false),
    ];
    for (rows, subject, in_parts) in shapes {
        let (version, plan) = subject_walk(rows, subject);
        let (keys, parts) = version.group_walk_size(&plan, 1);
        assert_eq!((keys, parts > 0), (rows as usize, in_parts), "{rows} rows");
        let budget = nrese_exec::Budget::new(64 << 20);
        let mut counts = ChargedVec::new(&budget);
        version
            .group_counts(&plan, 1, &mut counts, &budget)
            .expect("64 MiB fits the walk");
        assert!(budget.peak() > 0);
        assert!(
            budget.peak() <= super::group_walk_peak(keys, parts),
            "{rows} rows: peak {} over the bound {}",
            budget.peak(),
            super::group_walk_peak(keys, parts)
        );
    }
}

/// Every layout keeps the permutation each pattern shape's access plan reads, the pattern's
/// bound components a prefix of it: an equality expansion merges its matches in that order
/// as it reads them, collecting none (`Snapshot::expanded`).
#[test]
fn every_layout_keeps_every_access_plans_permutation() {
    let selectors = [
        GraphSelector::Any,
        GraphSelector::AnyNamed,
        GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        GraphSelector::Exact(graph(1)),
    ];
    for graph in selectors {
        for bits in 0..8u8 {
            let bound = |bit: u8| (bits & bit != 0).then(|| term(1));
            let pattern = QuadPattern {
                subject: bound(1),
                predicate: bound(2),
                object: bound(4),
                graph,
            };
            let permutation = AccessPlan::for_pattern(&pattern).permutation;
            assert!(
                AccessPlan::in_permutation(&pattern, permutation).is_some(),
                "{pattern:?}"
            );
            for layout in LAYOUTS {
                assert!(layout.supports(permutation), "{pattern:?} {layout:?}");
            }
        }
    }
}
