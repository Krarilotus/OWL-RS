//! Guard of rows without columns (SW1, `notes/2026-11-01-late-budget-r2.md`): a table with
//! no columns holds only a count, so a UNION of products of `{ {} UNION {} }` makes rows
//! that hold nothing, and an operator after it then makes a column per row (an unbound
//! variable's UNDEF in a projection, a BIND of one). Those rows count against the query's
//! budget where they are made, so a query whose rows need more than its budget fails with
//! its memory error before any operator allocates a column for them. Measured with a
//! counting allocator, so this file is a test binary of its own.

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::{Envelope, heap};
use nrese_sparql::{QueryEvaluationError, QueryOptions, QueryResults, Resources, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The query's memory limit.
const LIMIT: usize = 1 << 20;

/// What the query's thread may hold beyond its limit: the parsed query, its plan and the
/// evaluator's per-call bookkeeping, which grow with the query's text, not its rows.
const BOOKKEEPING: usize = 1 << 20;

/// 32 arms in one UNION, each a product of 16 `{ {} UNION {} }`: 2^16 rows an arm (each
/// within a join's row allowance), 2^21 in all. One column per row of them is 16 MiB.
fn rows_without_columns() -> String {
    let arm = format!("{{ {} }}", vec!["{ {} UNION {} }"; 16].join(" "));
    vec![arm; 32].join(" UNION ")
}

/// The outcome of `query` under [`LIMIT`] and the most its thread held while it ran.
fn run(query: &str, as_written: bool) -> (Result<usize, QueryEvaluationError>, usize) {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
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
    heap::start("zero-width");
    heap::begin_step();
    let outcome = match evaluate_query(&snapshot, &query, &options) {
        Ok(QueryResults::Solutions(mut rows)) => rows.try_fold(0, |n, row| row.map(|_| n + 1)),
        Ok(_) => panic!("solutions"),
        Err(error) => Err(error),
    };
    heap::end_step("zero-width");
    let peak = heap::steps().last().unwrap().peak;
    let _ = heap::finish();
    (outcome, peak)
}

/// Spec: a query's intermediate rows draw from its memory envelope, and one that needs more
/// fails with `MemoryLimit` (`Resources::memory`, the v1.5 bar's "no crash"). 2^21 rows
/// need more than 1 MiB as soon as anything holds a word per row, so the query is refused,
/// and nothing the query's thread held grew with its rows: at most the limit and the plan's
/// bookkeeping. On the default path and as written alike.
#[test]
fn rows_without_columns_draw_from_the_query_budget() {
    let rows = rows_without_columns();
    let queries = [
        (
            "an unbound variable projected",
            format!("SELECT ?x WHERE {{ {rows} }}"),
        ),
        (
            "an unbound variable bound",
            format!("SELECT ?y WHERE {{ {{ {rows} }} BIND(?u AS ?y) }}"),
        ),
    ];
    for (case, query) in &queries {
        for as_written in [false, true] {
            let (outcome, peak) = run(query, as_written);
            assert!(
                matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))),
                "{case}, as_written={as_written}: {:?}, not the memory error",
                outcome.map_err(|e| e.to_string())
            );
            assert!(
                peak <= LIMIT + BOOKKEEPING,
                "{case}, as_written={as_written}: the query's thread held {peak} bytes \
                 under a {LIMIT}-byte limit"
            );
        }
    }
}
