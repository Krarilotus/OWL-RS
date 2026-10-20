//! What reads of a merged default graph hold against what they charge, under this test
//! binary's counting allocator ([`nrese_exec::measure`]).

use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_exec::measure::{alone, measure, measure_within};
use nrese_exec::{Envelope, SharedBudget};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql_syntax::SparqlParser;

fn node(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{local}"))
}

fn store(quads: impl IntoIterator<Item = (String, &'static str, String, &'static str)>) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (s, p, o, g) in quads {
        tx.insert(Quad::new(node(&s), node(p), node(&o), node(g)).as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// A query's options with `graphs` as its default graph (their merge), under `memory`.
fn merging(graphs: &[&str], memory: Envelope) -> QueryOptions {
    let mut dataset = crate::results::QueryDatasetSpecification::new();
    dataset.set_default_graph(
        graphs
            .iter()
            .map(|g| GraphName::NamedNode(node(g)))
            .collect(),
    );
    let mut options = QueryOptions {
        dataset: Some(dataset),
        ..QueryOptions::default()
    };
    options.resources.memory = memory;
    options
}

fn triple(s: &str, p: &str, o: &str) -> TriplePattern {
    TriplePattern {
        subject: TermPattern::Variable(Variable::new_unchecked(s)),
        predicate: NamedNodePattern::NamedNode(node(p)),
        object: TermPattern::Variable(Variable::new_unchecked(o)),
    }
}

/// What an operator holds uncharged whatever its rows: its plan's bookkeeping (the
/// pattern's variables, the positions of each variable, the measured input's variable
/// list), made per call, not per row. The tables' column vectors and the row buffers are
/// charged (the tables own their charge). Uncharged by design (it grows with the query's
/// size, which the planner's own budget is to cover, not its data); bounded here so that it
/// stays small (20 Oct: 276 bytes for the streamed LIMIT, 431 for the index join).
const PLAN_BOOKKEEPING_BYTES: usize = 512;

/// What a column of an operator's input adds to its bookkeeping, per call: its variable,
/// copied into the input's, the plan's and the output's variable lists (20 Oct: 99 bytes a
/// column, the same at two chunks and at sixteen).
const COLUMN_BOOKKEEPING_BYTES: usize = 104;

/// Sol's case (review of 2b6ddbb7): 100 statements in two listed graphs under one-byte
/// request and server budgets, `LIMIT 100` refused with nothing left charged. And
/// measured, beyond the engine's own scan or probes, with the inputs made inside the
/// measurement: a streamed LIMIT over the merge holds exactly as much uncharged for 10,000
/// rows as for one, and an index join into it as much for a key's 5,000 matches, each in
/// three graphs (sorted, deduplicated and copied), as for one; that much is no more than
/// the plan's bookkeeping. Nothing that grows with the rows is held uncharged.
#[test]
fn merged_graph_reads_hold_no_more_than_they_charge() {
    if !alone() {
        return;
    }
    // Parallel parts one at a time (this test's process is its own): each holds the
    // engine's probe cursor, measured apart (`engine_probes`) and not the join's to charge,
    // and how many run at once depends on the threads, not the rows.
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global();
    let engine = store(
        (0..100).flat_map(|i| ["g0", "g1"].map(|g| (format!("s{i}"), "p", "target".to_owned(), g))),
    );
    let query = SparqlParser::new()
        .parse_query("SELECT * FROM <urn:g0> FROM <urn:g1> WHERE { ?s <urn:p> ?o } LIMIT 100")
        .unwrap();
    let shared = SharedBudget::new(1);
    let mut options = QueryOptions::default();
    options.resources.memory = Envelope::new(Some(1), Some(shared.clone()));
    let snapshot = engine.snapshot();
    let refused = crate::evaluate_query(&snapshot, &query, &options);
    assert!(
        matches!(refused, Err(QueryEvaluationError::MemoryLimit(_))),
        "accepted: {:?}",
        refused.map(|_| ())
    );
    assert_eq!((shared.used(), shared.peak()), (0, 0));

    // What a streamed LIMIT holds uncharged beyond the engine's scan, for `rows` rows
    // projected on `projection` (none: every variable).
    let streamed = |rows: usize, projection: &[&str]| -> usize {
        let engine = store(
            (0..rows)
                .flat_map(|i| ["g0", "g1"].map(|g| (format!("s{i}"), "p", format!("o{i}"), g))),
        );
        let snapshot = engine.snapshot();
        let envelope = Envelope::unlimited();
        let ctx = Context::new(
            &snapshot,
            &merging(&["g0", "g1"], envelope.clone()),
            None,
            None,
        );
        let mut pattern = GraphPattern::Bgp {
            patterns: vec![triple("s", "p", "o")],
        };
        if !projection.is_empty() {
            pattern = GraphPattern::Project {
                inner: Box::new(pattern),
                variables: projection
                    .iter()
                    .map(|v| Variable::new_unchecked(*v))
                    .collect(),
            };
        }
        let scan = ctx.scan_pattern(&triple("s", "p", "o")).unwrap();
        let engine_scan = measure(|| {
            snapshot
                .scan_sorted_in(ctx.model, &scan.quad_pattern(), scan.permutation_for(None))
                .unwrap()
                .count()
        })
        .1;
        let (solutions, excess) = measure_within(&envelope, || {
            ctx.limited_scan(&pattern, rows)
                .ok()
                .flatten()
                .expect("the rows")
        });
        assert_eq!(solutions.table.len(), rows);
        excess.saturating_sub(engine_scan)
    };
    // What an index join holds uncharged beyond the engine's probes, for `rows` input rows
    // `width` columns wide (one key, one match in each of two graphs), in the order the
    // index reads or not: sorted first, then probed in parallel chunks past two chunks' rows.
    // The least of three measurements: the thread pool's own bookkeeping now and then
    // allocates a little while one runs (144 bytes, at either size); what grows with the
    // rows shows in every one.
    let probed_rows = |rows: usize, ordered: bool, width: usize| -> usize {
        let engine = store(["g0", "g1"].map(|g| ("hub".to_owned(), "q", "o".to_owned(), g)));
        let snapshot = engine.snapshot();
        let envelope = Envelope::unlimited();
        let options = merging(&["g0", "g1"], envelope.clone());
        let ctx = Context::new(&snapshot, &options, None, None);
        let hub = snapshot.lookup(node("hub").as_ref().into()).unwrap();
        let scan = ctx.scan_pattern(&triple("b", "q", "c")).unwrap();
        let mut bound = scan.clone();
        bound.slots[0] = Slot::Const(hub);
        let engine_probes = measure(|| {
            let mut cursor = snapshot.probe_cursor(ctx.model);
            let mut quads = 0;
            cursor.for_each(&bound.quad_pattern(), |_| quads += 1);
            quads
        })
        .1;
        let shared = [Variable::new_unchecked("b")];
        let vars: Vec<Variable> = (0..width)
            .map(|i| match i {
                0 => shared[0].clone(),
                _ => Variable::new_unchecked(format!("v{i}")),
            })
            .collect();
        let row = vec![hub.raw(); width];
        (0..3)
            .map(|_| {
                let (joined, excess) = measure_within(&envelope, || {
                    // The input made and charged inside the measurement, charged before it
                    // is made.
                    let mut table = IdTable::charged(width, rows, &ctx.budget).unwrap();
                    for _ in 0..rows {
                        table.push_reserved(&row);
                    }
                    let left = Solutions {
                        vars: vars.clone(),
                        table,
                        ordered,
                    };
                    ctx.probe_join(left, &scan, &shared).ok().expect("the join")
                });
                assert_eq!(joined.table.len(), rows);
                ctx.consumed(joined);
                excess.saturating_sub(engine_probes)
            })
            .min()
            .expect("three")
    };
    // What an index join holds uncharged beyond the engine's probes, for a key's `fanout`
    // matches in each of three graphs.
    let probed = |fanout: usize| -> usize {
        let engine =
            store((0..fanout).flat_map(|i| {
                ["g0", "g1", "g2"].map(|g| ("hub".to_owned(), "q", format!("o{i}"), g))
            }));
        let snapshot = engine.snapshot();
        let envelope = Envelope::unlimited();
        let options = merging(&["g0", "g1", "g2"], envelope.clone());
        let ctx = Context::new(&snapshot, &options, None, None);
        let hub = snapshot.lookup(node("hub").as_ref().into()).unwrap();
        // `?b ?q ?c`: two values a match, sorted and copied through the probe's scratch.
        let pattern = TriplePattern {
            predicate: NamedNodePattern::Variable(Variable::new_unchecked("q")),
            ..triple("b", "q", "c")
        };
        let scan = ctx.scan_pattern(&pattern).unwrap();
        let mut bound = scan.clone();
        bound.slots[0] = Slot::Const(hub);
        let engine_probes = measure(|| {
            let mut cursor = snapshot.probe_cursor(ctx.model);
            let mut quads = 0;
            cursor.for_each(&bound.quad_pattern(), |_| quads += 1);
            quads
        })
        .1;
        let shared = [Variable::new_unchecked("b")];
        let (joined, excess) = measure_within(&envelope, || {
            // The input made and charged inside the measurement.
            let left = ctx
                .produced(Solutions {
                    vars: shared.to_vec(),
                    table: IdTable::from_rows(1, std::iter::once(&[hub.raw()][..]), &ctx.budget)
                        .unwrap(),
                    ordered: false,
                })
                .ok()
                .expect("charged");
            ctx.probe_join(left, &scan, &shared).ok().expect("the join")
        });
        assert_eq!(joined.table.len(), fanout);
        excess.saturating_sub(engine_probes)
    };
    // Each row scales one dimension that grows with the data: the rows a LIMIT streams
    // (plain, and projected on unbound variables: columns made per row), a key's matches,
    // and an index join's input rows, kept in order or sorted first, and 32 columns wide
    // (two chunks against sixteen, both on the parallel path). Each is bounded by its
    // plan's bookkeeping.
    let unbound = ["s", "o", "missing0", "missing1"];
    for (operator, one, many, columns) in [
        ("streamed LIMIT", streamed(1, &[]), streamed(10_000, &[]), 0),
        (
            "streamed LIMIT, unbound projected",
            streamed(2_000, &unbound),
            streamed(10_000, &unbound),
            0,
        ),
        ("index join", probed(1), probed(5_000), 0),
        (
            "index join, input in order",
            probed_rows(8_192, true, 1),
            probed_rows(65_536, true, 1),
            0,
        ),
        (
            "index join, input sorted",
            probed_rows(8_192, false, 1),
            probed_rows(65_536, false, 1),
            0,
        ),
        (
            "index join, wide input",
            probed_rows(8_192, true, 32),
            probed_rows(65_536, true, 32),
            32,
        ),
    ] {
        assert_eq!(many, one, "{operator}: uncharged bytes grow with the rows");
        let bound = PLAN_BOOKKEEPING_BYTES + columns * COLUMN_BOOKKEEPING_BYTES;
        assert!(
            one <= bound,
            "{operator}: {one} uncharged bytes, more than the plan's bookkeeping"
        );
    }
}

/// A FILTER's mask of its rows is charged before it is made, on one thread and in parallel
/// parts: what it holds uncharged doesn't grow with the rows.
#[test]
fn filter_masks_hold_no_more_than_they_charge() {
    if !alone() {
        return;
    }
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let v = Variable::new_unchecked("v");
    // FILTER(?v > 5) over inline integers: decided on ids.
    let expression = Expression::Greater(
        Box::new(Expression::Variable(v.clone())),
        Box::new(Expression::Literal(nrese_rdf::Literal::from(5))),
    );
    let held = |rows: usize| {
        let envelope = Envelope::unlimited();
        let mut options = QueryOptions::default();
        options.resources.memory = envelope.clone();
        let ctx = Context::new(&snapshot, &options, None, None);
        let (kept, excess) = measure_within(&envelope, || {
            // The input made charged inside the measurement.
            let ids = (0..rows as i64).map(|i| TermId::inline_integer(i).unwrap().raw());
            let table = IdTable::gathered(1, rows, &ctx.budget, [ids]).unwrap();
            let solutions = Solutions {
                vars: vec![v.clone()],
                table,
                ordered: false,
            };
            let kept = ctx.filter(solutions, &expression).ok().expect("filtered");
            kept.table.len()
        });
        assert_eq!(kept, rows.saturating_sub(6));
        excess
    };
    assert_eq!(held(100), held(10_000), "on one thread");
    assert_eq!(held(20_000), held(200_000), "in parallel parts");
}
