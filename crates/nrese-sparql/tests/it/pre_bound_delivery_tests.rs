//! A pre-bound variable (`QueryOptions::pre_bound`, SHACL §5.6 pre-binding) holds its
//! value in every delivery of a query's answer (Q-02): ordinary and typed results, the
//! direct JSON, TSV and CSV writer (`write_results`: "byte for byte what serialising
//! `evaluate_query`'s results gives"), EXPLAIN ANALYZE's row count, and the plan EXPLAIN
//! shows (`plan_query`: "the plan `query` would run as").
//!
//! The data: `<urn:s1> <urn:p> <urn:a>`, `<urn:s2> <urn:p> <urn:b>`, `<urn:s3> <urn:p> _:n`.
//! Each case states the answer pre-binding gives by SHACL §5.6 (the value put in for the
//! variable throughout the query, subqueries included).

use std::collections::HashMap;
use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{BlankNode, GraphName, NamedNode, Quad, Term, Variable};
use nrese_sparql::{
    QueryOptions, QueryResults, ResultsFormat, TypedResults, evaluate_query, evaluate_query_typed,
    explain_query, plan_query, write_results,
};
use nrese_sparql_results::{QueryResultsFormat, QueryResultsSerializer};
use nrese_sparql_syntax::{Query, SparqlParser};

fn iri(text: &str) -> NamedNode {
    NamedNode::new_unchecked(text)
}

fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let p = iri("urn:p");
    for (s, o) in [
        ("urn:s1", Term::from(iri("urn:a"))),
        ("urn:s2", iri("urn:b").into()),
        ("urn:s3", BlankNode::new_unchecked("n").into()),
    ] {
        tx.insert(Quad::new(iri(s), p.clone(), o, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The stored blank node `<urn:s3>` points to, as the store names it.
fn stored_blank(engine: &Engine) -> Term {
    let query = parse("SELECT ?o WHERE { <urn:s3> <urn:p> ?o }");
    let snapshot = engine.snapshot();
    let QueryResults::Solutions(mut solutions) =
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    else {
        panic!("solutions")
    };
    let term = solutions.next().unwrap().unwrap().get("o").unwrap().clone();
    assert!(matches!(term, Term::BlankNode(_)), "{term}");
    term
}

fn parse(text: &str) -> Query {
    SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn with_this(value: Term) -> QueryOptions {
    QueryOptions {
        pre_bound: Some(Arc::new(HashMap::from([(
            Variable::new_unchecked("this"),
            value,
        )]))),
        ..QueryOptions::default()
    }
}

/// A SELECT's rows as sorted lines, each value in projection order.
fn lines(rows: impl Iterator<Item = Vec<Option<Term>>>) -> Vec<String> {
    let mut out: Vec<String> = rows
        .map(|row| {
            row.iter()
                .map(|t| t.as_ref().map_or("-".to_owned(), ToString::to_string))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    out.sort();
    out
}

fn serialised(results: QueryResults<'_>, format: QueryResultsFormat) -> Vec<u8> {
    let serializer = QueryResultsSerializer::from_format(format);
    let mut out = Vec::new();
    match results {
        QueryResults::Solutions(solutions) => {
            let mut writer = serializer
                .serialize_solutions_to_writer(&mut out, solutions.variables().to_vec())
                .unwrap();
            for solution in solutions {
                writer.serialize(&solution.unwrap()).unwrap();
            }
            writer.finish().unwrap();
        }
        QueryResults::Boolean(value) => {
            serializer
                .serialize_boolean_to_writer(&mut out, value)
                .unwrap();
        }
        QueryResults::Graph(_) => unreachable!("SELECT and ASK only"),
    }
    out
}

/// Every delivery of `text` under `options` against `expected`, the SELECT's rows: each
/// reported on its own.
fn assert_every_delivery(engine: &Engine, text: &str, options: &QueryOptions, expected: &[&str]) {
    let snapshot = engine.snapshot();
    let query = parse(text);
    let mut wrong = Vec::new();

    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, options).unwrap()
    else {
        panic!("a SELECT")
    };
    let variables = solutions.variables().to_vec();
    let ordinary = lines(solutions.map(|s| {
        let s = s.unwrap();
        variables.iter().map(|v| s.get(v).cloned()).collect()
    }));
    if ordinary != expected {
        wrong.push(format!("ordinary: {ordinary:?}"));
    }

    match evaluate_query_typed(&snapshot, &query, options).unwrap() {
        TypedResults::Solutions(table) => {
            let typed = lines((0..table.len()).map(|r| table.row(r)));
            if typed != expected {
                wrong.push(format!("typed: {typed:?}"));
            }
        }
        _ => wrong.push("typed: not solutions".to_owned()),
    }

    for (format, reference) in [
        (ResultsFormat::Json, QueryResultsFormat::Json),
        (ResultsFormat::Tsv, QueryResultsFormat::Tsv),
        (ResultsFormat::Csv, QueryResultsFormat::Csv),
    ] {
        let mut direct = Vec::new();
        match write_results(&snapshot, &query, options, format, None, &mut direct) {
            Some(written) => written.unwrap(),
            None => {
                wrong.push(format!("{format:?}: not written directly"));
                continue;
            }
        }
        let fresh = evaluate_query(&snapshot, &query, options).unwrap();
        let expected_bytes = serialised(fresh, reference);
        if direct != expected_bytes {
            wrong.push(format!(
                "{format:?}: {:?}, the serialiser: {:?}",
                String::from_utf8_lossy(&direct),
                String::from_utf8_lossy(&expected_bytes)
            ));
        }
    }

    let explained = explain_query(&snapshot, &query, options).unwrap();
    if explained.rows != expected.len() as u64 {
        wrong.push(format!("explain: {} rows", explained.rows));
    }

    let planned = plan_query(&snapshot, &query, options).unwrap();
    if let Some(step) = planned.steps.iter().find(|s| s.detail.contains("?this")) {
        wrong.push(format!("plan: {} {}", step.operator, step.detail));
    }

    assert!(wrong.is_empty(), "{text}:\n{}", wrong.join("\n"));
}

#[test]
fn a_pre_bound_value_holds_in_every_delivery() {
    assert_every_delivery(
        &engine(),
        "SELECT ?s WHERE { ?s <urn:p> ?this }",
        &with_this(iri("urn:a").into()),
        &["<urn:s1>"],
    );
}

/// SHACL §5.6: pre-bound variables are substituted in subqueries too, projected there or
/// not.
#[test]
fn a_pre_bound_value_holds_in_a_subquery_in_every_delivery() {
    assert_every_delivery(
        &engine(),
        "SELECT ?s WHERE { { SELECT ?s WHERE { ?s <urn:p> ?this } } }",
        &with_this(iri("urn:a").into()),
        &["<urn:s1>"],
    );
}

/// A stored blank node pre-bound (a SHACL focus node can be one): that node, nothing else.
#[test]
fn a_pre_bound_blank_node_holds_in_every_delivery() {
    let engine = engine();
    let blank = stored_blank(&engine);
    assert_every_delivery(
        &engine,
        "SELECT ?s WHERE { ?s <urn:p> ?this }",
        &with_this(blank),
        &["<urn:s3>"],
    );
}

/// ASK in JSON is written directly too: `<urn:s2>` points to `<urn:b>`, not to the value.
#[test]
fn a_pre_bound_value_holds_in_a_directly_written_ask() {
    let engine = engine();
    let snapshot = engine.snapshot();
    let query = parse("ASK { <urn:s2> <urn:p> ?this }");
    let options = with_this(iri("urn:a").into());
    assert!(matches!(
        evaluate_query(&snapshot, &query, &options).unwrap(),
        QueryResults::Boolean(false)
    ));
    let mut direct = Vec::new();
    write_results(
        &snapshot,
        &query,
        &options,
        ResultsFormat::Json,
        None,
        &mut direct,
    )
    .expect("ASK in JSON is written directly")
    .unwrap();
    assert_eq!(
        direct,
        serialised(QueryResults::Boolean(false), QueryResultsFormat::Json)
    );
}
