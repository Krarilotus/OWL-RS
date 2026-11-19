//! An expression with a new value at each evaluation (`BNODE()`, `RAND()`, `UUID()`,
//! `STRUUID()`) is evaluated as often as SPARQL evaluates it, however the planner joins: a
//! group's `BIND` or `FILTER` once per solution of the group, a filter above a join once
//! per joined row, an `OPTIONAL`'s condition once per pair of its group, an `EXISTS` once
//! per solution it tests. Values are random, so each test states a property every SPARQL
//! evaluation has and a wrong count of evaluations breaks.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
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

/// The rows `text` gives on `engine`'s data under `options`, each as its values of
/// `variables` (empty where unbound).
fn values_in(
    engine: &Engine,
    text: &str,
    options: &QueryOptions,
    variables: &[&str],
) -> Vec<Vec<String>> {
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let QueryResults::Solutions(solutions) =
        evaluate_query(&snapshot, &query, options).unwrap_or_else(|e| panic!("{e}: {text}"))
    else {
        panic!("solutions: {text}");
    };
    solutions
        .map(|row| {
            let row = row.unwrap();
            variables
                .iter()
                .map(|v| row.get(*v).map_or_else(String::new, ToString::to_string))
                .collect()
        })
        .collect()
}

/// The rows `text` gives on [`engine`]'s data, optimised or as written, each as its values
/// of `variables`.
fn values(text: &str, as_written: bool, variables: &[&str]) -> Vec<Vec<String>> {
    let options = QueryOptions {
        as_written,
        ..QueryOptions::default()
    };
    values_in(&engine(), text, &options, variables)
}

