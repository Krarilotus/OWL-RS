//! E6 gate: the inferred stack. Read models, the disjointness rule, and a differential test
//! of both stacks against a two-set model (in memory with compaction, and durable across
//! checkpoints and reopening).

use std::collections::BTreeSet;
use std::sync::OnceLock;

use nrese_engine::{
    EncodedQuad, EncodedTriple, Engine, EngineConfig, GraphSelector, QuadPattern, ReadModel,
    Snapshot, TermId, Transaction, quad::Permutation,
};
use nrese_rdf::NamedNode;

const MODELS: [ReadModel; 3] = [
    ReadModel::Materialised,
    ReadModel::Asserted,
    ReadModel::Inferred,
];

const TERM_COUNT: u64 = 32;

/// Interns `http://example.com/0..32` in order. A fresh dictionary assigns ids
/// deterministically, so every engine prepared this way agrees with [`id`].
fn intern_terms(engine: &Engine) -> Vec<TermId> {
    let tx = engine.transaction();
    (0..TERM_COUNT)
        .map(|n| {
            let iri = NamedNode::new_unchecked(format!("http://example.com/{n}"));
            tx.intern(iri.as_ref().into())
        })
        .collect()
}

fn id(n: u64) -> TermId {
    static IDS: OnceLock<Vec<TermId>> = OnceLock::new();
    IDS.get_or_init(|| intern_terms(&new_engine()))[n as usize]
}

fn new_engine() -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .expect("engine")
}

/// An in-memory engine whose dictionary knows the test terms.
fn engine() -> Engine {
    let engine = new_engine();
    assert_eq!(
        intern_terms(&engine),
        (0..TERM_COUNT).map(id).collect::<Vec<_>>()
    );
    engine
}

fn triple(s: u64, p: u64, o: u64) -> EncodedTriple {
    EncodedTriple::new(id(s), id(p), id(o))
}

fn scan(snapshot: &Snapshot, model: ReadModel) -> BTreeSet<EncodedQuad> {
    snapshot
        .quads_for_pattern_in(model, &QuadPattern::all())
        .collect()
}

#[test]
fn read_models_select_the_stacks() {
    let engine = engine();
    let asserted = triple(1, 2, 3).in_default_graph();
    let inferred = triple(1, 2, 4);
    let mut tx = engine.transaction();
    assert!(tx.insert_encoded(asserted));
    assert!(tx.insert_inferred(inferred));
    assert!(!tx.insert_inferred(inferred), "already inferred");
    let summary = tx.commit().expect("commit");
    assert_eq!(
        (summary.inserted, summary.inferred_inserted),
        (1, 1),
        "{summary:?}"
    );

    let snapshot = engine.snapshot();
    let inferred = inferred.in_default_graph();
    assert_eq!(
        scan(&snapshot, ReadModel::Materialised),
        BTreeSet::from([asserted, inferred])
    );
    assert_eq!(
        scan(&snapshot, ReadModel::Asserted),
        BTreeSet::from([asserted])
    );
    assert_eq!(
        scan(&snapshot, ReadModel::Inferred),
        BTreeSet::from([inferred])
    );
    assert_eq!(
        MODELS.map(|model| snapshot.len_in(model)),
        [2, 1, 1],
        "materialised, asserted, inferred"
    );
    assert!(snapshot.contains(&inferred));
    assert!(!snapshot.contains_in(ReadModel::Asserted, &inferred));
    let stats = engine.stats();
    assert_eq!((stats.quads, stats.inferred), (1, 1));
}

#[test]
fn inferred_statements_live_in_the_default_graph_only() {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert_inferred(triple(1, 2, 3));
    tx.commit().expect("commit");
    let snapshot = engine.snapshot();
    let in_graph = |graph| QuadPattern {
        graph,
        ..QuadPattern::all()
    };
    let count = |pattern: QuadPattern| snapshot.quads_for_pattern(&pattern).count();
    assert_eq!(
        count(in_graph(GraphSelector::Exact(TermId::DEFAULT_GRAPH))),
        1
    );
    assert_eq!(count(in_graph(GraphSelector::AnyNamed)), 0);
    assert_eq!(count(in_graph(GraphSelector::Exact(id(9)))), 0);
    assert_eq!(snapshot.named_graphs().count(), 0);
}

