//! Guards of the private engine-count bridge, not another engine kernel suite.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use nrese_engine::{EncodedQuad, EncodedTriple, Engine, EngineConfig};
use nrese_exec::{Budget, Scratch, measure};
use nrese_rdf::{GraphName, NamedNode, Quad, Term, TermRef};
use nrese_sparql_syntax::SparqlParser;

use super::*;
use crate::{QueryOptions, QueryResults, ReadView, evaluate_query};

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

fn engine() -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap()
}

fn success<T>(result: NativeResult<T>) -> T {
    result.unwrap_or_else(|error| panic!("{}", crate::QueryEvaluationError::from(error)))
}

fn evaluator<'a>(
    snapshot: &'a Snapshot,
    graph: PathGraph<'a>,
    budget: &'a Arc<Budget>,
) -> PathEvaluator<'a> {
    PathEvaluator {
        snapshot,
        model: ReadModel::Materialised,
        graph,
        fixed: None,
        cancellation: None,
        closures: Cell::new(0),
        budget,
    }
}

/// A deliberately non-Sync external implementation; snapshot() keeps its default None.
struct CountedView<V> {
    inner: V,
    evaluations: Cell<usize>,
}

impl<V: ReadView> ReadView for CountedView<V> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, V> {
        self.inner.quads_for_pattern_in(model, pattern)
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, V> {
        self.inner.named_graphs()
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        self.inner.contains_named_graph(graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.inner.lookup(term)
    }

    fn decode(&self, id: TermId) -> Option<Term> {
        self.inner.decode(id)
    }

    fn decode_within<B: std::ops::Deref<Target = Budget>>(
        &self,
        id: TermId,
        held: &mut nrese_exec::Reservation<B>,
    ) -> Result<Option<Term>, nrese_exec::BudgetExceeded> {
        self.inner.decode_within(id, held)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.inner.decode_quad(quad)
    }

    fn evaluation_snapshot(&self) -> Cow<'_, Snapshot> {
        self.evaluations.set(self.evaluations.get() + 1);
        self.inner.evaluation_snapshot()
    }
}

fn column(view: &impl ReadView, text: &str, options: &QueryOptions) -> Vec<Term> {
    let query = SparqlParser::new().parse_query(text).unwrap();
    let QueryResults::Solutions(rows) = evaluate_query(view, &query, options).unwrap() else {
        panic!("SELECT gives solutions")
    };
    rows.map(|row| row.unwrap().get(0).unwrap().clone())
        .collect()
}

#[test]
fn pending_non_sync_views_use_one_evaluation_snapshot_per_query() {
    let engine = engine();
    let old = Quad::new(node("old"), node("p"), node("old"), GraphName::DefaultGraph);
    let mut tx = engine.transaction();
    tx.insert(old.as_ref());
    let inferred = EncodedTriple {
        subject: tx.intern(node("old-inferred").as_ref().into()),
        predicate: tx.intern(node("p").as_ref().into()),
        object: tx.intern(node("old-inferred").as_ref().into()),
    };
    tx.insert_inferred(inferred);
    tx.commit().unwrap();
    let base = engine.snapshot();
    // Populate base scalars before changing either stack at the same revision.
    for model in [
        ReadModel::Asserted,
        ReadModel::Inferred,
        ReadModel::Materialised,
    ] {
        column(
            &base,
            "SELECT (COUNT(*) AS ?n) { ?a <urn:p>* ?b }",
            &QueryOptions {
                read_model: model,
                ..QueryOptions::default()
            },
        );
    }
    let mut tx = engine.transaction();
    tx.remove(old.as_ref());
    tx.remove_inferred(inferred);
    tx.insert(Quad::new(node("a"), node("p"), node("b"), GraphName::DefaultGraph).as_ref());
    tx.insert_inferred(EncodedTriple {
        subject: tx.intern(node("c").as_ref().into()),
        predicate: tx.intern(node("p").as_ref().into()),
        object: tx.intern(node("d").as_ref().into()),
    });
    assert_eq!(tx.pending(), (1, 1));
    assert_eq!(tx.inferred_pending(), (1, 1));
    assert!(ReadView::snapshot(&tx).is_none());
    let view = CountedView {
        inner: tx,
        evaluations: Cell::new(0),
    };
    assert!(view.snapshot().is_none());
    let cache = Arc::new(crate::ResultCache::new(1 << 20).admitting_all());
    for (model, names, count) in [
        (ReadModel::Asserted, vec!["a", "b"], 3u64),
        (ReadModel::Inferred, vec!["c", "d"], 3),
        (ReadModel::Materialised, vec!["a", "b", "c", "d"], 6),
    ] {
        let options = QueryOptions {
            read_model: model,
            result_cache: Some(Arc::clone(&cache)),
            ..QueryOptions::default()
        };
        let projection = "SELECT ?x { ?x <urn:p>* ?x }";
        let aggregate = "SELECT (COUNT(*) AS ?n) { ?a <urn:p>* ?b }";
        let want: BTreeSet<Term> = names.into_iter().map(|name| node(name).into()).collect();
        // Exercise Transaction itself as well as the external wrapper.
        assert_eq!(
            column(&view.inner, projection, &options)
                .into_iter()
                .collect::<BTreeSet<_>>(),
            want
        );
        let before = view.evaluations.get();
        assert_eq!(
            column(&view, projection, &options)
                .into_iter()
                .collect::<BTreeSet<_>>(),
            want
        );
        assert_eq!(view.evaluations.get(), before + 1);
        let result = column(&view, aggregate, &options);
        assert_eq!(view.evaluations.get(), before + 2);
        let Term::Literal(answer) = &result[0] else {
            panic!("COUNT is a literal")
        };
        assert_eq!(answer.value().parse::<u64>().unwrap(), count);
    }
    assert_eq!(view.evaluations.get(), 6);
}

