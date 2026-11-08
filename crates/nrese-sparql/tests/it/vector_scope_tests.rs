//! Vector selection under `SearchScope::Reader` (native/vectors.rs,
//! `Snapshot::vector_search_in`): the nearest vectors come from the vectors the reader's
//! scans show alone, compared exactly, so a graph the reader may not read changes no
//! selection, however the index's shared graph was built over it. The oracle is an
//! independent store holding the visible vectors only, searched exactly.

use std::sync::Arc;

use nrese_engine::vector::{DATATYPE, lexical};
use nrese_engine::{Engine, EngineConfig, VectorQuery};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{GraphAccess, QueryOptions, QueryResults, SearchScope, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX nrv: <urn:nrese:vector:> PREFIX ex: <http://example.com/> ";
const DIMENSION: usize = 16;

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// A fixed pseudo-random sequence (a 64-bit LCG): the same vectors on every run.
struct Lcg(u64);

impl Lcg {
    fn vector(&mut self) -> Vec<f32> {
        (0..DIMENSION)
            .map(|_| {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
            })
            .collect()
    }
}

fn literal(values: &[f32]) -> Literal {
    Literal::new_typed_literal(lexical(values), NamedNode::new_unchecked(DATATYPE))
}

/// `visible` items in the default graph; with `hidden`, as many again times three in graph
/// `G`, interleaved with them in the dictionary.
fn store(visible: &[Vec<f32>], hidden: &[Vec<f32>]) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let embedding = ex("embedding");
    let mut hidden = hidden.iter().enumerate();
    for (i, values) in visible.iter().enumerate() {
        tx.insert(
            Quad::new(
                ex(&format!("item/{i}")),
                embedding.clone(),
                literal(values),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        for (j, values) in hidden.by_ref().take(3) {
            tx.insert(
                Quad::new(
                    ex(&format!("secret/{j}")),
                    embedding.clone(),
                    literal(values),
                    GraphName::from(ex("G")),
                )
                .as_ref(),
            );
        }
    }
    tx.commit().unwrap();
    engine
}

fn items(engine: &Engine, query: &str, options: &QueryOptions) -> Vec<String> {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let snapshot = engine.snapshot();
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, options).unwrap()
    else {
        panic!("solutions")
    };
    solutions
        .map(|solution| match solution.unwrap().get("item") {
            Some(Term::NamedNode(n)) => n.as_str().trim_start_matches(EX).to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// For each of 24 query vectors, a reader of the default graph gets the ten nearest
/// visible items an exact search of a store without `G` finds, in that order; with the
/// shared graph built over every vector and a narrow beam asked for. A guard: the global
/// path's filtered graph search found the same ten on the parent tree for these vectors
/// (red proof `syn24-red-1-7535a82c`); reader mode is exact by construction, and its
/// report's counts are checked in nrese-engine's `text_scope_tests`.
#[test]
fn an_unreadable_graph_changes_no_vector_selection() {
    let mut random = Lcg(7);
    let visible: Vec<Vec<f32>> = (0..1500).map(|_| random.vector()).collect();
    let hidden: Vec<Vec<f32>> = (0..4500).map(|_| random.vector()).collect();
    let queries: Vec<Vec<f32>> = (0..24).map(|_| random.vector()).collect();
    let with_g = store(&visible, &hidden);
    let without_g = store(&visible, &[]);
    with_g
        .snapshot()
        .prepare_vector_graph(&VectorQuery::new(queries[0].clone(), 10));
    let reader = QueryOptions {
        search_scope: SearchScope::Reader,
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    let search = |values: &[f32], exact: bool| {
        format!(
            "SELECT ?item WHERE {{
               ?item ex:embedding ?v .
               SERVICE nrv:search {{ ?v nrv:near \"{}\"^^nrv:vector ; nrv:k 10 ; nrv:rank ?r ;
                                     nrv:searchBudget 10 ; nrv:exact {exact} }}
             }} ORDER BY ?r",
            lexical(values)
        )
    };
    let mut failed = Vec::new();
    for (i, query) in queries.iter().enumerate() {
        let want = items(&without_g, &search(query, true), &QueryOptions::default());
        assert_eq!(want.len(), 10, "oracle");
        let got = items(&with_g, &search(query, false), &reader);
        if got != want {
            failed.push(format!("query {i}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of 24 failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