/// The number of rows `text` gives on [`engine`]'s data, optimised or as written.
fn rows(text: &str, as_written: bool) -> usize {
    values(text, as_written, &[]).len()
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
/// of the forty solutions is kept or not by its own test; the join copies each kept one to
/// both `VALUES ?k` rows, so the `?i` kept with `?k = 1` are those kept with `?k = 2`. A
/// test per joined row makes the two differ (a chance of 2^-40 per run that they agree).
/// This states the join's part only: the engine tests such a body once for all forty
/// solutions, a known fault left for a later release, which this test does not catch.
#[test]
fn an_exists_drawing_a_value_keeps_each_solution_of_its_group_for_every_joined_row_or_none() {
    let keys: Vec<String> = (1..=40).map(|i| i.to_string()).collect();
    let query = format!(
        "SELECT ?k ?i {{ VALUES ?k {{ 1 2 }} {{ VALUES ?i {{ {} }} \
         FILTER EXISTS {{ FILTER(RAND() < 0.5) }} }} }}",
        keys.join(" ")
    );
    for as_written in [false, true] {
        for _ in 0..5 {
            let mut kept = std::collections::BTreeMap::<String, Vec<String>>::new();
            for row in values(&query, as_written, &["k", "i"]) {
                kept.entry(row[0].clone()).or_default().push(row[1].clone());
            }
            kept.values_mut().for_each(|i| i.sort());
            let sets: Vec<_> = kept.values().collect();
            assert!(
                sets.is_empty() || (sets.len() == 2 && sets[0] == sets[1]),
                "kept per key: {kept:?}, as written: {as_written}"
            );
        }
    }
}

/// Spec: a subquery is evaluated once, alone, whatever joins it, so in the group
/// `{ ?s <urn:p> ?v . { SELECT (UUID() AS ?u) {} LIMIT 1 } FILTER(isIRI(?u)) }` it draws
/// once wherever the group is evaluated, and the group's triple pattern may still be
/// matched from the one `VALUES` row. Against 65,536 statements of the predicate the
/// answer is one row, within a memory budget (256 KiB) that holds it but not the 65,536.
#[test]
fn a_group_drawing_only_in_a_subquery_is_still_matched_from_the_joined_rows() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let p = NamedNode::new_unchecked("urn:p");
    for i in 0..65_536 {
        let s = NamedNode::new_unchecked(format!("urn:s:{i}"));
        let o = Literal::new_simple_literal("needle");
        tx.insert(Quad::new(s, p.clone(), o, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let query = "SELECT ?s ?v ?u { VALUES ?s { <urn:s:0> } \
                 { ?s <urn:p> ?v . { SELECT (UUID() AS ?u) WHERE {} LIMIT 1 } \
                   FILTER(isIRI(?u)) } }";
    let mut options = QueryOptions::default();
    options.resources.memory = nrese_exec::Envelope::new(Some(256 * 1024), None);
    let found = values_in(&engine, query, &options, &["s"]);
    assert_eq!(found, [["<urn:s:0>"]]);
}

/// Spec: in `?s a <urn:Rare> OPTIONAL { ?s <urn:label> ?l BIND(STRUUID() AS ?u) }`, and in
/// the same with the group joined instead, each solution of the group draws one `?u`, so
/// the two rare subjects get one row each with distinct values. Only the rare subjects'
/// labels can meet a row, so drawing for them alone gives SPARQL's answer, against 65,536
/// labelled subjects within a memory budget (256 KiB) that holds two rows but not the
/// 65,536 labels.
#[test]
fn a_drawing_group_met_by_distinct_keys_is_matched_from_them() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let label = NamedNode::new_unchecked("urn:label");
    for i in 0..65_536 {
        let s = NamedNode::new_unchecked(format!("urn:s:{i}"));
        let o = Literal::new_simple_literal(format!("label {i}"));
        tx.insert(Quad::new(s, label.clone(), o, GraphName::DefaultGraph).as_ref());
    }
    let a = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    for s in ["urn:s:7", "urn:s:9"] {
        let s = NamedNode::new_unchecked(s);
        let rare = NamedNode::new_unchecked("urn:Rare");
        tx.insert(Quad::new(s, a.clone(), rare, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let mut options = QueryOptions::default();
    options.resources.memory = nrese_exec::Envelope::new(Some(256 * 1024), None);
    for query in [
        "SELECT ?s ?u { ?s a <urn:Rare> \
         OPTIONAL { ?s <urn:label> ?l BIND(STRUUID() AS ?u) } }",
        "SELECT ?s ?u { ?s a <urn:Rare> { ?s <urn:label> ?l BIND(STRUUID() AS ?u) } }",
    ] {
        let mut found = values_in(&engine, query, &options, &["s", "u"]);
        found.sort();
        assert_eq!(found.len(), 2, "{found:?}: {query}");
        assert_eq!(found[0][0], "<urn:s:7>", "{query}");
        assert_eq!(found[1][0], "<urn:s:9>", "{query}");
        assert!(
            !found[0][1].is_empty() && found[0][1] != found[1][1],
            "{found:?}: {query}"
        );
    }
    // Two joined rows with the same key meet the one solution of the group, which draws
    // once: both rows get the same value.
    let query = "SELECT ?s ?u { VALUES (?s ?k) { (<urn:s:7> 1) (<urn:s:7> 2) } \
                 { ?s <urn:label> ?l BIND(STRUUID() AS ?u) } }";
    let found = values_in(&engine, query, &options, &["s", "u"]);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        !found[0][1].is_empty() && found[0][1] == found[1][1],
        "{found:?}"
    );
}

/// Spec: `FILTER(STRLEN(?gate) > 8 && RAND() < 0.5)` over `?s <urn:gate> ?gate ;
/// <urn:wide> ?wide` keeps no row whatever is drawn, since the one `?gate` is "no". The
/// condition that draws nothing may be applied as soon as `?gate` is bound, before the
/// 65,536 `?wide` statements are joined, so the empty answer comes within a memory budget
/// (256 KiB) that does not hold them.
#[test]
fn a_drawing_filter_keeps_its_other_conditions_applied_early() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let s = NamedNode::new_unchecked("urn:s");
    let gate = NamedNode::new_unchecked("urn:gate");
    let no = Literal::new_simple_literal("no");
    tx.insert(Quad::new(s.clone(), gate, no, GraphName::DefaultGraph).as_ref());
    let wide = NamedNode::new_unchecked("urn:wide");
    for i in 0..65_536 {
        let o = Literal::new_simple_literal(format!("wide {i}"));
        tx.insert(Quad::new(s.clone(), wide.clone(), o, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let query = "SELECT ?wide { VALUES ?s { <urn:s> } \
                 { ?s <urn:gate> ?gate ; <urn:wide> ?wide \
                   FILTER(STRLEN(?gate) > 8 && RAND() < 0.5) } }";
    let mut options = QueryOptions::default();
    options.resources.memory = nrese_exec::Envelope::new(Some(256 * 1024), None);
    assert!(values_in(&engine, query, &options, &["wide"]).is_empty());
}
