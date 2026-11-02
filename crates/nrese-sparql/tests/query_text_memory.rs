//! Guard of allocations the query's text multiplies (notes/2026-11-02-column-budget.md): a
//! projection makes a column per unbound variable and a UNION one per variable a side
//! lacks, so k variables in the text make k columns of a charged table's rows; and a
//! sequence of alternatives multiplies the ways through a node, `(p|p)/(p|p)` reaching it
//! four times over one statement. Both draw from the query's budget where they are made:
//! the columns are charged before they are made, and a path's ends are held once each,
//! with how often they are reached. Measured with a counting allocator, so this file is a
//! test binary of its own.

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::{Envelope, heap};
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
        Ok(_) => panic!("solutions"),
        Err(error) => Err(error),
    };
    heap::end_step("query-text");
    let peak = heap::steps().last().unwrap().peak;
    let _ = heap::finish();
    (outcome, peak)
}

/// Spec: a query's intermediate results draw from its memory envelope (`Resources::memory`,
/// the v1.5 bar's "no crash"), wherever their size comes from. Over 4,096 rows, 256
/// unbound variables projected, or a UNION with a side binding 256 others, make 32 MiB of
/// columns: more than 1 MiB, so the query fails with `MemoryLimit`, its thread holding at
/// most the limit and its bookkeeping. 4 variables make 128 KiB, which fits: answered.
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
