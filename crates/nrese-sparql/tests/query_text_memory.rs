//! Guard of allocations the query's text multiplies (notes/2026-11-02-column-budget.md): a
//! projection makes a column per unbound variable and a UNION one per variable a side
//! lacks, so k variables in the text make k columns of a charged table's rows; and a
//! sequence of alternatives multiplies the ways through a node, `(p|p)/(p|p)` reaching it
//! four times over one statement. Both draw from the query's budget where they are made:
//! the columns are charged before they are made, and a path's ends are held once each,
//! with how often they are reached. Measured with a counting allocator, so this file is a
//! test binary of its own.

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::{Envelope, SharedBudget, heap};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql::{QueryEvaluationError, QueryOptions, QueryResults, Resources, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The query's memory limit.
const LIMIT: usize = 1 << 20;

/// What the query's thread may hold beyond its limit: the parsed query, its plan and the
/// evaluator's per-call bookkeeping, which grow with the query's text, not its rows.
const BOOKKEEPING: usize = 1 << 20;

const EX: &str = "http://example.com/";

/// Statements `ex:s<i> ex:p ex:o<i>` for `0..ROWS`: one column of them is 32 KiB.
const ROWS: usize = 1 << 12;

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn engine(statements: impl IntoIterator<Item = Quad>) -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for quad in statements {
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The outcome of `query` under [`LIMIT`] (its rows) and the most its thread held.
fn run(
    engine: &Engine,
    query: &str,
    as_written: bool,
) -> (Result<usize, QueryEvaluationError>, usize) {
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        as_written,
        resources: Resources {
            memory: Envelope::new(Some(LIMIT), None),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let query = SparqlParser::new().parse_query(query).unwrap();
    // The profile is the process's: one measured run at a time.
    static MEASURING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _alone = MEASURING.lock().unwrap_or_else(|p| p.into_inner());
    heap::start("query-text");
    heap::begin_step();
    let outcome = match evaluate_query(&snapshot, &query, &options) {
        Ok(QueryResults::Solutions(mut rows)) => rows.try_fold(0, |n, row| row.map(|_| n + 1)),
        Ok(QueryResults::Graph(mut triples)) => triples.try_fold(0, |n, t| t.map(|_| n + 1)),
        Ok(_) => panic!("solutions or a graph"),
        Err(error) => Err(error),
    };
    heap::end_step("query-text");
    let peak = heap::steps().last().unwrap().peak;
    let _ = heap::finish();
    (outcome, peak)
}

/// Spec: a query's intermediate results draw from its memory envelope (`Resources::memory`,
/// the v1.5 bar's "no crash"), wherever their size comes from. Over 4,096 rows, 256
/// unbound variables projected, or a UNION with a side binding 256 others, make 8 MiB of
/// columns (256 × 4,096 × 8 bytes): more than 1 MiB, so the query fails with
/// `MemoryLimit`, its thread holding at most the limit and its bookkeeping. 4 variables
/// make 128 KiB, which fits: answered.
/// On the default path and as written alike.
#[test]
fn columns_made_per_variable_draw_from_the_query_budget() {
    let p = ex("p");
    let engine = engine((0..ROWS).map(|i| {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
    }));
    let rows = format!("?s <{EX}p> ?o");
    for (k, fits) in [(4usize, true), (256, false)] {
        let vars = (0..k)
            .map(|i| format!("?a{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let queries = [
            (
                "unbound variables projected",
                format!("SELECT ?s {vars} WHERE {{ {rows} }}"),
            ),
            (
                "a UNION's other side's variables",
                format!("SELECT * WHERE {{ {{ {rows} }} UNION {{ VALUES ({vars}) {{ }} }} }}"),
            ),
        ];
        for (case, query) in &queries {
            for as_written in [false, true] {
                let (outcome, peak) = run(&engine, query, as_written);
                let case = format!("{case}, {k} of them, as_written={as_written}");
                if fits {
                    assert_eq!(outcome.map_err(|e| e.to_string()), Ok(ROWS), "{case}");
                } else {
                    assert!(
                        matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))),
                        "{case}: {:?}, not the memory error",
                        outcome.map_err(|e| e.to_string())
                    );
                }
                assert!(
                    peak <= LIMIT + BOOKKEEPING,
                    "{case}: the query's thread held {peak} bytes under a {LIMIT}-byte limit"
                );
            }
        }
    }
}

/// Spec: SPARQL 1.1 §18.4: `p+` reaches each end once per start, however many ways lead
/// there. Over one statement `ex:a ex:p ex:a`, a sequence of 20 `(ex:p|ex:p)` has 2^20
/// ways from `ex:a` to `ex:a`, one end: `ex:a ((ex:p|ex:p)/…)+ ?y` answers one row, and
/// the query's thread holds at most the limit and its bookkeeping (a copy of the end per
/// way through would be 8 MiB). On the default path and as written alike.
#[test]
fn a_closure_over_a_sequence_holds_its_ends_once() {
    let engine = engine([Quad::new(
        ex("a"),
        ex("p"),
        ex("a"),
        GraphName::DefaultGraph,
    )]);
    let sequence = vec![format!("(<{EX}p>|<{EX}p>)"); 20].join("/");
    let query = format!("SELECT ?y WHERE {{ <{EX}a> ({sequence})+ ?y }}");
    for as_written in [false, true] {
        let (outcome, peak) = run(&engine, &query, as_written);
        assert_eq!(
            outcome.map_err(|e| e.to_string()),
            Ok(1),
            "as_written={as_written}"
        );
        assert!(
            peak <= LIMIT + BOOKKEEPING,
            "as_written={as_written}: the query's thread held {peak} bytes under a \
             {LIMIT}-byte limit"
        );
    }
}

/// Spec: as above, for the other places where the query's text multiplies what a query
/// makes per row (notes/2026-11-02-column-budget.md CB-6, S1-S6). Over 4,096 rows, 256 of
/// each make 8 MiB or more (256 × 4,096 × 8 bytes) where each is a column or a term per
/// row: a column per EXISTS (S1), per GROUP BY variable (S2), per LATERAL variable put in
/// (S4), a sort key per
/// ORDER BY expression (S3), the 256 strings a CONCAT of 256 parts interns per row (S5):
/// more than 1 MiB, so the query fails with `MemoryLimit`. What only repeats what is there
/// makes nothing: ORDER BY unbound variables sorts by nothing (§15.1: unbound sorts first,
/// so all rows tie), and a closure over 256 times the same link reaches what one does
/// (§18.4: `p+` is a set), each answered. A CONSTRUCT of 256 template triples makes 1 M
/// triples, or 16 MiB from one row with a 64 KiB literal, and SAMPLE of that CONCAT a 6 KiB
/// string per group (4,096 groups), and a GROUP_CONCAT whose 4 KiB separator comes
/// between each two of 4,096 values (16 MiB): refused; over IRIs it has no string
/// (§18.5.1.7: unbound), so nothing to make, however long its separator: answered.
/// Either way, the query's thread holds at most the limit and its bookkeeping. On the
/// default path and as written alike.
#[test]
fn what_the_query_text_multiplies_draws_from_the_query_budget() {
    let p = ex("p");
    // And one statement whose object is a 64 KiB literal.
    let large = nrese_rdf::Literal::new_simple_literal("x".repeat(1 << 16));
    let large = Quad::new(ex("large"), ex("q"), large, GraphName::DefaultGraph);
    let engine = engine(
        (0..ROWS)
            .map(|i| {
                let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
                Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
            })
            .chain([large]),
    );
    let rows = format!("?s <{EX}p> ?o");
    let list = |item: &dyn Fn(usize) -> String, separator: &str| {
        (0..256).map(item).collect::<Vec<_>>().join(separator)
    };
    let exists = list(&|_| format!("EXISTS {{ {rows} }}"), " || ");
    let unbound = list(&|i| format!("?u{i}"), " ");
    let left = list(&|i| format!("?a{i}"), " ");
    let ones = list(&|_| "1".to_owned(), " ");
    let mentioned = list(&|i| format!("?a{i} != 0"), " && ");
    let keys = list(&|i| format!("CONCAT(STR(?s), \"{i}\")"), " ");
    let parts = list(&|_| "STR(?s)".to_owned(), ", ");
    let links = list(&|_| format!("<{EX}p>"), "|");
    let template = list(&|i| format!("?s <{EX}t{i}> ?o ."), " ");
    let separator = "x".repeat(1 << 12);
    let cases: [(&str, String, Option<usize>); 13] = [
        (
            "S1 FILTER of 256 EXISTS",
            format!("SELECT ?s WHERE {{ {rows} FILTER({exists}) }}"),
            None,
        ),
        (
            "S1 BIND of 256 EXISTS",
            format!("SELECT ?s WHERE {{ {rows} BIND(({exists}) AS ?b) }}"),
            None,
        ),
        (
            "S2 GROUP BY 256 unbound variables",
            format!("SELECT ?s WHERE {{ {rows} }} GROUP BY ?s {unbound}"),
            None,
        ),
        (
            "S3 ORDER BY 256 expressions",
            format!("SELECT ?s WHERE {{ {rows} }} ORDER BY {keys}"),
            None,
        ),
        (
            "S3 ORDER BY 256 unbound variables",
            format!("SELECT ?s WHERE {{ {rows} }} ORDER BY {unbound}"),
            Some(ROWS),
        ),
        (
            "S4 LATERAL putting in 256 variables",
            format!(
                "SELECT ?s WHERE {{ VALUES ({left}) {{ ({ones}) }} \
                 LATERAL {{ {rows} FILTER({mentioned}) }} }}"
            ),
            None,
        ),
        (
            "S5 CONCAT of 256 parts",
            format!("SELECT ?x WHERE {{ {rows} BIND(CONCAT({parts}) AS ?x) }}"),
            None,
        ),
        (
            "S6 closure of 256 times one link",
            format!("SELECT ?x ?y WHERE {{ ?x ({links})+ ?y }}"),
            Some(ROWS),
        ),
        (
            "CONSTRUCT of 256 template triples",
            format!("CONSTRUCT {{ {template} }} WHERE {{ {rows} }}"),
            None,
        ),
        (
            "CONSTRUCT of 256 template triples of one 64 KiB literal",
            format!("CONSTRUCT {{ {template} }} WHERE {{ ?s <{EX}q> ?o }}"),
            None,
        ),
        (
            "SAMPLE of a CONCAT of 256 parts per group",
            format!("SELECT ?s (SAMPLE(CONCAT({parts})) AS ?v) WHERE {{ {rows} }} GROUP BY ?s"),
            None,
        ),
        (
            "GROUP_CONCAT with a 4 KiB separator",
            format!(
                "SELECT (GROUP_CONCAT(STR(?s); SEPARATOR=\"{separator}\") AS ?v) \
                 WHERE {{ {rows} }}"
            ),
            None,
        ),
        (
            "GROUP_CONCAT of 64 IRIs with a 32 KiB separator (unbound)",
            format!(
                "SELECT (COALESCE(GROUP_CONCAT(?s; SEPARATOR=\"{}\"), \"none\") AS ?v) \
                 WHERE {{ {{ SELECT ?s WHERE {{ {rows} }} LIMIT 64 }} }}",
                "x".repeat(1 << 15)
            ),
            Some(1),
        ),
    ];
    // Every case runs, and each one that fails is listed.
    let mut failed = Vec::new();
    for (case, query, answered) in &cases {
        for as_written in [false, true] {
            let (outcome, peak) = run(&engine, query, as_written);
            let case = format!("{case}, as_written={as_written}");
            let outcome = match outcome {
                Err(QueryEvaluationError::MemoryLimit(_)) => None,
                other => Some(other.map_err(|e| e.to_string())),
            };
            if outcome != (*answered).map(Ok) {
                failed.push(format!(
                    "{case}: {outcome:?}, not {answered:?} (None: the memory error)"
                ));
            }
            if peak > LIMIT + BOOKKEEPING {
                failed.push(format!(
                    "{case}: held {peak} bytes under a {LIMIT}-byte limit"
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Spec: a table the query holds stays charged while it lives (`Resources::memory`): the
/// live result of a SELECT keeps at least a word a row and column charged. A projection
/// that drops a column must release only that column's charge: the graph's column that
/// `GRAPH ?g` adds per graph (one named graph here), and one of two columns a `GROUP BY`
/// of the same variable twice makes (SPARQL 1.1 §11.2: grouping on `?x ?x` groups on
/// `?x`; one row). On the default path and as written alike.
#[test]
fn projections_release_only_the_columns_they_drop() {
    let graph = GraphName::NamedNode(ex("g"));
    let engine = engine([Quad::new(ex("a"), ex("p"), ex("a"), graph)]);
    let values = (0..1_000).map(|i| i.to_string()).collect::<Vec<_>>();
    let cases = [
        (
            "the graph's column dropped",
            format!(
                "SELECT ?x WHERE {{ GRAPH ?g {{ VALUES ?x {{ {} }} }} }}",
                values.join(" ")
            ),
            1_000,
        ),
        (
            "a variable grouped on twice",
            "SELECT ?x WHERE { VALUES ?x { 1 } } GROUP BY ?x ?x".to_owned(),
            1,
        ),
    ];
    for (case, query, expected) in &cases {
        for as_written in [false, true] {
            let shared = SharedBudget::new(1 << 30);
            let options = QueryOptions {
                as_written,
                resources: Resources {
                    memory: Envelope::new(None, Some(shared.clone())),
                    ..Resources::default()
                },
                ..QueryOptions::default()
            };
            let parsed = SparqlParser::new().parse_query(query).unwrap();
            let snapshot = engine.snapshot();
            let result = evaluate_query(&snapshot, &parsed, &options);
            let held = shared.used();
            let Ok(QueryResults::Solutions(rows)) = result else {
                panic!("{case}, as_written={as_written}: no solutions");
            };
            let rows = rows.count();
            assert_eq!(rows, *expected, "{case}, as_written={as_written}");
            assert!(
                held >= rows * 8,
                "{case}, as_written={as_written}: the live result of {rows} rows keeps \
                 {held} bytes charged"
            );
        }
    }
}
