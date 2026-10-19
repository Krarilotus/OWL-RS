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
/// pattern's variables and scan pattern, the positions of each variable, a row buffer, the
/// tables' column lists, the measured input's variable list), made per call, not per row.
/// Uncharged by design (the executor charges rows); bounded here so that it stays small
/// (19 Oct: 324 bytes for the streamed LIMIT, 553 for the index join).
const PLAN_BOOKKEEPING_BYTES: usize = 640;

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

    // What a streamed LIMIT holds uncharged beyond the engine's scan, for `rows` rows.
    let streamed = |rows: usize| -> usize {
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
        let pattern = GraphPattern::Bgp {
            patterns: vec![triple("s", "p", "o")],
        };
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
                    table: IdTable::from_rows(1, std::iter::once(&[hub.raw()][..])),
                    ordered: false,
                })
                .ok()
                .expect("charged");
            ctx.probe_join(left, &scan, &shared).ok().expect("the join")
        });
        assert_eq!(joined.table.len(), fanout);
        excess.saturating_sub(engine_probes)
    };
    for (operator, one, many) in [
        ("streamed LIMIT", streamed(1), streamed(10_000)),
        ("index join", probed(1), probed(5_000)),
    ] {
        assert_eq!(many, one, "{operator}: uncharged bytes grow with the rows");
        assert!(
            one <= PLAN_BOOKKEEPING_BYTES,
            "{operator}: {one} uncharged bytes, more than the plan's bookkeeping"
        );
    }
}
