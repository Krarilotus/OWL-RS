//! Vector similarity search through `SERVICE nrv:search` (native/vectors.rs): the
//! nearest, scores and ranks, candidates from the rest of the query, query vectors bound
//! outside the block, metrics, literals the data no longer uses, graphs a user may not
//! read, and the options' errors.

use std::sync::Arc;

use nrese_engine::vector::{DATATYPE, lexical};
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{GraphAccess, QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX nrv: <urn:nrese:vector:> PREFIX ex: <http://example.com/> ";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn vector(values: &[f32]) -> Literal {
    Literal::new_typed_literal(lexical(values), NamedNode::new_unchecked(DATATYPE))
}

/// Items on a circle, `item/i` at angle 10·i degrees, each with a kind (even or odd); one
/// in another graph, one whose vector the data no longer uses.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let embedding = ex("embedding");
    for i in 0..36 {
        let angle = (i as f32 * 10.0).to_radians();
        let item = ex(&format!("item/{i}"));
        tx.insert(
            Quad::new(
                item.clone(),
                embedding.clone(),
                vector(&[angle.cos(), angle.sin()]),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        let kind = if i % 2 == 0 { "even" } else { "odd" };
        tx.insert(Quad::new(item, ex("kind"), ex(kind), GraphName::DefaultGraph).as_ref());
    }
    // In another graph, right at 90 degrees.
    tx.insert(
        Quad::new(
            ex("secret"),
            embedding.clone(),
            vector(&[0.0, 1.0]),
            GraphName::from(ex("g")),
        )
        .as_ref(),
    );
    // Used once, at 91 degrees.
    let gone = Quad::new(
        ex("gone"),
        embedding,
        vector(&[91f32.to_radians().cos(), 91f32.to_radians().sin()]),
        GraphName::DefaultGraph,
    );
    tx.insert(gone.as_ref());
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    tx.remove(gone.as_ref());
    tx.commit().unwrap();
    engine
}

fn rows(engine: &Engine, query: &str, options: &QueryOptions) -> Vec<Vec<String>> {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let snapshot = engine.snapshot();
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, options).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|solution| {
            let solution = solution.unwrap();
            variables
                .iter()
                .map(|v| {
                    solution.get(v).map_or("-".to_owned(), |t| match t {
                        Term::NamedNode(n) => n.as_str().trim_start_matches(EX).to_owned(),
                        Term::Literal(l) => l.value().to_owned(),
                        other => other.to_string(),
                    })
                })
                .collect()
        })
        .collect()
}

fn error(engine: &Engine, query: &str) -> String {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let snapshot = engine.snapshot();
    match evaluate_query(&snapshot, &query, &QueryOptions::default()) {
        Ok(QueryResults::Solutions(mut solutions)) => match solutions.next() {
            Some(Err(error)) => error.to_string(),
            _ => panic!("an error expected"),
        },
        Ok(_) => panic!("an error expected"),
        Err(error) => error.to_string(),
    }
}

fn near(degrees: f32) -> String {
    let angle = degrees.to_radians();
    format!("\"{}\"^^nrv:vector", lexical(&[angle.cos(), angle.sin()]))
}

#[test]
fn the_nearest_items_are_found_and_ranked() {
    let engine = engine();
    let plain = QueryOptions::default();
    let found = rows(
        &engine,
        &format!(
            "SELECT ?item ?rank WHERE {{
               ?item ex:embedding ?v .
               SERVICE nrv:search {{ ?v nrv:near {} ; nrv:k 3 ; nrv:rank ?rank }}
             }} ORDER BY ?rank",
            near(92.0)
        ),
        &plain,
    );
    // 90 (item/9) is nearest; the unused literal at 91 and the other graph's at 90 are
    // not found.
    assert_eq!(
        found,
        [["item/9", "1"], ["item/10", "2"], ["item/8", "3"]].map(|r| r.map(str::to_owned).to_vec())
    );
    // The search alone, first in the group: the same vectors, with scores.
    let scored = rows(
        &engine,
        &format!(
            "SELECT ?v ?score WHERE {{
               SERVICE nrv:search {{ ?v nrv:near {} ; nrv:k 2 ; nrv:score ?score }}
             }} ORDER BY DESC(?score)",
            near(90.0)
        ),
        &plain,
    );
    assert_eq!(scored.len(), 2);
    let best: f64 = scored[0][1].parse().unwrap();
    assert!((best - 1.0).abs() < 1e-5, "{scored:?}");
    // Euclidean distances, nearest first.
    let distances = rows(
        &engine,
        &format!(
            "SELECT ?item ?d WHERE {{
               ?item ex:embedding ?v .
               SERVICE nrv:search {{ ?v nrv:near {} ; nrv:k 2 ; nrv:metric \"l2\" ; nrv:score ?d }}
             }} ORDER BY ?d",
            near(0.0)
        ),
        &plain,
    );
    assert_eq!(distances[0][0], "item/0");
    assert!(distances[0][1].parse::<f64>().unwrap() < 1e-5);
    // Exact, explicitly: the same answer.
    let exact = rows(
        &engine,
        &format!(
            "SELECT ?item WHERE {{
               ?item ex:embedding ?v .
               SERVICE nrv:search {{ ?v nrv:near {} ; nrv:k 1 ; nrv:exact true }}
             }}",
            near(92.0)
        ),
        &plain,
    );
    assert_eq!(exact, [vec!["item/9".to_owned()]]);
}