#[test]
fn asserting_an_inferred_statement_makes_it_explicit() {
    let engine = engine();
    let statement = triple(1, 2, 3);
    let quad = statement.in_default_graph();
    let mut tx = engine.transaction();
    tx.insert_inferred(statement);
    tx.commit().expect("commit");

    let mut tx = engine.transaction();
    assert!(tx.insert_encoded(quad), "newly asserted");
    // Visible once, as asserted, already inside the transaction.
    assert_eq!(tx.quads_for_pattern(&QuadPattern::all()).count(), 1);
    assert_eq!(tx.len_in(ReadModel::Inferred), 0);
    assert!(
        !tx.insert_inferred(statement),
        "asserted statements are never inferred"
    );
    let summary = tx.commit().expect("commit");
    assert_eq!((summary.inserted, summary.inferred_deleted), (1, 1));
    let snapshot = engine.snapshot();
    assert_eq!(
        MODELS.map(|model| snapshot.len_in(model)),
        [1, 1, 0],
        "materialised, asserted, inferred"
    );
}

#[test]
fn asserting_in_a_named_graph_keeps_the_inferred_default_graph_statement() {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert_inferred(triple(1, 2, 3));
    let named = EncodedQuad::new(id(1), id(2), id(3), id(9));
    tx.insert_encoded(named);
    tx.commit().expect("commit");
    let snapshot = engine.snapshot();
    assert_eq!(MODELS.map(|model| snapshot.len_in(model)), [2, 1, 1]);
}

#[test]
fn retracting_leaves_inferred_statements_to_the_reasoner() {
    let engine = engine();
    let statement = triple(1, 2, 3);
    let quad = statement.in_default_graph();
    let mut tx = engine.transaction();
    tx.insert_inferred(statement);
    tx.commit().expect("commit");

    // Assert and retract within one transaction: the net change is nothing, so the
    // inferred statement survives.
    let mut tx = engine.transaction();
    assert!(tx.insert_encoded(quad));
    assert!(tx.remove_encoded(quad));
    assert!(tx.contains_in(ReadModel::Inferred, &quad));
    let summary = tx.commit().expect("commit");
    assert_eq!(summary.revision, 1, "no net change: {summary:?}");
    assert!(engine.snapshot().contains_in(ReadModel::Inferred, &quad));

    // Retract, infer, then assert again: it ends up asserted and not inferred.
    let mut tx = engine.transaction();
    tx.insert_encoded(triple(5, 5, 5).in_default_graph());
    tx.commit().expect("commit");
    let other = triple(5, 5, 5);
    let mut tx = engine.transaction();
    assert!(tx.remove_encoded(other.in_default_graph()));
    assert!(tx.insert_inferred(other));
    assert!(tx.insert_encoded(other.in_default_graph()));
    assert!(!tx.contains_in(ReadModel::Inferred, &other.in_default_graph()));
    assert_eq!(tx.len_in(ReadModel::Inferred), 1);
    tx.commit().expect("commit");
    let snapshot = engine.snapshot();
    assert!(snapshot.contains_in(ReadModel::Asserted, &other.in_default_graph()));
    assert!(!snapshot.contains_in(ReadModel::Inferred, &other.in_default_graph()));
    assert_eq!(snapshot.len_in(ReadModel::Inferred), 1);
}

#[test]
fn remove_matching_retracts_asserted_statements_only() {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert_encoded(triple(1, 2, 3).in_default_graph());
    tx.insert_inferred(triple(1, 2, 4));
    assert_eq!(tx.remove_matching(&QuadPattern::all()), 1);
    tx.commit().expect("commit");
    let snapshot = engine.snapshot();
    assert_eq!(MODELS.map(|model| snapshot.len_in(model)), [1, 0, 1]);
}

// --- Differential test against a two-set model ------------------------------------------

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
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
}

/// The reference semantics: two sets; inferred quads are visible while not asserted, and
/// a commit drops the ones that are.
#[derive(Clone, Default, PartialEq, Debug)]
struct Model {
    asserted: BTreeSet<EncodedQuad>,
    inferred: BTreeSet<EncodedQuad>,
}

impl Model {
    fn visible(&self, model: ReadModel) -> BTreeSet<EncodedQuad> {
        let inferred = self.inferred.difference(&self.asserted).copied();
        match model {
            ReadModel::Asserted => self.asserted.clone(),
            ReadModel::Inferred => inferred.collect(),
            ReadModel::Materialised => self.asserted.iter().copied().chain(inferred).collect(),
        }
    }

