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

/// A store of `quads`, without background maintenance: no compaction or checkpoint runs on a
/// thread of its own while a measurement does (every thread counts).
fn store(quads: impl IntoIterator<Item = (String, &'static str, String, &'static str)>) -> Engine {
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::new(config).unwrap();
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
/// (19 Oct: 324 bytes for the streamed LIMIT, 553 for the index join). A bound for these
/// fixed plans: the bookkeeping grows with the query's size (its patterns), not its data,
/// which the planner's own budget is to cover.
const PLAN_BOOKKEEPING_BYTES: usize = 640;

/// What a column of an operator's input adds to its bookkeeping, per call: its variable, its
/// header in the input's, the output's and each running chunk's column lists, a row slot
/// (19 Oct: about 122 bytes a column, the same at two chunks and at sixteen).
const COLUMN_BOOKKEEPING_BYTES: usize = 128;

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
                    let mut held = Reservation::new(&*ctx.budget);
                    held.charge(rows * width * 8).unwrap();
                    let mut table = IdTable::with_capacity(width, rows);
                    for _ in 0..rows {
                        table.push_row(&row);
                    }
                    held.transfer();
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

/// Guard (PR #84's timing, notes/2026-11-02-zero-width-budget.md ZW-10): one solution without
/// bindings counts nothing, so making and consuming it, as an empty group does once per row
/// of a substituted EXISTS, never draws on the query's budget: its peak stays where it was.
/// Two such rows count one id, charged where they are made and released where they are
/// consumed.
#[test]
fn a_solution_without_bindings_draws_nothing_from_the_budget() {
    let engine = store([]);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    // What the evaluation holds from its start (its reads' state); nothing charged and
    // released before, so a charge now would raise the peak.
    let held = ctx.budget.used();
    assert_eq!(ctx.budget.peak(), held, "a fresh evaluation");
    for _ in 0..3 {
        let unit = ctx.unit().ok().expect("one solution");
        ctx.consumed(unit);
    }
    assert_eq!((ctx.budget.used(), ctx.budget.peak()), (held, held));
    let two = ctx
        .produced(Solutions {
            vars: Vec::new(),
            table: IdTable::from_rows(0, [&[][..], &[][..]]),
            ordered: false,
        })
        .ok()
        .expect("room");
    assert_eq!(ctx.budget.used(), held + 8);
    ctx.consumed(two);
    assert_eq!(ctx.budget.used(), held);
}

/// Spec (notes/2026-11-02-exec-memory-stack.md EMS-2): what a table is charged is what its
/// consumer releases. One solution without bindings counts nothing, so an operator that gives
/// such a row a column, in place or in a table it makes of it, charges that column, or
/// consuming its answer releases another holder's charge: a projection's UNDEF column, a
/// union's widening, GRAPH ?g's column, a BIND's column, LATERAL's put-in column, and
/// SERVICE ?e's endpoint column (that one on any width: sol-loop r1 S1). Each pattern is
/// evaluated with another holder's charge on the query's budget; after its answer is
/// consumed the budget holds at least that much again. Every term the cases name is
/// stored, so no computed term's kept charge covers a release (sol-loop r2 S2).
#[test]
fn a_column_given_to_a_row_without_columns_is_charged() {
    use crate::{CancellationToken, ServiceClient, ServiceResults, Services};
    use nrese_sparql_syntax::Query;

    /// An endpoint whose every answer is five rows of one variable, a stored term.
    struct Five;
    impl ServiceClient for Five {
        fn select(
            &self,
            _endpoint: &str,
            _query: &str,
            _cancellation: Option<&CancellationToken>,
        ) -> Result<ServiceResults, Box<dyn std::error::Error + Send + Sync>> {
            let x = Term::from(node("a"));
            Ok(ServiceResults {
                variables: vec![Variable::new_unchecked("x")],
                rows: vec![vec![Some(x)]; 5],
            })
        }
    }

    let engine =
        store((0..3).map(|g| ("a".to_owned(), "r", "b".to_owned(), ["g0", "g1", "g2"][g])));
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        services: Some(Services(Arc::new(Five))),
        ..QueryOptions::default()
    };
    let cases = [
        ("a projection's UNDEF column", "SELECT ?u WHERE { }"),
        (
            "a union's widening",
            "SELECT * WHERE { { } UNION { VALUES ?u { <urn:a> } } }",
        ),
        ("GRAPH ?g's column", "SELECT * WHERE { GRAPH ?g { } }"),
        ("a BIND's column", "SELECT * WHERE { BIND(<urn:a> AS ?u) }"),
        (
            "LATERAL's put-in column",
            "SELECT * WHERE { VALUES ?s { <urn:a> <urn:b> } LATERAL { FILTER(?s = ?s) } }",
        ),
        (
            "SERVICE ?e's endpoint column",
            "SELECT * WHERE { VALUES ?e { <urn:b> } SERVICE ?e { ?a ?b ?x } }",
        ),
    ];
    // Every case's verdict, not only the first failing one's.
    let mut failures = Vec::new();
    for (case, text) in cases {
        let Query::Select { pattern, .. } = SparqlParser::new().parse_query(text).unwrap() else {
            panic!("{case}: a SELECT")
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let mut other = Reservation::new(&*ctx.budget);
        other.charge(1 << 20).unwrap();
        let before = ctx.budget.used();
        match ctx.eval(&pattern) {
            Ok(answer) if answer.table.is_empty() => {
                failures.push(format!("{case}: no rows, nothing tested"));
            }
            Ok(answer) => ctx.consumed(answer),
            Err(_) => failures.push(format!("{case}: refused")),
        }
        let after = ctx.budget.used();
        if after < before {
            failures.push(format!(
                "{case}: consuming the answer released {} bytes of another holder's charge",
                before - after
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

/// Guard (the executor memory stack's timing, notes/2026-11-02-exec-memory-stack.md EMS-11:
/// a budget call per value, term or triple cost GROUP_CONCAT x1.63): charges through
/// `charge_ahead` call the budget once for as many as its room ahead holds, each still
/// charged before it is made, and the room left is released by the caller. Near the limit
/// only what is left is taken ahead: a charge that fits is never refused for it.
#[test]
fn charges_ahead_call_the_budget_once_for_many() {
    let budget = nrese_exec::Budget::new(1 << 20);
    let mut held = Reservation::new(&budget);
    let mut spare = 0;
    // A thousand values of 10 bytes after the first: one budget call for all of them.
    charge_ahead(&budget, &mut held, &mut spare, 10).unwrap();
    let first = budget.used();
    for _ in 0..1_000 {
        charge_ahead(&budget, &mut held, &mut spare, 10).unwrap();
    }
    assert_eq!(
        budget.used(),
        first,
        "no budget call while the room ahead lasts"
    );
    held.release(spare);
    assert_eq!(budget.used(), 10 * 1_001);

    let small = nrese_exec::Budget::new(100);
    let mut held = Reservation::new(&small);
    let mut spare = 0;
    charge_ahead(&small, &mut held, &mut spare, 60).unwrap();
    assert_eq!((small.used(), spare), (100, 40));
    charge_ahead(&small, &mut held, &mut spare, 40).unwrap();
    assert!(charge_ahead(&small, &mut held, &mut spare, 1).is_err());
    assert_eq!(small.used(), 100);
}
