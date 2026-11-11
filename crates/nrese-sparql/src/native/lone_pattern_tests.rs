//! A query of one triple pattern reads only what it uses: outside EXPLAIN its BGP takes no
//! count to plan joins, and its preparation keeps the query's own pattern where nothing
//! rewrites it.

use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};

/// `:a :p :b`, and two `:b :q` objects.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data = "@prefix : <http://e/> . :a :p :b . :b :q :c , :d .";
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(data.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

const ONE: &str = "SELECT ?s WHERE { ?s <http://e/p> <http://e/b> }";
const TWO: &str = "SELECT ?s ?o WHERE { ?s <http://e/p> ?m . ?m <http://e/q> ?o }";

/// The patterns `text`'s BGPs counted while it was evaluated, and its solutions.
fn counted(snapshot: &Snapshot, text: &str) -> (usize, usize) {
    let query = Query::parse(text, None).unwrap_or_else(|e| panic!("{e}: {text}"));
    let options = QueryOptions::default();
    let ctx = Context::new(
        snapshot,
        &options,
        query_dataset(&query),
        query_base(&query),
    );
    let (pattern, ..) = native_pattern(&query, &options, &ctx).unwrap();
    BGP_ESTIMATES.set(0);
    let rows = ctx
        .eval_root(&pattern, None)
        .map_err(QueryEvaluationError::from)
        .unwrap()
        .table
        .len();
    (BGP_ESTIMATES.get(), rows)
}

/// Spec: a BGP's patterns are counted to order its joins, to size the later ones and to
/// give EXPLAIN its estimates (and, under a string filter, to weigh a dictionary pass,
/// which these queries have none of). A lone pattern has none of these to do outside
/// EXPLAIN: it is scanned, not counted. Each of two patterns is counted once. EXPLAIN of a
/// lone pattern still estimates its rows, from its exact count.
#[test]
fn a_lone_pattern_is_read_once() {
    let engine = engine();
    let snapshot = engine.snapshot();
    assert_eq!(counted(&snapshot, ONE), (0, 1), "{ONE}: (counted, rows)");
    assert_eq!(counted(&snapshot, TWO), (2, 2), "{TWO}: (counted, rows)");
    let query = Query::parse(ONE, None).unwrap();
    let (_, steps, rows, ..) = explain(&snapshot, &query, &QueryOptions::default()).unwrap();
    assert_eq!(rows, 1);
    let scan = steps
        .iter()
        .find(|step| step.operator.ends_with("scan"))
        .expect("EXPLAIN shows the scan");
    assert_eq!(scan.estimated_rows, Some(1), "EXPLAIN's scan estimate");
}

/// Whether `a` is `b` itself, not a copy.
fn same(a: &GraphPattern, b: &GraphPattern) -> bool {
    std::ptr::eq(a, b)
}

/// Spec: a query that nothing rewrites (no open triple term, QL rewriting, canonical
/// answers, rewrite pass that changes it, ASK limit or pre-bound values) is prepared
/// without copying its pattern: the executor runs the query's own.
#[test]
fn a_query_nothing_rewrites_is_prepared_without_a_copy() {
    let engine = engine();
    let snapshot = engine.snapshot();
    for text in [ONE, TWO] {
        let query = Query::parse(text, None).unwrap();
        let options = QueryOptions::default();
        let ctx = Context::new(
            &snapshot,
            &options,
            query_dataset(&query),
            query_base(&query),
        );
        let (prepared, ..) = native_pattern(&query, &options, &ctx).unwrap();
        assert!(
            same(&prepared, pattern_and_form(&query).0),
            "{text}: copied"
        );
    }
}