    fn committed(mut self) -> Self {
        self.inferred = self.visible(ReadModel::Inferred);
        self
    }
}

const TERMS: u64 = 3;

fn random_quad(rng: &mut Rng) -> EncodedQuad {
    let graph = match rng.below(3) {
        0 => id(20),
        _ => TermId::DEFAULT_GRAPH,
    };
    EncodedQuad::new(
        id(rng.below(TERMS)),
        id(rng.below(TERMS)),
        id(rng.below(TERMS)),
        graph,
    )
}

/// One random operation on both the transaction and the model; return values must agree.
fn random_operation(rng: &mut Rng, tx: &mut Transaction<'_>, model: &mut Model) {
    let quad = random_quad(rng);
    let statement = EncodedTriple::from(quad);
    let inferred_quad = statement.in_default_graph();
    let (got, expected) = match rng.below(4) {
        0 => (tx.insert_encoded(quad), model.asserted.insert(quad)),
        1 => (tx.remove_encoded(quad), model.asserted.remove(&quad)),
        2 => (
            tx.insert_inferred(statement),
            !model.asserted.contains(&inferred_quad)
                && !model.inferred.contains(&inferred_quad)
                && model.inferred.insert(inferred_quad),
        ),
        _ => (
            tx.remove_inferred(statement),
            model.visible(ReadModel::Inferred).contains(&inferred_quad)
                && model.inferred.remove(&inferred_quad),
        ),
    };
    assert_eq!(got, expected, "operation result for {quad:?}");
}

