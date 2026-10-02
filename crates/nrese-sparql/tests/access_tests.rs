//! Graph-level access control in queries and updates (`QueryOptions::access`): a user's
//! dataset is restricted to the graphs it may read, before anything is evaluated; the
//! others are absent.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{
    GraphAccess, QueryOptions, QueryResults, UpdateOptions, apply_update, evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let quad = |s: &str, graph: GraphName| {
        Quad::new(
            NamedNode::new_unchecked(format!("{EX}{s}")),
            NamedNode::new_unchecked(format!("{EX}p")),
            Literal::new_simple_literal(s),
            graph,
        )
    };
    let graph =
        |name: &str| GraphName::NamedNode(NamedNode::new_unchecked(format!("{EX}g/{name}")));
    for (s, g) in [
        ("d1", GraphName::DefaultGraph),
        ("d2", GraphName::DefaultGraph),
        ("p1", graph("pub/1")),
        ("p2", graph("pub/2")),
        ("p3", graph("pub/2")),
        ("s1", graph("secret")),
    ] {
        tx.insert(quad(s, g).as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn count(engine: &Engine, query: &str, options: &QueryOptions) -> usize {
    let query = SparqlParser::new().parse_query(query).unwrap();
    match evaluate_query(&engine.snapshot(), &query, options).unwrap() {
        QueryResults::Solutions(solutions) => solutions.count(),
        QueryResults::Boolean(answer) => usize::from(answer),
        QueryResults::Graph(triples) => triples.count(),
    }
}

fn with(access: GraphAccess, union_default_graph: bool) -> QueryOptions {
    QueryOptions {
        access: Some(Arc::new(access)),
        union_default_graph,
        ..QueryOptions::default()
    }
}

#[test]
fn queries_read_only_the_graphs_they_may() {
    let engine = engine();
    let public = GraphAccess {
        prefixes: vec![format!("{EX}g/pub/")],
        ..GraphAccess::default()
    };
    let all = "SELECT * WHERE { ?s ?p ?o }";
    let named = "SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }";
    // Unrestricted: the store's default graph, every named graph.
    assert_eq!(count(&engine, all, &QueryOptions::default()), 2);
    assert_eq!(count(&engine, named, &QueryOptions::default()), 4);
    // The public graphs only: no default graph, three named statements, two graphs.
    assert_eq!(count(&engine, all, &with(public.clone(), false)), 0);
    assert_eq!(count(&engine, named, &with(public.clone(), false)), 3);
    assert_eq!(
        count(
            &engine,
            "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }",
            &with(public.clone(), false)
        ),
        2
    );
    // A union default graph merges the readable graphs only.
    assert_eq!(count(&engine, all, &with(public.clone(), true)), 3);
    // FROM and GRAPH a forbidden graph: as if it were empty.
    let secret = format!("<{EX}g/secret>");
    assert_eq!(
        count(
            &engine,
            &format!("SELECT * FROM {secret} WHERE {{ ?s ?p ?o }}"),
            &with(public.clone(), false)
        ),
        0
    );
    assert_eq!(
        count(
            &engine,
            &format!("SELECT * WHERE {{ GRAPH {secret} {{ ?s ?p ?o }} }}"),
            &with(public.clone(), false)
        ),
        0
    );
    assert_eq!(
        count(
            &engine,
            &format!("ASK {{ GRAPH {secret} {{ ?s ?p ?o }} }}"),
            &with(public.clone(), false)
        ),
        0
    );
    assert_eq!(
        count(
            &engine,
            &format!("SELECT * FROM <{EX}g/pub/2> WHERE {{ ?s ?p ?o }}"),
            &with(public.clone(), false)
        ),
        2
    );
    // The default graph when it is readable; an exact graph IRI.
    let mixed = GraphAccess {
        graphs: vec![format!("{EX}g/secret")],
        default_graph: true,
        ..GraphAccess::default()
    };
    assert_eq!(count(&engine, all, &with(mixed.clone(), false)), 2);
    assert_eq!(count(&engine, named, &with(mixed.clone(), false)), 1);
    assert_eq!(count(&engine, all, &with(mixed, true)), 3);
}

#[test]
fn update_where_clauses_read_only_the_graphs_they_may() {
    let engine = engine();
    let public = GraphAccess {
        prefixes: vec![format!("{EX}g/pub/")],
        ..GraphAccess::default()
    };
    // Copying everything into a graph reads only what the user may.
    let update = SparqlParser::new()
        .parse_update(&format!(
            "INSERT {{ GRAPH <{EX}g/pub/copy> {{ ?s ?p ?o }} }} WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}"
        ))
        .unwrap();
    let mut tx = engine.transaction();
    apply_update(
        &mut tx,
        &update,
        &UpdateOptions {
            access: Some(Arc::new(public)),
            ..UpdateOptions::default()
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        count(
            &engine,
            &format!("SELECT * WHERE {{ GRAPH <{EX}g/pub/copy> {{ ?s ?p ?o }} }}"),
            &QueryOptions::default()
        ),
        3
    );
}
