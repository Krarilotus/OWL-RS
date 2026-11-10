//! Guard of scans that decode, merge or grow a query's table before it is charged
//! (notes/2026-11-09-d039-scan-charges.md): a scan's columns are decoded from the index, a
//! scan over both stacks merges two column sets into a third, a scan the index can't answer
//! a column at a time grows its table row by row, and a distinct walk makes a column of the
//! groups it found. Each draws from the query's budget before it allocates, so a query whose
//! scan outgrows its limit fails with `MemoryLimit` while its thread holds at most the limit
//! and its bookkeeping. Measured with a counting allocator, so this file is a test binary of
//! its own.

use nrese_engine::{EncodedTriple, Engine, EngineConfig};
use nrese_exec::{Envelope, heap};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql::{QueryEvaluationError, QueryOptions, QueryResults, Resources, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The query's memory limit.
const LIMIT: usize = 256 << 10;

/// What the query's thread may hold beyond its limit: the parsed query, its plan and the
/// evaluator's per-call bookkeeping, which grow with the query's text, not its rows.
const BOOKKEEPING: usize = 1 << 20;

const EX: &str = "http://example.com/";

/// Matching statements: one column of them is 2 MiB, eight times the limit.
const ROWS: usize = 1 << 18;

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
fn run(engine: &Engine, query: &str) -> (Result<usize, QueryEvaluationError>, usize) {
    let snapshot = engine.snapshot();
    let options = QueryOptions {
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
    heap::start("scan");
    heap::begin_step();
    let outcome = match evaluate_query(&snapshot, &query, &options) {
        Ok(QueryResults::Solutions(mut rows)) => rows.try_fold(0, |n, row| row.map(|_| n + 1)),
        Ok(_) => panic!("solutions"),
        Err(error) => Err(error),
    };
    heap::end_step("scan");
    let peak = heap::steps().last().unwrap().peak;
    let _ = heap::finish();
    (outcome, peak)
}

/// `query` over `engine` fails with `MemoryLimit`, its thread holding at most the limit and
/// its bookkeeping.
fn refused_within_the_limit(case: &str, engine: &Engine, query: &str) {
    let (outcome, peak) = run(engine, query);
    assert!(
        matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))),
        "{case}: {:?}, not the memory error",
        outcome.map_err(|e| e.to_string())
    );
    assert!(
        peak <= LIMIT + BOOKKEEPING,
        "{case}: the query's thread held {peak} bytes under a {LIMIT}-byte limit"
    );
}

/// Spec: a query's intermediate results draw from its memory envelope (`Resources::memory`,
/// the v1.5 bar's "no crash"), however the index hands them over. 2^18 statements of
/// `ex:p` are two columns of 2 MiB each, which the index decodes a column at a time: more
/// than 256 KiB, so `SELECT ?s ?o { ?s ex:p ?o }` fails with `MemoryLimit` before the
/// columns are decoded.
#[test]
fn a_scans_columns_draw_from_the_query_budget_before_they_are_decoded() {
    let p = ex("p");
    let engine = engine((0..ROWS).map(|i| {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
    }));
    refused_within_the_limit(
        "columns decoded from one stack",
        &engine,
        &format!("SELECT ?s ?o WHERE {{ ?s <{EX}p> ?o }}"),
    );
}

/// Spec: as above, for matches in both stacks: 2^17 asserted and 2^17 inferred statements
/// of `ex:p` are decoded per stack and merged in the scan's order into columns of 2^18
/// rows, all of which exist at once. More than 256 KiB, so the query fails with
/// `MemoryLimit` before any of them is made.
#[test]
fn a_scan_over_both_stacks_draws_its_parts_and_their_merge_from_the_query_budget() {
    let half = ROWS / 2;
    let p = ex("p");
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for i in 0..half {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        tx.insert(Quad::new(subject, p.clone(), object, GraphName::DefaultGraph).as_ref());
        // Terms of the inferred statements, stated under another predicate.
        let (subject, object) = (ex(&format!("t{i}")), ex(&format!("u{i}")));
        tx.insert(Quad::new(subject, ex("q"), object, GraphName::DefaultGraph).as_ref());
    }
    let id = |tx: &nrese_engine::Transaction<'_>, node: NamedNode| {
        tx.lookup(nrese_rdf::TermRef::from(&node))
            .expect("a stated term")
    };
    let p_id = id(&tx, p.clone());
    for i in 0..half {
        let (subject, object) = (ex(&format!("t{i}")), ex(&format!("u{i}")));
        let triple = EncodedTriple::new(id(&tx, subject), p_id, id(&tx, object));
        assert!(tx.insert_inferred(triple));
    }
    tx.commit().unwrap();
    refused_within_the_limit(
        "columns of both stacks, merged",
        &engine,
        &format!("SELECT ?s ?o WHERE {{ ?s <{EX}p> ?o }}"),
    );
}