#[test]
fn the_rest_of_the_query_chooses_the_candidates() {
    let engine = engine();
    let plain = QueryOptions::default();
    // Only odd items: the nearest odd ones to 98 degrees (90, 110, 70).
    let odd = rows(
        &engine,
        &format!(
            "SELECT ?item WHERE {{
               ?item ex:kind ex:odd ; ex:embedding ?v .
               SERVICE nrv:search {{ ?v nrv:near {} ; nrv:k 3 ; nrv:rank ?r }}
             }} ORDER BY ?r",
            near(98.0)
        ),
        &plain,
    );
    assert_eq!(
        odd,
        [["item/9"], ["item/11"], ["item/7"]].map(|r| r.map(str::to_owned).to_vec())
    );
    // Items like item/0: the query vector bound outside the block, a search per value.
    let similar = rows(
        &engine,
        "SELECT ?query ?item WHERE {
           VALUES ?query { <http://example.com/item/0> <http://example.com/item/18> }
           ?query ex:embedding ?q .
           ?item ex:embedding ?v .
           SERVICE nrv:search { ?v nrv:near ?q ; nrv:k 2 ; nrv:rank ?r }
           FILTER(?item != ?query)
         } ORDER BY ?query ?r",
        &plain,
    );
    assert_eq!(similar.len(), 2, "{similar:?}");
    assert_eq!(similar[0], ["item/0", "item/1"].map(str::to_owned).to_vec());
    assert_eq!(similar[1][0], "item/18");
}

#[test]
fn a_user_finds_only_vectors_of_graphs_it_may_read() {
    let engine = engine();
    // Every graph merged: the other graph's vector at 90 degrees is found.
    let everything = QueryOptions {
        union_default_graph: true,
        ..QueryOptions::default()
    };
    let query = format!(
        "SELECT ?v WHERE {{ SERVICE nrv:search {{ ?v nrv:near {} ; nrv:k 1 }} }}",
        near(90.0)
    );
    let all = rows(&engine, &query, &everything);
    assert_eq!(all.len(), 1);
    // A user who may read the default graph only: the other graph's vector (at exactly
    // 90 degrees) isn't found, item/9's (at 90 degrees as computed) and the next are.
    let restricted = QueryOptions {
        union_default_graph: true,
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    let seen = rows(
        &engine,
        &format!(
            "SELECT ?item WHERE {{ ?item ex:embedding ?v . SERVICE nrv:search {{ ?v nrv:near {} ; nrv:k 2 }} }}",
            near(90.0)
        ),
        &restricted,
    );
    assert!(seen.iter().all(|row| row[0] != "secret"), "{seen:?}");
    assert_eq!(seen.len(), 2);
}

#[test]
fn malformed_searches_are_explained() {
    let engine = engine();
    for (block, expected) in [
        ("?v nrv:k 3", "nrv:near gives the query vector"),
        ("?v nrv:near \"[1, x]\"^^nrv:vector", "not a finite number"),
        (
            &format!("?v nrv:near {} ; nrv:metric \"manhattan\"", near(0.0)),
            "nrv:metric",
        ),
        (
            &format!("?v nrv:near {} ; nrv:bogus 1", near(0.0)),
            "unknown option nrv:bogus",
        ),
        (
            &format!("?v nrv:near {} ; nrv:k \"many\"", near(0.0)),
            "whole number",
        ),
        ("?v nrv:near ?q", "has no value"),
    ] {
        let message = error(
            &engine,
            &format!("SELECT * WHERE {{ SERVICE nrv:search {{ {block} }} }}"),
        );
        assert!(message.contains(expected), "{block}: {message}");
    }
}
