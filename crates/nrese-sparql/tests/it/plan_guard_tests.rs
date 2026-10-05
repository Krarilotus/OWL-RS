//! Guards for the query engine's measured wins (docs/design/performance.md §0): each
//! query below must still run with the operator that made it fast, read from EXPLAIN.
//! A planner change that loses one fails here at once, not in a benchmark run.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{QueryOptions, explain_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.org/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// A store of 3,000 nodes: a cyclic `knows` graph (each node knows three others), a
/// `label` per node and a `kind` among ten.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..3000u32 {
        let node = ex(&format!("n{i}"));
        for step in [1, 7, 31] {
            tx.insert(
                Quad::new(
                    node.clone(),
                    ex("knows"),
                    ex(&format!("n{}", (i + step) % 3000)),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.insert(
            Quad::new(
                node.clone(),
                ex("label"),
                Literal::new_simple_literal(format!("label number {i}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        tx.insert(
            Quad::new(
                node,
                ex("kind"),
                ex(&format!("k{}", i % 10)),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    engine
}

/// The operators `query` ran with.
fn operators(engine: &Engine, query: &str) -> Vec<String> {
    let text = format!("PREFIX e: <{EX}> {query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    explain_query(&engine.snapshot(), &query, &QueryOptions::default())
        .unwrap()
        .steps
        .into_iter()
        .map(|s| s.operator)
        .collect()
}

fn assert_uses(engine: &Engine, query: &str, operator: &str) {
    let used = operators(engine, query);
    assert!(
        used.iter().any(|o| o == operator),
        "{query}\nno {operator:?} among {used:?}"
    );
}

/// Group counts on the index: COUNT … GROUP BY one variable of one pattern reads run
/// lengths of the permutation sorted on it (Wikidata q07: 2,101 → 0.18 ms).
#[test]
fn group_counts_walk_the_index() {
    assert_uses(
        &engine(),
        "SELECT ?k (COUNT(*) AS ?n) WHERE { ?s e:kind ?k } GROUP BY ?k",
        "group count",
    );
}

/// The distinct values of a variable by a group walk where a set suffices, as for the
/// side of a NOT EXISTS (DBpedia q13, people without a death date: 25 → 3.1 ms).
#[test]
fn distinct_values_walk_the_index() {
    assert_uses(
        &engine(),
        "SELECT (COUNT(*) AS ?n) WHERE { ?s e:kind ?k FILTER NOT EXISTS { ?s e:label ?l } }",
        "group walk",
    );
}

/// Worst-case-optimal joins for cyclic patterns (LUBM 100 q2: 18 → 4.8 ms).
#[test]
fn triangles_join_worst_case_optimally() {
    assert_uses(
        &engine(),
        "SELECT * WHERE { ?a e:knows ?b . ?b e:knows ?c . ?c e:knows ?a }",
        "wcoj",
    );
}

/// Eager aggregation: a group over a join aggregates the side its aggregates read before
/// the join, `AVG`, `MIN` and `MAX` included (BSBM BI q4's shape, an average of offer
/// prices per product feature).
#[test]
fn averages_over_joins_aggregate_before_joining() {
    let text = format!(
        "PREFIX e: <{EX}> SELECT ?k (AVG(STRLEN(?l)) AS ?mean) (MAX(?l) AS ?last) WHERE {{ ?s e:kind ?k . ?s e:knows ?o . ?o e:label ?l }} GROUP BY ?k"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let explained = explain_query(&engine().snapshot(), &query, &QueryOptions::default()).unwrap();
    assert!(
        explained.rewrites.contains(&"eager-aggregation"),
        "{:?}",
        explained.rewrites
    );
    assert_eq!(explained.rows, 10);
}

/// String filters on the dictionary: CONTAINS on a large pattern's object runs once per
/// distinct term, and the passing ids semi-join the scan.
#[test]
fn string_filters_test_the_dictionary() {
    assert_uses(
        &engine(),
        "SELECT ?s WHERE { ?s e:label ?l FILTER(CONTAINS(?l, \"number 12\")) }",
        "dictionary string test",
    );
}
