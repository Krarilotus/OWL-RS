//! A search under `GRAPH ?g` is evaluated per named graph, as SPARQL 1.1 §18.6 evaluates a
//! `GRAPH ?g { P }` pattern: P in each named graph of the dataset, joined with `?g` bound to
//! that graph. So `?g` is bound on every row, a literal in several graphs gives one row per
//! graph, and `bds:rank`, `bds:minRank` and `bds:maxRank` count within each graph.
//!
//! The fixture: four labels whose relevance for `alpha` is strictly ordered by length (one
//! occurrence each, BM25): `alpha` > `alpha beta` > `alpha beta gamma delta` >
//! `alpha gamma delta epsilon zeta`. Graph g1 holds the first two, g2 the second and third,
//! the default graph the last. Store-wide, `alpha beta` ranks 2; within g2 it ranks 1.

use std::sync::Arc;

use nrese_engine::vector::{DATATYPE, lexical};
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{GraphAccess, QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                        PREFIX text: <http://jena.apache.org/text#> \
                        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                        PREFIX nrv: <urn:nrese:vector:> \
                        PREFIX ex: <http://example.com/> ";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let label = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label");
    for (subject, text, graph) in [
        ("s1", "alpha", Some("g1")),
        ("s2", "alpha beta", Some("g1")),
        ("s2b", "alpha beta", Some("g2")),
        ("s4", "alpha beta gamma delta", Some("g2")),
        ("s3", "alpha gamma delta epsilon zeta", None),
    ] {
        let graph: GraphName = match graph {
            Some(g) => ex(g).into(),
            None => GraphName::DefaultGraph,
        };
        let text = Term::from(Literal::new_simple_literal(text));
        tx.insert(Quad::new(ex(subject), label.clone(), text, graph).as_ref());
    }
    // One vector per named graph, for the vector search's row.
    for (item, values, graph) in [("v1", [1.0, 0.0], "g1"), ("v2", [0.0, 1.0], "g2")] {
        let vector =
            Literal::new_typed_literal(lexical(&values), NamedNode::new_unchecked(DATATYPE));
        tx.insert(Quad::new(ex(item), ex("embedding"), vector, ex(graph)).as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The rows of `query`, each its variables' values (IRIs without the prefix, a literal's
/// value, `-` for unbound), sorted.
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
    let mut rows: Vec<Vec<String>> = solutions
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
    rows.sort();
    rows
}

fn table(rows: &[&[&str]]) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|v| (*v).to_owned()).collect())
        .collect();
    rows.sort();
    rows
}

/// The seven cases of `notes/2026-11-07-graph-var-search-binding-sizing.md`, one row each
/// (some with two queries), every row reported.
#[test]
fn a_search_under_graph_variable_is_evaluated_per_named_graph() {
    let engine = engine();
    let plain = QueryOptions::default();
    let mut failed = Vec::new();
    let mut row = |name: &str, got: Vec<Vec<String>>, want: Vec<Vec<String>>| {
        if got != want {
            failed.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    };
    // Projected `?g ?o`, then the relevance or rank variable `extra` binds, if any.
    let search = |extra: &str, from: &str| {
        let bound = ["?s", "?r"].into_iter().filter(|v| extra.contains(v));
        let vars: Vec<&str> = ["?g", "?o"].into_iter().chain(bound).collect();
        format!(
            "SELECT {} {from} WHERE {{ GRAPH ?g {{ ?o bds:search \"alpha\" {extra} }} }}",
            vars.join(" ")
        )
    };
    // 1, 2, 3: `?g` bound; a literal in two graphs gives a row per graph; one only in the
    // default graph gives none.
    row(
        "1-3 rows per graph",
        rows(&engine, &search("", ""), &plain),
        table(&[
            &["g1", "alpha"],
            &["g1", "alpha beta"],
            &["g2", "alpha beta"],
            &["g2", "alpha beta gamma delta"],
        ]),
    );
    // 2: a row in each graph, at the same relevance (the index's).
    let scored = rows(&engine, &search("; bds:relevance ?s", ""), &plain);
    let of = |graph: &str| {
        scored
            .iter()
            .find(|r| r[0] == graph && r[1] == "alpha beta")
            .map(|r| r[2].clone())
    };
    let (in_g1, in_g2) = (of("g1"), of("g2"));
    let same = in_g1.is_some() && in_g1 == in_g2;
    row(
        "2 relevance per graph",
        vec![vec![format!("{in_g1:?} {in_g2:?} same {same}")]],
        vec![vec![format!("{in_g1:?} {in_g1:?} same true")]],
    );
    // 4: `FROM NAMED` and a restricted reader: only their graphs.
    row(
        "4 FROM NAMED g2",
        rows(
            &engine,
            &search("", "FROM NAMED <http://example.com/g2>"),
            &plain,
        ),
        table(&[&["g2", "alpha beta"], &["g2", "alpha beta gamma delta"]]),
    );
    let reader_of_g1 = QueryOptions {
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            graphs: vec![format!("{EX}g1")],
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    row(
        "4 reader of g1",
        rows(&engine, &search("", ""), &reader_of_g1),
        table(&[&["g1", "alpha"], &["g1", "alpha beta"]]),
    );
    // 5: rank, minRank and maxRank within each graph (store-wide, `alpha beta` is 2nd).
    row(
        "5 rank per graph",
        rows(&engine, &search("; bds:rank ?r", ""), &plain),
        table(&[
            &["g1", "alpha", "1"],
            &["g1", "alpha beta", "2"],
            &["g2", "alpha beta", "1"],
            &["g2", "alpha beta gamma delta", "2"],
        ]),
    );
    row(
        "5 maxRank 1 per graph",
        rows(&engine, &search("; bds:maxRank 1", ""), &plain),
        table(&[&["g1", "alpha"], &["g2", "alpha beta"]]),
    );
    row(
        "5 minRank 2 per graph",
        rows(&engine, &search("; bds:minRank 2", ""), &plain),
        table(&[&["g1", "alpha beta"], &["g2", "alpha beta gamma delta"]]),
    );
    // 6: Jena's vocabulary joins the literal's subject in the same graph (guard: its label
    // pattern is a scan that binds `?g`).
    row(
        "6 text:query joins per graph",
        rows(
            &engine,
            "SELECT ?s ?g WHERE { GRAPH ?g { (?s ?score) text:query (rdfs:label \"alpha\") } }",
            &plain,
        ),
        table(&[&["s1", "g1"], &["s2", "g1"], &["s2b", "g2"], &["s4", "g2"]]),
    );
    // 7: a vector search under `GRAPH ?g` binds it too (guard: it runs graph by graph).
    row(
        "7 vector search per graph",
        rows(
            &engine,
            &format!(
                "SELECT ?item ?g WHERE {{ GRAPH ?g {{ ?item ex:embedding ?v .
                   SERVICE nrv:search {{ ?v nrv:near \"{}\"^^nrv:vector ; nrv:k 1 }} }} }}",
                lexical(&[1.0, 0.0])
            ),
            &plain,
        ),
        table(&[&["v1", "g1"], &["v2", "g2"]]),
    );
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