fn store(n: usize) -> Snapshot {
    let engine = engine();
    let mut tx = engine.transaction();
    for i in 0..n {
        let quad = Quad::new(
            node(&format!("n{i}")),
            node("other"),
            node(&format!("n{}", i + 1)),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
        // Duplicate statements plus named-only nodes distinguish Any from Exact/filtered.
        tx.insert(
            Quad::new(
                quad.subject.clone(),
                quad.predicate.clone(),
                quad.object.clone(),
                node("g"),
            )
            .as_ref(),
        );
    }
    tx.insert(
        Quad::new(
            node("named-only"),
            node("other"),
            node("named-end"),
            node("g"),
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    engine.snapshot()
}

fn domain(evaluator: &PathEvaluator<'_>) -> Vec<u64> {
    evaluator
        .quads(None, None, None)
        .flat_map(|(s, _, o)| [s, o])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[test]
fn exact_all_filtered_and_empty_star_domains_remain_distinct() {
    let snapshot = store(7);
    let budget = Arc::new(Budget::unlimited());
    let graph = snapshot.lookup(node("g").as_ref().into()).unwrap();
    let only_default = crate::dataset::GraphSet::Only(vec![TermId::DEFAULT_GRAPH]);
    let empty = crate::dataset::GraphSet::Only(Vec::new());
    let star = Path::ZeroOrMore(Box::new(Path::Link(None)));
    for (graph, supported) in [
        (PathGraph::Default, true),
        (PathGraph::Named(graph), true),
        (PathGraph::Merged(None), true),
        (PathGraph::Merged(Some(&only_default)), false),
        (PathGraph::Merged(Some(&empty)), false),
    ] {
        let evaluator = evaluator(&snapshot, graph, &budget);
        let want = domain(&evaluator);
        let streamed = success(evaluator.star_domain_within());
        assert_eq!(streamed.is_some(), supported);
        if let Some(table) = streamed {
            assert_eq!(table.column(0), want);
        }
        assert_eq!(success(evaluator.node_count()), want.len() as u64);
        let table = success(evaluator.diagonal(&star)).unwrap();
        assert_eq!(table.width(), 1);
        assert_eq!(table.column(0), want);
        drop(table);
        assert_eq!(
            success(evaluator.count_open(&star)),
            Some(want.len() as u64)
        );
        assert_eq!(budget.used(), 0);
    }
    let missing = snapshot.lookup(node("other").as_ref().into()).unwrap();
    let evaluator = evaluator(&snapshot, PathGraph::Named(missing), &budget);
    let table = success(evaluator.star_domain_within()).expect("supported empty, not unsupported");
    assert_eq!((table.width(), table.len()), (1, 0));
    assert!(
        table.charge() > 0,
        "empty nonzero-width output owns its header"
    );
    drop(table);
    assert_eq!(success(evaluator.node_count()), 0);
    assert_eq!(budget.used(), 0);
}

fn memory<T>(result: NativeResult<T>) {
    assert!(
        matches!(
            result,
            Err(NativeError::Evaluation(
                crate::QueryEvaluationError::MemoryLimit(_)
            ))
        ),
        "supported refusal is terminal MemoryLimit"
    );
}

fn cancelled<T>(result: NativeResult<T>) {
    assert!(
        matches!(
            result,
            Err(NativeError::Evaluation(
                crate::QueryEvaluationError::Cancelled
            ))
        ),
        "supported cancellation is terminal Cancelled"
    );
}

#[test]
fn cold_bridge_errors_are_terminal_and_preserve_unrelated_owners() {
    let snapshot = store(1024);
    let named = snapshot.lookup(node("g").as_ref().into()).unwrap();
    for graph in [
        PathGraph::Default,
        PathGraph::Named(named),
        PathGraph::Merged(None),
    ] {
        let budget = Arc::new(Budget::new(37));
        let mut prior = Scratch::with_capacity(&*budget, 37).unwrap();
        prior.extend_from_slice(&[19u8; 37]).unwrap();
        let evaluator = evaluator(&snapshot, graph, &budget);
        // First call is cold: the legacy uncharged scalar succeeds here and must fail this guard.
        memory(evaluator.node_count());
        memory(evaluator.star_domain_within());
        memory(evaluator.diagonal(&Path::ZeroOrMore(Box::new(Path::Link(None)))));
        assert_eq!(budget.used(), 37);
        assert_eq!(&*prior, &[19u8; 37]);
        drop(prior);
        assert_eq!(budget.used(), 0);
        let unlimited = Arc::new(Budget::unlimited());
        let token = crate::CancellationToken::new();
        token.cancel();
        let evaluator = PathEvaluator {
            budget: &unlimited,
            cancellation: Some(&token),
            ..evaluator
        };
        cancelled(evaluator.node_count());
        cancelled(evaluator.star_domain_within());
        cancelled(evaluator.diagonal(&Path::ZeroOrMore(Box::new(Path::Link(None)))));
        assert_eq!(unlimited.used(), 0);
    }
}

#[test]
fn star_streams_and_retained_output_are_strictly_admitted() {
    if !measure::alone() {
        return;
    }
    // First preparation is outside all pools/locks. This pool lives for the process.
    measure::measure(|| ());
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.broadcast(|_| ());
        pool
    });
    pool.install(|| {
        // The immutable store is outside request admission; all stream/read/table tokens
        // and the independent live payload are made inside each measured operation.
        let snapshot = store(1024);
        for graph in [PathGraph::Default, PathGraph::Merged(None)] {
            let budget = Arc::new(Budget::unlimited());
            let want = domain(&evaluator(&snapshot, graph, &budget));
            let ((), excess) = measure::measure_within(&budget, || {
                let mut prior = Scratch::with_capacity(&*budget, 37).unwrap();
                prior.extend_from_slice(&[23u8; 37]).unwrap();
                let bounded = snapshot.reading_within(Arc::clone(&budget)).unwrap();
                let table =
                    success(evaluator(&bounded, graph, &budget).star_domain_within()).unwrap();
                assert_eq!(table.column(0), want);
                drop(bounded); // Both streams, the evaluator, and read handle are gone.
                assert_eq!(budget.used(), table.charge() + 37);
                assert_eq!(
                    table.column(0),
                    want,
                    "output outlives streams and read handle"
                );
                drop(table);
                assert_eq!(budget.used(), 37);
                assert_eq!(&*prior, &[23u8; 37]);
                drop(prior);
            });
            assert_eq!(excess, 0, "stream/final-table admission excess={excess}");
            assert_eq!(budget.used(), 0);
            eprintln!("star-domain charge-peak={} excess={excess}", budget.peak());
            // Caller must reach the warm scalar before preparing any caller scratch.
            assert_eq!(
                success(evaluator(&snapshot, graph, &budget).node_count()),
                want.len() as u64
            );
            let zero = Arc::new(Budget::new(0));
            let (count, allocated) =
                measure::measure(|| success(evaluator(&snapshot, graph, &zero).node_count()));
            assert_eq!(count, want.len() as u64);
            assert_eq!((allocated, zero.peak()), (0, 0));
        }
        // Refusal while preparing parts, and cancellation, release only their own work.
        for limit in [37, 165, 4133] {
            let budget = Arc::new(Budget::new(limit));
            let ((), excess) = measure::measure_within(&budget, || {
                let mut prior = Scratch::with_capacity(&*budget, 37).unwrap();
                prior.extend_from_slice(&[29u8; 37]).unwrap();
                memory(evaluator(&snapshot, PathGraph::Merged(None), &budget).star_domain_within());
                assert_eq!(budget.used(), 37);
                assert_eq!(&*prior, &[29u8; 37]);
                drop(prior);
            });
            assert_eq!(excess, 0, "refused stream/drop excess={excess}");
            assert_eq!(budget.used(), 0);
        }
        let budget = Arc::new(Budget::unlimited());
        let token = crate::CancellationToken::new(); // Existing query-size bookkeeping.
        token.cancel();
        let ((), excess) = measure::measure_within(&budget, || {
            let mut prior = Scratch::with_capacity(&*budget, 37).unwrap();
            prior.extend_from_slice(&[31u8; 37]).unwrap();
            let evaluator = PathEvaluator {
                cancellation: Some(&token),
                ..evaluator(&snapshot, PathGraph::Default, &budget)
            };
            cancelled(evaluator.star_domain_within());
            assert_eq!(budget.used(), 37);
            assert_eq!(&*prior, &[31u8; 37]);
            drop(prior);
        });
        assert_eq!(excess, 0, "cancelled stream/drop excess={excess}");
        assert_eq!(budget.used(), 0);
    });
}