fn patterns() -> Vec<QuadPattern> {
    let choices = |n: u64| [None, Some(id(n))];
    let graphs = [
        GraphSelector::Any,
        GraphSelector::AnyNamed,
        GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        GraphSelector::Exact(id(20)),
    ];
    let mut patterns = Vec::new();
    for subject in choices(0) {
        for predicate in choices(1) {
            for object in choices(2) {
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

/// Checks every read model, pattern shape, count and point lookup of a view.
fn assert_view(
    model: &Model,
    len_in: impl Fn(ReadModel) -> u64,
    contains_in: impl Fn(ReadModel, &EncodedQuad) -> bool,
    scan: impl Fn(ReadModel, &QuadPattern) -> Vec<EncodedQuad>,
    context: &str,
) {
    for read_model in MODELS {
        let expected = model.visible(read_model);
        assert_eq!(
            len_in(read_model),
            expected.len() as u64,
            "{context}: len {read_model:?}"
        );
        for pattern in patterns() {
            let mut got = scan(read_model, &pattern);
            let returned = got.len();
            got.sort();
            got.dedup();
            assert_eq!(
                returned,
                got.len(),
                "{context}: duplicates {read_model:?} {pattern:?}"
            );
            let want: Vec<_> = expected
                .iter()
                .filter(|q| pattern.matches(q))
                .copied()
                .collect();
            assert_eq!(got, want, "{context}: {read_model:?} {pattern:?}");
        }
        let mut rng = Rng(fuzz(expected.len() as u64));
        for _ in 0..8 {
            let probe = random_quad(&mut rng);
            assert_eq!(
                contains_in(read_model, &probe),
                expected.contains(&probe),
                "{context}: contains {read_model:?} {probe:?}"
            );
        }
    }
}

fn assert_snapshot(snapshot: &Snapshot, model: &Model, context: &str) {
    assert_view(
        model,
        |m| snapshot.len_in(m),
        |m, q| snapshot.contains_in(m, q),
        |m, p| snapshot.quads_for_pattern_in(m, p).collect(),
        context,
    );
    // XC1: exact counts, and sorted scans merging both stacks, in every permutation the
    // pattern binds a prefix of and every included stack supports.
    for read_model in MODELS {
        let visible = model.visible(read_model);
        for pattern in patterns() {
            let expected: Vec<_> = visible
                .iter()
                .filter(|q| pattern.matches(q))
                .copied()
                .collect();
            assert_eq!(
                snapshot.count_in(read_model, &pattern),
                expected.len() as u64,
                "{context}: count {read_model:?} {pattern:?}"
            );
            for permutation in Permutation::ALL {
                let Some(scan) = snapshot.scan_sorted_in(read_model, &pattern, permutation) else {
                    continue;
                };
                let got: Vec<_> = scan.collect();
                let mut sorted = expected.clone();
                sorted.sort_by_key(|q| key(permutation, q));
                assert_eq!(
                    got, sorted,
                    "{context}: {read_model:?} {pattern:?} {permutation:?}"
                );
                // A range on the first unbound component keeps exactly the quads whose value
                // there lies in the range, in the same order; the count agrees.
                if pattern.graph == GraphSelector::AnyNamed {
                    continue;
                }
                let bound = |component: usize| match component {
                    0 => pattern.subject.is_some(),
                    1 => pattern.predicate.is_some(),
                    2 => pattern.object.is_some(),
                    _ => matches!(pattern.graph, GraphSelector::Exact(_)),
                };
                let position = permutation
                    .order()
                    .iter()
                    .take_while(|&&c| bound(c))
                    .count();
                if position == 4 {
                    continue;
                }
                let (low, high) = (id(3), id(10));
                let in_range: Vec<_> = sorted
                    .iter()
                    .filter(|q| (low..=high).contains(&key(permutation, q)[position]))
                    .copied()
                    .collect();
                let ranged: Vec<_> = snapshot
                    .scan_range_in(read_model, &pattern, permutation, low, high)
                    .expect("same plan as the sorted scan")
                    .collect();
                assert_eq!(
                    ranged, in_range,
                    "{context}: range {read_model:?} {pattern:?} {permutation:?}"
                );
                assert_eq!(
                    snapshot.count_range_in(read_model, &pattern, permutation, low, high),
                    Some(in_range.len() as u64),
                    "{context}: range count {read_model:?} {pattern:?} {permutation:?}"
                );
            }
        }
    }
}

/// A quad's components in `permutation`'s key order, as the public order describes it.
fn key(permutation: Permutation, quad: &EncodedQuad) -> [TermId; 4] {
    let canonical = [quad.subject, quad.predicate, quad.object, quad.graph];
    permutation.order().map(|component| canonical[component])
}

fn assert_transaction(tx: &Transaction<'_>, model: &Model, context: &str) {
    assert_view(
        model,
        |m| tx.len_in(m),
        |m, q| tx.contains_in(m, q),
        |m, p| tx.quads_for_pattern_in(m, p).collect(),
        context,
    );
}

/// Runs `steps` random transactions against `engine`, committing most and aborting some.
fn run_random_transactions(engine: &Engine, rng: &mut Rng, committed: &mut Model, steps: u32) {
    for step in 0..steps {
        let context = format!("step {step}");
        let mut tx = engine.transaction();
        let mut pending = committed.clone();
        for _ in 0..=rng.below(6) {
            random_operation(rng, &mut tx, &mut pending);
        }
        assert_transaction(&tx, &pending, &context);
        if rng.below(5) == 0 {
            drop(tx); // abort
        } else {
            tx.commit().expect("commit");
            *committed = pending.committed();
        }
        assert_snapshot(&engine.snapshot(), committed, &context);
    }
}

#[test]
fn both_stacks_match_the_model_with_compaction() {
    for seed in 0..25 {
        let engine = engine(); // inline compaction after every commit
        let mut rng = Rng(fuzz(seed));
        let mut committed = Model::default();
        run_random_transactions(&engine, &mut rng, &mut committed, 40);
    }
}

#[test]
fn both_stacks_survive_checkpoints_and_reopening() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let mut rng = Rng(fuzz(99));
    let mut committed = Model::default();
    for round in 0..6 {
        let engine = Engine::open(dir.path(), config).expect("open");
        assert_eq!(
            intern_terms(&engine),
            (0..TERM_COUNT).map(id).collect::<Vec<_>>()
        );
        assert_snapshot(&engine.snapshot(), &committed, &format!("reopen {round}"));
        run_random_transactions(&engine, &mut rng, &mut committed, 15);
        if round % 2 == 1 {
            engine.checkpoint().expect("checkpoint");
        }
    }
}

/// A snapshot restricted to a subset of its inferred statements reads that subset alone,
/// in every read path, whichever side of the split names it; the whole snapshot is
/// unchanged.
#[test]
fn inferred_subsets_restrict_every_read() {
    use nrese_engine::InferredSubset;
    let engine = engine();
    let mut tx = engine.transaction();
    assert!(tx.insert_encoded(triple(1, 2, 3).in_default_graph()));
    let inferred: Vec<EncodedQuad> = (4..10)
        .map(|o| triple(1, 2, o).in_default_graph())
        .collect();
    for quad in &inferred {
        assert!(tx.insert_inferred(EncodedTriple::new(
            quad.subject,
            quad.predicate,
            quad.object
        )));
    }
    tx.commit().expect("commit");
    let whole = engine.snapshot();
    let (kept, hidden) = inferred.split_at(2);
    for subset in [
        InferredSubset::Only(kept.to_vec()),
        InferredSubset::Without(hidden.to_vec()),
    ] {
        let restricted = whole.with_inferred_subset(subset);
        let expected: BTreeSet<EncodedQuad> = kept.iter().copied().collect();
        assert_eq!(scan(&restricted, ReadModel::Inferred), expected);
        assert_eq!(restricted.len_in(ReadModel::Inferred), 2);
        let pattern = QuadPattern {
            subject: Some(id(1)),
            ..QuadPattern::all()
        };
        assert_eq!(restricted.count_in(ReadModel::Materialised, &pattern), 3);
        let sorted: Vec<EncodedQuad> = restricted
            .scan_sorted_in(ReadModel::Materialised, &pattern, Permutation::Spog)
            .expect("sorted scan")
            .collect();
        assert_eq!(sorted.len(), 3);
        assert!(!restricted.contains_in(ReadModel::Materialised, &hidden[0]));
        assert!(restricted.contains_in(ReadModel::Materialised, &kept[0]));
        assert_eq!(restricted.revision(), whole.revision());
    }
    assert_eq!(whole.len_in(ReadModel::Inferred), 6);
}

/// A mask of hidden inferred statements carried across a commit by a run per change:
/// reads see exactly the statements neither hidden nor gone, counts included.
#[test]
fn inferred_masks_stack_across_commits() {
    use nrese_engine::InferredMask;
    let engine = engine();
    let quad = |o: u64| triple(1, 2, o).in_default_graph();
    let infer = |tx: &mut Transaction<'_>, o: u64| {
        let q = quad(o);
        assert!(tx.insert_inferred(EncodedTriple::new(q.subject, q.predicate, q.object)));
    };
    let mut tx = engine.transaction();
    for o in 4..10 {
        infer(&mut tx, o);
    }
    tx.commit().expect("commit");
    let first = engine.snapshot();
    // Hide 6, 7, 8.
    let mask = InferredMask::hiding(&first, &[quad(6), quad(7), quad(8)]);
    let view = first.with_inferred_mask(&mask).expect("same layout");
    let seen = |view: &Snapshot| -> Vec<u64> {
        scan(view, ReadModel::Inferred)
            .into_iter()
            .map(|q| (0..TERM_COUNT).find(|&n| id(n) == q.object).unwrap())
            .collect()
    };
    assert_eq!(seen(&view), [4, 5, 9]);
    assert_eq!(view.len_in(ReadModel::Inferred), 3);
    // A commit removes 7 (hidden) and 4 (shown) and adds 10 and 11.
    let mut tx = engine.transaction();
    for o in [7, 4] {
        let q = quad(o);
        assert!(tx.remove_inferred(EncodedTriple::new(q.subject, q.predicate, q.object)));
    }
    infer(&mut tx, 10);
    infer(&mut tx, 11);
    tx.commit().expect("commit");
    let second = engine.snapshot();
    // Now hidden: 6 still, 11 newly; 8 shown again; 7 gone.
    let mask = mask.changed(&second, &[quad(11)], &[quad(8), quad(7)]);
    assert_eq!(mask.runs(), 2);
    let view = second.with_inferred_mask(&mask).expect("same layout");
    assert_eq!(seen(&view), [5, 8, 9, 10]);
    assert_eq!(view.len_in(ReadModel::Inferred), 4);
    let pattern = QuadPattern {
        subject: Some(id(1)),
        ..QuadPattern::all()
    };
    assert_eq!(view.count_in(ReadModel::Inferred, &pattern), 4);
    assert!(!view.contains_in(ReadModel::Materialised, &quad(6)));
    assert!(!view.contains_in(ReadModel::Materialised, &quad(7)));
    assert!(view.contains_in(ReadModel::Materialised, &quad(8)));
    let sorted: Vec<EncodedQuad> = view
        .scan_sorted_in(ReadModel::Inferred, &pattern, Permutation::Spog)
        .expect("sorted scan")
        .collect();
    assert_eq!(sorted.len(), 4);
}
