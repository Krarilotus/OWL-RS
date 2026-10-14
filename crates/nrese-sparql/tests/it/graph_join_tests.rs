//! `GRAPH <g> { … }` blocks joined to each other: each is estimated by its patterns' counts
//! in its own graph, the smaller goes first and the other is evaluated from its rows (index
//! probes in that graph). Found on Geographica 2 (14 October 2026): the estimates counted
//! the patterns in the default graph, 0 for data in named graphs, so the blocks ran in the
//! written order, the larger one read whole.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// `big` statements `ex:s<i> ex:p i` in `ex:big`, and `ex:q` of two of those subjects in
/// `ex:small`.
fn engine(big: usize) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..big {
        tx.insert(
            Quad::new(
                ex(&format!("s{i}")),
                ex("p"),
                Literal::from(i as i64),
                GraphName::NamedNode(ex("big")),
            )
            .as_ref(),
        );
    }
    for i in [3, 7] {
        tx.insert(
            Quad::new(
                ex(&format!("s{i}")),
                ex("q"),
                Literal::new_simple_literal("x"),
                GraphName::NamedNode(ex("small")),
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    engine
}

#[test]
fn the_smaller_graph_block_goes_first_and_the_other_is_probed() {
    let engine = engine(5000);
    let snapshot = engine.snapshot();
    let text = format!(
        "PREFIX ex: <{EX}> SELECT ?s ?o WHERE {{ GRAPH ex:big {{ ?s ex:p ?o }} \
         GRAPH ex:small {{ ?s ex:q ?x }} }}"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let QueryResults::Solutions(solutions) =
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    else {
        panic!()
    };
    assert_eq!(solutions.count(), 2);
    let plan = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    // No step reads the 5,000 statements of the larger block.
    let largest = plan.steps.iter().map(|s| s.rows).max().unwrap();
    assert!(largest <= 2, "{:#?}", plan.steps);
}
