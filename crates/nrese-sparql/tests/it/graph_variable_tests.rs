//! Inside `GRAPH ?g { P }`, `P` is evaluated in each named graph with `?g` free, and only
//! then joined with `?g` bound to that graph's name (SPARQL 1.1 §18.6, the evaluation of
//! `Graph`). A part of `P` that binds `?g` from data, or reads it, sees it free: an
//! OPTIONAL that binds `?g` to a term other than the graph's name extends the row there,
//! and the join with the graph's name then removes it. Each case states the answer the
//! specification's algebra gives, and the reference evaluator is held to it as well.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::{GraphAccess, QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_reference::Dataset;
use nrese_sparql_syntax::SparqlParser;

/// `<urn:g>`: `:s :p :o` and `:s :q :other`.
const ONE_GRAPH: &str = "\
<urn:s> <urn:p> <urn:o> <urn:g> .
<urn:s> <urn:q> <urn:other> <urn:g> .
";

/// [`ONE_GRAPH`], and `<urn:h>`: `:s :p :o` and `:s :q :h`, a `:q` naming its own graph.
const TWO_GRAPHS: &str = "\
<urn:s> <urn:p> <urn:o> <urn:g> .
<urn:s> <urn:q> <urn:other> <urn:g> .
<urn:s> <urn:p> <urn:o> <urn:h> .
<urn:s> <urn:q> <urn:h> <urn:h> .
";

/// A `:q` statement whose subject is another graph's name in `<urn:g>`, its own in `<urn:h>`.
const SUBJECTS: &str = "\
<urn:s> <urn:p> <urn:o> <urn:g> .
<urn:other> <urn:q> <urn:z> <urn:g> .
<urn:s> <urn:p> <urn:o> <urn:h> .
<urn:h> <urn:q> <urn:z> <urn:h> .
";

/// A predicate that is another graph's name in `<urn:g>`, its own in `<urn:h>`.
const PREDICATES: &str = "\
<urn:s> <urn:p> <urn:o> <urn:g> .
<urn:s> <urn:other> <urn:z> <urn:g> .
<urn:s> <urn:p> <urn:o> <urn:h> .
<urn:s> <urn:h> <urn:z> <urn:h> .
";

const OPTIONAL_OBJECT: &str =
    "SELECT ?s ?g WHERE { GRAPH ?g { ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?g } } }";

struct Case {
    name: &'static str,
    data: &'static str,
    query: &'static str,
    /// The graphs a restricted reader may read (an IRI prefix); `None`: every graph.
    access: Option<&'static str>,
    /// The specification's answer: one row a string of `?variable=term`, sorted.
    expected: &'static [&'static str],
}

