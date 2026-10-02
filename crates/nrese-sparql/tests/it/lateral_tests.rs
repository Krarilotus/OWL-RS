//! `LATERAL` (SEP-0006, native/lateral.rs): the right side evaluated for each solution of
//! the left, so a LIMIT, ORDER BY or aggregate inside it applies per left solution. As in
//! SPARQL's scoping, a subquery sees a left variable only where it projects it.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX ex: <http://example.com/> ";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// Three persons: ann knows bob, cid and dan; bob knows ann; cid knows no one.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let a = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let mut insert = |s: &str, p: NamedNode, o: Term| {
        tx.insert(Quad::new(ex(s), p, o, GraphName::DefaultGraph).as_ref());
    };
    for person in ["ann", "bob", "cid"] {
        insert(person, a.clone(), ex("Person").into());
    }
    for (s, o) in [
        ("ann", "dan"),
        ("ann", "bob"),
        ("ann", "cid"),
        ("bob", "ann"),
    ] {
        insert(s, ex("knows"), ex(o).into());
    }
    insert("dan", ex("age"), Literal::from(40).into());
    tx.commit().unwrap();
    engine
}

fn rows(engine: &Engine, query: &str) -> Vec<Vec<String>> {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    assert_eq!(
        explain_query(&snapshot, &query, &options).unwrap().executor,
        "native"
    );
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, &options).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    let mut out: Vec<Vec<String>> = solutions
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
        .collect();
    out.sort();
    out
}

#[test]
fn the_right_side_runs_per_left_solution() {
    let engine = engine();
    // The first acquaintance of each person, in name order: a LIMIT per person.
    assert_eq!(
        rows(
            &engine,
            "SELECT ?p ?f WHERE { ?p a ex:Person \
             LATERAL { SELECT ?p ?f WHERE { ?p ex:knows ?f } ORDER BY ?f LIMIT 1 } }"
        ),
        [["ann", "bob"], ["bob", "ann"]]
    );
    // Not projected, the subquery doesn't see ?p: the first acquaintance of anyone.
    assert_eq!(
        rows(
            &engine,
            "SELECT ?p ?f WHERE { ?p a ex:Person \
             LATERAL { SELECT ?f WHERE { ?p ex:knows ?f } ORDER BY ?f LIMIT 1 } }"
        ),
        [["ann", "ann"], ["bob", "ann"], ["cid", "ann"]]
    );
    // An aggregate per person; a group key put in stays a key, so cid has no group.
    assert_eq!(
        rows(
            &engine,
            "SELECT ?p ?n WHERE { ?p a ex:Person \
             LATERAL { SELECT ?p (COUNT(*) AS ?n) WHERE { ?p ex:knows ?f } GROUP BY ?p } }"
        ),
        [["ann", "3"], ["bob", "1"]]
    );
    // An OPTIONAL inside: the left solutions stay, with what the right side adds.
    assert_eq!(
        rows(
            &engine,
            "SELECT ?p ?age WHERE { ?p a ex:Person \
             LATERAL { OPTIONAL { ?p ex:knows ?f . ?f ex:age ?age } } }"
        ),
        [["ann", "40"], ["bob", "-"], ["cid", "-"]]
    );
    // Nothing shared: a cross product.
    assert_eq!(
        rows(
            &engine,
            "SELECT ?p ?x WHERE { ?p ex:knows ex:ann LATERAL { ?x ex:age 40 } }"
        ),
        [["bob", "dan"]]
    );
}
