//! What reads of a merged default graph hold against what they charge, under this test
//! binary's counting allocator ([`nrese_exec::measure`]).

use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_exec::measure::{alone, measure, measure_within};
use nrese_exec::{Envelope, SharedBudget};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql_syntax::SparqlParser;
use nrese_sparql_syntax::algebra::PropertyPathExpression;

/// Spec: retaining an alias reserves at least its copied literal payload, once (F1).
#[test]
fn e1_followup_alias_retention_is_charged_once() {
    let engine = store([]);
    let payload = "x".repeat(65_536);
    let term: Term = nrese_rdf::Triple::new(
        node("s"),
        node("p"),
        nrese_rdf::Literal::from(payload.as_str()),
    )
    .into();
    let mut tx = engine.transaction();
    tx.insert(Quad::new(node("r"), node("p"), term.clone(), GraphName::DefaultGraph).as_ref());
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let id = ctx.id(&term).ok().unwrap();
    let alias = internal::class_constant(id);
    let before = ctx.budget.used();
    ctx.register_alias(alias.as_str(), id);
    let after = ctx.budget.used();
    assert!(
        after - before >= payload.len(),
        "retained triple copy must draw from the budget"
    );
    ctx.register_alias(alias.as_str(), id);
    assert_eq!(ctx.budget.used(), after, "same alias retains no new value");
    let options = QueryOptions {
        resources: crate::Resources {
            memory: Envelope::new(Some(payload.len() - 1), None),
            ..crate::Resources::default()
        },
        ..QueryOptions::default()
    };
    let limited = Context::new(&snapshot, &options, None, None);
    assert!(
        limited.check().is_ok(),
        "reader state fits before the alias"
    );
    limited.register_alias(alias.as_str(), id);
    assert!(limited.check().is_err(), "the copied payload cannot fit");
    assert!(
        limited.aliases.borrow().is_empty(),
        "nothing kept on refusal"
    );
    let query = SparqlParser::new()
        .parse_query("SELECT * WHERE {}")
        .unwrap();
    assert!(
        native_pattern(&query, &options, &limited).is_err(),
        "preparation must propagate the alias refusal too"
    );
}

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
    use crate::{ServiceCall, ServiceClient, ServiceResults, Services};
    use nrese_sparql_syntax::Query;

    /// An endpoint whose every answer is five rows of one variable, a stored term.
    struct Five;
    impl ServiceClient for Five {
        fn select(
            &self,
            _endpoint: &str,
            _query: &str,
            _call: ServiceCall<'_>,
        ) -> Result<ServiceResults, QueryEvaluationError> {
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

/// Spec (sol-loop r3 S2; the budget types' owner, 3 Nov): under a 4 KiB envelope no grant
/// holds spare, so CONSTRUCT's two grants (the copies it keeps to emit a triple once, and
/// the triples) charge exactly and answer its one triple, never refused for spare.
#[test]
fn construct_under_a_small_envelope_is_never_refused_for_spare() {
    let engine = store([]);
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query("CONSTRUCT { <urn:s> <urn:p> <urn:o> } WHERE { }")
        .unwrap();
    let mut options = QueryOptions::default();
    options.resources.memory = Envelope::new(Some(4 << 10), None);
    let answer = crate::evaluate_query(&snapshot, &query, &options);
    assert!(answer.is_ok(), "refused: {:?}", answer.err());
    assert!(!options.resources.memory.refused());
}

/// A store of default-graph statements `urn:s urn:p urn:o`, without background maintenance.
fn default_store(statements: impl IntoIterator<Item = (String, String, String)>) -> Engine {
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::new(config).unwrap();
    let mut tx = engine.transaction();
    for (s, p, o) in statements {
        let quad = Quad::new(node(&s), node(&p), node(&o), GraphName::DefaultGraph);
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// A path's end: `?name` a variable, else the IRI `urn:name`.
fn end(text: &str) -> TermPattern {
    match text.strip_prefix('?') {
        Some(name) => TermPattern::Variable(Variable::new_unchecked(name)),
        None => TermPattern::NamedNode(node(text)),
    }
}

fn link(p: &str) -> PropertyPathExpression {
    PropertyPathExpression::NamedNode(node(p))
}

/// A path pattern: its subject, path and object ([`end`]).
type PathTerms<'a> = (&'a str, &'a PropertyPathExpression, &'a str);

/// A path pattern that owns its path, for a case to return.
type PathCase = (&'static str, PropertyPathExpression, &'static str);

/// `subject path object` over `engine`'s default graph under a request limit of `limit`
/// bytes (`None`: unlimited): its rows or the refusal, and the most it held beyond its charge
/// at any moment ([`measure_within`]: a charge released while its memory lives counts there).
fn path_held(
    engine: &Engine,
    (subject, path, object): PathTerms<'_>,
    limit: Option<usize>,
) -> (Result<usize, String>, usize) {
    let snapshot = engine.snapshot();
    let mut options = QueryOptions::default();
    options.resources.memory = Envelope::new(limit, None);
    let ctx = Context::new(&snapshot, &options, None, None);
    let (subject, object) = (end(subject), end(object));
    measure_within(&options.resources.memory, || {
        ctx.path(&subject, path, &object)
            .map(|solutions| solutions.table.len())
            .map_err(|error| format!("{:?}", QueryEvaluationError::from(error)))
    })
}

/// Guard (the executor memory stack's package fix check r3, EMS-R3-C1, and its related
/// sites): a path's buffers hold no more than they are charged at any moment, so a query
/// sharing the server's budget can never take the room of memory that still lives. Each
/// case runs at `rows` and at `more` rows, and the most it held beyond its charge may not
/// grow by a byte a row it adds: what grows with the rows is charged throughout (what doesn't
/// is the plan's bookkeeping, the same at both sizes). `case` makes the store for a size and
/// names the path pattern, `limit` the request's limit at that size.
fn holds_nothing_uncharged_per_row(
    name: &str,
    (rows, more): (usize, usize),
    case: impl Fn(usize) -> (Engine, PathCase),
    limit: impl Fn(usize) -> Option<usize>,
    answer: impl Fn(Result<usize, String>, usize) -> Result<(), String>,
) -> Result<(), String> {
    let held = |n: usize| {
        let (engine, (subject, path, object)) = case(n);
        let (rows, excess) = path_held(&engine, (subject, &path, object), limit(n));
        answer(rows, n).map_err(|wrong| format!("{name} at {n} rows: {wrong}"))?;
        Ok::<_, String>(excess)
    };
    let (few, many) = (held(rows)?, held(more)?);
    eprintln!("{name}: {few} bytes beyond its charge at {rows} rows, {many} at {more}");
    match many.saturating_sub(few) < more - rows {
        true => Ok(()),
        false => Err(format!(
            "{name}: held {few} bytes beyond its charge at {rows} rows, {many} at {more}"
        )),
    }
}

/// The answer must be `n` rows.
fn rows_of(rows: Result<usize, String>, n: usize) -> Result<(), String> {
    match rows {
        Ok(rows) if rows == n => Ok(()),
        other => Err(format!("answered {other:?}, not {n} rows")),
    }
}

/// Spec (EMS-R3-C1 and the sites the package fix check r3 named beside it): every row-sized
/// buffer of a path is charged while it lives, through the move into its table and through a
/// refusal. Cases, each at two sizes:
/// - `<h> (p|missing) ?y` over 2^k + 1 statements (T1): the union's vector has room for 2^(k+1)
///   ends; moved into its column, its whole capacity stays charged (it was released down to
///   its length, then charged again, at 6449d5cd).
/// - `?x (p|missing) ?y` (T2): the pairs' columns are made beside the pairs, whose charge
///   goes only once they are freed.
/// - T2 under a limit of its answer's two columns alone, n ids each (T3), which the pairs
///   and their columns can't fit together: refused, wherever the refusal comes (the union's
///   growth or a column), and every buffer freed before its charge is released.
/// - `<h> p/q ?y` (T4, a sequence from a node), `?x p/q ?y` (T5, every pair), `?x p/(q/r) ?y`
///   (T6, a step that isn't a link), `<h> p/q <b>` (T7, both ends bound: the ways counted): a
///   step's input is freed before its charge goes.
#[test]
fn a_paths_buffers_are_charged_while_they_live() {
    if !alone() {
        return;
    }
    let either = || PropertyPathExpression::Alternative(vec![link("p"), link("missing")]);
    let fanned =
        |n: usize| default_store((0..n).map(|i| ("h".to_owned(), "p".to_owned(), format!("n{i}"))));
    let unlimited = |_| None;
    let refused = |rows: Result<usize, String>, _| match rows {
        Err(refusal) if refusal.contains("MemoryLimit") => Ok(()),
        other => Err(format!("answered {other:?}, not refused")),
    };
    // A sequence `p/q` whose middle nodes are `m<i>`: from `h` (T4), or from `x<i>` (T5).
    let chain = |n: usize, shared_start: bool| {
        default_store((0..n).flat_map(|i| {
            let start = match shared_start {
                true => "h".to_owned(),
                false => format!("x{i}"),
            };
            [
                (start, "p".to_owned(), format!("m{i}")),
                (format!("m{i}"), "q".to_owned(), format!("y{i}")),
            ]
        }))
    };
    let sequence = |steps: Vec<PropertyPathExpression>| PropertyPathExpression::Sequence(steps);
    let (wide, narrow) = ((32_769, 65_537), (4_097, 8_193));
    let failures: Vec<String> = [
        holds_nothing_uncharged_per_row(
            "T1 <h> (p|missing) ?y",
            wide,
            |n| (fanned(n), ("h", either(), "?y")),
            unlimited,
            rows_of,
        ),
        holds_nothing_uncharged_per_row(
            "T2 ?x (p|missing) ?y",
            wide,
            |n| (fanned(n), ("?x", either(), "?y")),
            unlimited,
            rows_of,
        ),
        holds_nothing_uncharged_per_row(
            "T3 ?x (p|missing) ?y refused under its columns' bytes",
            wide,
            |n| (fanned(n), ("?x", either(), "?y")),
            |n: usize| Some(2 * n * 8),
            refused,
        ),
        holds_nothing_uncharged_per_row(
            "T4 <h> p/q ?y",
            narrow,
            |n| {
                (
                    chain(n, true),
                    ("h", sequence(vec![link("p"), link("q")]), "?y"),
                )
            },
            unlimited,
            rows_of,
        ),
        holds_nothing_uncharged_per_row(
            "T5 ?x p/q ?y",
            narrow,
            |n| {
                (
                    chain(n, false),
                    ("?x", sequence(vec![link("p"), link("q")]), "?y"),
                )
            },
            unlimited,
            rows_of,
        ),
        holds_nothing_uncharged_per_row(
            "T6 ?x p/(q/r) ?y",
            narrow,
            |n| {
                let engine = default_store(
                    std::iter::once(("a".to_owned(), "p".to_owned(), "m".to_owned())).chain(
                        (0..n).flat_map(|i| {
                            [
                                ("m".to_owned(), "q".to_owned(), format!("x{i}")),
                                (format!("x{i}"), "r".to_owned(), format!("y{i}")),
                            ]
                        }),
                    ),
                );
                let inner = sequence(vec![link("q"), link("r")]);
                (engine, ("?x", sequence(vec![link("p"), inner]), "?y"))
            },
            unlimited,
            rows_of,
        ),
        holds_nothing_uncharged_per_row(
            "T7 <h> p/q <b>",
            narrow,
            |n| {
                let engine = default_store((0..n).flat_map(|i| {
                    [
                        ("h".to_owned(), "p".to_owned(), format!("m{i}")),
                        (format!("m{i}"), "q".to_owned(), "b".to_owned()),
                    ]
                }));
                (engine, ("h", sequence(vec![link("p"), link("q")]), "b"))
            },
            unlimited,
            rows_of,
        ),
    ]
    .into_iter()
    .filter_map(Result::err)
    .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Spec (sol-loop r1 S1 of the executor memory stack's second lead): a path with both ends
/// bound answers as many solutions as it connects the ends ways, a table without columns of
/// that many rows, charged before its rows are made. Over `a p a`, 64 steps of `(p|p)`
/// connect `a` to `a` 2^64 ways, saturated to `usize::MAX`: refused at once, within its
/// deadline. Made row by row and charged afterwards, they ran for good.
#[test]
fn a_both_bound_paths_rows_are_charged_before_they_are_made() {
    let (answered, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let engine = default_store([("a".to_owned(), "p".to_owned(), "a".to_owned())]);
        let snapshot = engine.snapshot();
        let options = QueryOptions::default();
        let ctx = Context::new(&snapshot, &options, None, None);
        let step = PropertyPathExpression::Alternative(vec![link("p"), link("p")]);
        let path = PropertyPathExpression::Sequence(vec![step; 64]);
        let solutions = ctx
            .path(&end("a"), &path, &end("a"))
            .map(|solutions| solutions.table.len())
            .map_err(|error| format!("{:?}", QueryEvaluationError::from(error)));
        let _ = answered.send(solutions);
    });
    let solutions = answer
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("answered within 30 s");
    assert!(
        matches!(&solutions, Err(refusal) if refusal.contains("MemoryLimit")),
        "{solutions:?}"
    );
}