const CASES: &[Case] = &[
    // `<urn:g>`: the OPTIONAL binds `?g` to `:other`; the join with `<urn:g>` removes the row.
    Case {
        name: "the OPTIONAL binds the graph variable as its object",
        data: ONE_GRAPH,
        query: OPTIONAL_OBJECT,
        access: None,
        expected: &[],
    },
    // `<urn:h>`'s OPTIONAL binds `?g` to `<urn:h>` itself: that row stays.
    Case {
        name: "the same, with a graph whose statement names itself",
        data: TWO_GRAPHS,
        query: OPTIONAL_OBJECT,
        access: None,
        expected: &["?g=<urn:h> ?s=<urn:s>"],
    },
    Case {
        name: "the OPTIONAL binds the graph variable as its subject",
        data: SUBJECTS,
        query: "SELECT ?s ?g ?z WHERE { GRAPH ?g { ?s <urn:p> <urn:o> OPTIONAL { ?g <urn:q> ?z } } }",
        access: None,
        expected: &["?g=<urn:h> ?s=<urn:s> ?z=<urn:z>"],
    },
    // In `<urn:g>` both statements bind `?g` (`:p`, `:other`), neither to `<urn:g>`.
    Case {
        name: "the OPTIONAL binds the graph variable as its predicate",
        data: PREDICATES,
        query: "SELECT ?s ?g ?z WHERE { GRAPH ?g { ?s <urn:p> <urn:o> OPTIONAL { ?s ?g ?z } } }",
        access: None,
        expected: &["?g=<urn:h> ?s=<urn:s> ?z=<urn:z>"],
    },
    // The OPTIONAL's condition reads `?g` free: an error, so no row extends (§18.5,
    // LeftJoin), and each graph keeps its left row.
    Case {
        name: "the OPTIONAL's FILTER alone reads the graph variable",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g ?x WHERE { GRAPH ?g { ?s <urn:p> <urn:o> \
                OPTIONAL { ?s <urn:q> ?x FILTER(?g = <urn:g>) } } }",
        access: None,
        expected: &["?g=<urn:g> ?s=<urn:s>", "?g=<urn:h> ?s=<urn:s>"],
    },
    Case {
        name: "a nested OPTIONAL binds the graph variable",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g ?y WHERE { GRAPH ?g { ?s <urn:p> <urn:o> \
                OPTIONAL { ?s <urn:p> ?y OPTIONAL { ?s <urn:q> ?g } } } }",
        access: None,
        expected: &["?g=<urn:h> ?s=<urn:s> ?y=<urn:o>"],
    },
    // A UNION keeps both branches' rows: the second branch's `:other` row goes, `<urn:h>`'s
    // stays beside its first branch's.
    Case {
        name: "a UNION branch binds the graph variable",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g WHERE { GRAPH ?g { { ?s <urn:p> <urn:o> } UNION { ?s <urn:q> ?g } } }",
        access: None,
        expected: &[
            "?g=<urn:g> ?s=<urn:s>",
            "?g=<urn:h> ?s=<urn:s>",
            "?g=<urn:h> ?s=<urn:s>",
        ],
    },
    // One branch binds `?g`; in the other it is free, so `!BOUND(?g)` holds there.
    Case {
        name: "a UNION branch binds the graph variable and the other tests it",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g ?x WHERE { GRAPH ?g { { ?s <urn:q> ?g } \
                UNION { ?s <urn:p> ?x FILTER(!BOUND(?g)) } } }",
        access: None,
        expected: &[
            "?g=<urn:g> ?s=<urn:s> ?x=<urn:o>",
            "?g=<urn:h> ?s=<urn:s>",
            "?g=<urn:h> ?s=<urn:s> ?x=<urn:o>",
        ],
    },
    // A required pattern binding `?g` is a join with it: `<urn:g>`'s `:other` row goes.
    Case {
        name: "a required pattern binds the graph variable",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g WHERE { GRAPH ?g { ?s <urn:p> <urn:o> . ?s <urn:q> ?g } }",
        access: None,
        expected: &["?g=<urn:h> ?s=<urn:s>"],
    },
    Case {
        name: "an OPTIONAL that does not mention the graph variable",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g ?x WHERE { GRAPH ?g { ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?x } } }",
        access: None,
        expected: &[
            "?g=<urn:g> ?s=<urn:s> ?x=<urn:other>",
            "?g=<urn:h> ?s=<urn:s> ?x=<urn:h>",
        ],
    },
    // A constant graph: `?g` is an ordinary variable, bound by the OPTIONAL.
    Case {
        name: "a constant graph with a variable in the same place",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g WHERE { GRAPH <urn:g> { ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?g } } }",
        access: None,
        expected: &["?g=<urn:other> ?s=<urn:s>"],
    },
    Case {
        name: "FROM NAMED gives the graph whose OPTIONAL binds another term",
        data: TWO_GRAPHS,
        query: "SELECT ?s ?g FROM NAMED <urn:g> WHERE { GRAPH ?g { ?s <urn:p> <urn:o> \
                OPTIONAL { ?s <urn:q> ?g } } }",
        access: None,
        expected: &[],
    },
    Case {
        name: "a reader restricted to the graph whose OPTIONAL binds another term",
        data: TWO_GRAPHS,
        query: OPTIONAL_OBJECT,
        access: Some("urn:g"),
        expected: &[],
    },
];

fn quads(data: &str) -> Vec<Quad> {
    RdfParser::from_format(RdfFormat::NQuads)
        .for_reader(data.as_bytes())
        .map(|quad| quad.unwrap())
        .collect()
}

fn rows(results: QueryResults<'_>) -> Vec<String> {
    let QueryResults::Solutions(solutions) = results else {
        panic!("a SELECT gives solutions");
    };
    let mut rows: Vec<String> = solutions
        .map(|solution| {
            let solution = solution.unwrap();
            let mut row: Vec<String> = solution
                .iter()
                .map(|(variable, term): (_, &Term)| format!("{variable}={term}"))
                .collect();
            row.sort();
            row.join(" ")
        })
        .collect();
    rows.sort();
    rows
}

/// The engine's answer, and the reference evaluator's over the graphs the reader may read.
fn answers(case: &Case) -> (Vec<String>, Vec<String>) {
    let data = quads(case.data);
    let readable = |quad: &&Quad| match (&quad.graph_name, case.access) {
        (_, None) => true,
        (GraphName::NamedNode(graph), Some(prefix)) => graph.as_str().starts_with(prefix),
        _ => false,
    };
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for quad in &data {
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    let mut reference = Dataset::default();
    for quad in data.iter().filter(readable) {
        reference.insert(quad.clone());
    }
    let options = QueryOptions {
        access: case.access.map(|prefix| {
            Arc::new(GraphAccess {
                prefixes: vec![prefix.to_owned()],
                ..GraphAccess::default()
            })
        }),
        ..QueryOptions::default()
    };
    let query = SparqlParser::new().parse_query(case.query).unwrap();
    let ours = rows(evaluate_query(&engine.snapshot(), &query, &options).unwrap());
    let theirs = rows(reference.query(&query, &QueryOptions::default()).unwrap());
    (ours, theirs)
}

/// Spec: SPARQL 1.1 §18.6, `eval(D(G), Graph(var, P))`: `P` in each graph with `var` free,
/// then the join with `var` bound to the graph's name.
#[test]
fn a_pattern_under_graph_sees_the_graph_variable_free() {
    let mut wrong = Vec::new();
    for case in CASES {
        let (ours, reference) = answers(case);
        let expected: Vec<String> = case.expected.iter().map(|row| (*row).to_owned()).collect();
        if ours != expected {
            wrong.push(format!(
                "{}: the engine gives {ours:?}, the specification {expected:?}",
                case.name
            ));
        }
        if reference != expected {
            wrong.push(format!(
                "{}: the reference evaluator gives {reference:?}, the specification {expected:?}",
                case.name
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