/// Spec: as above, for a scan the index can't answer a column at a time: a variable twice
/// in one pattern binds the same term (SPARQL 1.1 §18.3), so `?s ex:p ?s` is matched quad
/// by quad. Over 2^18 statements `ex:s<i> ex:p ex:s<i>` its table grows to one column of
/// 2 MiB, more than 256 KiB: the query fails with `MemoryLimit` as the table outgrows its
/// limit, not after.
#[test]
fn a_scan_matched_quad_by_quad_draws_its_table_from_the_query_budget_as_it_grows() {
    let p = ex("p");
    let engine = engine((0..ROWS).map(|i| {
        let node = ex(&format!("s{i}"));
        Quad::new(node.clone(), p.clone(), node, GraphName::DefaultGraph)
    }));
    refused_within_the_limit(
        "a variable twice in one pattern",
        &engine,
        &format!("SELECT ?s WHERE {{ ?s <{EX}p> ?s }}"),
    );
}

/// Spec: as above, for the distinct values of a variable found by a walk over the index's
/// groups: 2^18 subjects of `ex:p` are a column of 2 MiB, more than 256 KiB, so
/// `SELECT DISTINCT ?s { ?s ex:p ?o }` fails with `MemoryLimit` before the column is made.
#[test]
fn a_distinct_walks_column_draws_from_the_query_budget_before_it_is_made() {
    let p = ex("p");
    let engine = engine((0..ROWS).map(|i| {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
    }));
    refused_within_the_limit(
        "the distinct values of a group walk",
        &engine,
        &format!("SELECT DISTINCT ?s WHERE {{ ?s <{EX}p> ?o }}"),
    );
}

/// Spec: as above, for a count per group answered from the index's groups without reading
/// the matches: 2^18 subjects of `ex:p` are 2^18 groups of a value and a count (4 MiB), more
/// than 256 KiB, so `SELECT ?s (COUNT(*) AS ?n) { ?s ex:p ?o } GROUP BY ?s` fails with
/// `MemoryLimit` before the groups are listed.
#[test]
fn a_count_per_group_draws_its_groups_from_the_query_budget() {
    let p = ex("p");
    let engine = engine((0..ROWS).map(|i| {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
    }));
    refused_within_the_limit(
        "the groups of a grouped count",
        &engine,
        &format!("SELECT ?s (COUNT(*) AS ?n) WHERE {{ ?s <{EX}p> ?o }} GROUP BY ?s"),
    );
}

/// Spec: as above, for the graph's nodes a zero-length path starts from (SPARQL 1.1 §18.5:
/// `p*` pairs every node of the graph with itself, whatever its predicates): one statement
/// of `ex:p` and 2^18 statements `ex:s<i> ex:q ex:o<i>` give 2^18 distinct subjects and
/// 2^18 distinct objects, each listed by a walk over the index's groups (4 MiB each) to count
/// them, while `ex:p`'s edges are one. More than 256 KiB, so
/// `SELECT (COUNT(*) AS ?n) { ?s ex:p* ?o }` fails with `MemoryLimit` before they are listed.
#[test]
fn the_nodes_a_zero_length_path_counts_draw_from_the_query_budget() {
    let (p, q) = (ex("p"), ex("q"));
    let edge = Quad::new(ex("a"), p, ex("b"), GraphName::DefaultGraph);
    let engine = engine(std::iter::once(edge).chain((0..ROWS).map(|i| {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        Quad::new(subject, q.clone(), object, GraphName::DefaultGraph)
    })));
    refused_within_the_limit(
        "the nodes of a zero-length path",
        &engine,
        &format!("SELECT (COUNT(*) AS ?n) WHERE {{ ?s <{EX}p>* ?o }}"),
    );
}
