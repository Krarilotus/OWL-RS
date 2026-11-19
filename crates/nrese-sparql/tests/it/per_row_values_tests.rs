//! An expression with a new value at each evaluation (`BNODE()`, `RAND()`, `UUID()`,
//! `STRUUID()`) is evaluated as often as SPARQL evaluates it, however the planner joins: a
//! group's `BIND` or `FILTER` once per solution of the group, a filter above a join once
//! per joined row, an `OPTIONAL`'s condition once per pair of its group, an `EXISTS` once
//! per solution it tests. Values are random, so each test states a property every SPARQL
//! evaluation has and a wrong count of evaluations breaks.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

const LABEL: &str = "<http://www.w3.org/2000/01/rdf-schema#label>";

/// One statement: `<urn:a> rdfs:label "alpha one"`.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let data = format!("<urn:a> {LABEL} \"alpha one\" .");
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(data.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The number of rows `text` gives on [`engine`]'s data, optimised or as written.
fn rows(text: &str, as_written: bool) -> usize {
    let engine = engine();
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let options = QueryOptions {
        as_written,
        ..QueryOptions::default()
    };
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, &options).unwrap()
    else {
        panic!("solutions: {text}");
    };
    solutions.collect::<Result<Vec<_>, _>>().unwrap().len()
}

/// `VALUES ?k { 1 2 … 40 }`.
fn forty() -> String {
    let keys: Vec<String> = (1..=40).map(|k| k.to_string()).collect();
    format!("VALUES ?k {{ {} }}", keys.join(" "))
}

/// Spec: `{ BIND(BNODE() AS ?b) }` has one solution, so one blank node, which the join
/// with the two `VALUES` rows copies to both: `DISTINCT ?b` gives one row. The planner
/// evaluates the group from the `VALUES` rows; that must not draw a blank node per row.
#[test]
fn a_group_binding_a_blank_node_binds_one_per_solution_of_the_group() {
    let query = format!(
        "SELECT DISTINCT ?b {{ VALUES ?k {{ 1 2 }} {{ BIND(BNODE() AS ?b) }} \
         FILTER EXISTS {{ ?s {LABEL} ?o }} }}"
    );
    for as_written in [false, true] {
        assert_eq!(rows(&query, as_written), 1, "as written: {as_written}");
    }
}

/// Spec: the group `{ ?x rdfs:label ?l FILTER(RAND() < 0.5) }` has one solution before its
/// filter, which keeps it or not by one draw; the join copies the outcome to all forty
/// `VALUES` rows, so the answer has 0 or 40 rows. A draw per joined row gives about 20
/// (both ends together have a chance of 2^-39 per run).
#[test]
fn a_group_filter_drawing_a_value_keeps_its_solution_for_every_joined_row_or_none() {
    let query = format!(
        "SELECT ?k {{ {} {{ ?x {LABEL} ?l FILTER(RAND() < 0.5) }} }}",
        forty()
    );
    for as_written in [false, true] {
        for _ in 0..5 {
            let n = rows(&query, as_written);
            assert!(n == 0 || n == 40, "{n} rows, as written: {as_written}");
        }
    }
}

/// Spec: the planner's other routes keep the counts too. An `OPTIONAL`'s right side,
/// evaluated from the left side's values, binds one blank node for its one solution
/// (`DISTINCT ?b`: one row). A filter on the joined rows draws per joined row, so of forty
/// rows some are kept and some not (all or none: a chance of 2^-39 per run), wherever
/// filter pushdown would move it.
#[test]
fn optional_sides_and_pushed_filters_evaluate_as_often_as_sparql_does() {
    let optional = format!(
        "SELECT DISTINCT ?b {{ VALUES ?k {{ 1 2 }} ?x {LABEL} ?l \
         OPTIONAL {{ ?x {LABEL} ?m BIND(BNODE() AS ?b) }} }}"
    );
    let joined = format!(
        "SELECT ?k {{ {} ?x {LABEL} ?l FILTER(RAND() < 0.5) }}",
        forty()
    );
    for as_written in [false, true] {
        assert_eq!(
            rows(&optional, as_written),
            1,
            "optional, as written: {as_written}"
        );
        let n = rows(&joined, as_written);
        assert!(
            0 < n && n < 40,
            "{n} rows of the joined filter, as written: {as_written}"
        );
    }
}

/// Spec: the group `{ <urn:a> rdfs:label ?l OPTIONAL { <urn:a> rdfs:label ?o
/// FILTER(RAND() < 0.5) } }` has one pair for its `OPTIONAL`, decided by one draw; the
/// join copies the outcome to all forty `VALUES` rows, so `FILTER(BOUND(?o))` keeps 0 or
/// 40 of them.
#[test]
fn an_optional_condition_drawing_a_value_decides_once_for_its_group() {
    let query = format!(
        "SELECT ?k {{ {} {{ <urn:a> {LABEL} ?l OPTIONAL {{ <urn:a> {LABEL} ?o \
         FILTER(RAND() < 0.5) }} }} FILTER(BOUND(?o)) }}",
        forty()
    );
    for as_written in [false, true] {
        for _ in 0..5 {
            let n = rows(&query, as_written);
            assert!(n == 0 || n == 40, "{n} rows, as written: {as_written}");
        }
    }
}

/// Spec: in the group `{ VALUES ?i { 1 … 40 } FILTER EXISTS { FILTER(RAND() < 0.5) } }` each
/// of the forty solutions draws once; the join copies each kept one to both `VALUES ?k`
/// rows, so the answer has an even number of rows (2 per kept solution).
#[test]
fn an_exists_drawing_a_value_draws_once_per_solution_of_its_group() {
    let keys: Vec<String> = (1..=40).map(|i| i.to_string()).collect();
    let query = format!(
        "SELECT ?k ?i {{ VALUES ?k {{ 1 2 }} {{ VALUES ?i {{ {} }} \
         FILTER EXISTS {{ FILTER(RAND() < 0.5) }} }} }}",
        keys.join(" ")
    );
    for as_written in [false, true] {
        for _ in 0..5 {
            let n = rows(&query, as_written);
            assert!(n.is_multiple_of(2), "{n} rows, as written: {as_written}");
        }
    }
}
