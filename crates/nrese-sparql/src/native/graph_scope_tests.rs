//! Which patterns under `GRAPH ?g` are evaluated once for every graph (`scans_everywhere`):
//! those whose answer is the same as evaluating them graph by graph with `?g` free
//! (SPARQL 1.1 §18.6). The answers themselves are held in `tests/it/graph_variable_tests.rs`.

use super::*;

/// `text`'s `GRAPH ?g { … }` pattern and its variable.
fn under_graph(text: &str) -> (GraphPattern, Variable) {
    let query = Query::parse(text, None).unwrap_or_else(|e| panic!("{e}: {text}"));
    let Query::Select { pattern, .. } = query else {
        panic!("a SELECT: {text}");
    };
    let GraphPattern::Project { inner, .. } = pattern else {
        panic!("a projection: {text}");
    };
    let GraphPattern::Graph {
        name: NamedNodePattern::Variable(variable),
        inner,
    } = *inner
    else {
        panic!("GRAPH ?g at the top: {text}");
    };
    (*inner, variable)
}

/// Spec: SPARQL 1.1 §18.6. An OPTIONAL that never names `?g` gives the same rows either
/// way, so it keeps the single evaluation; one that binds `?g` must see it free, which only
/// the graph-by-graph evaluation gives.
#[test]
fn only_an_optional_that_binds_the_graph_variable_leaves_the_single_evaluation() {
    for (text, once) in [
        (
            "SELECT * WHERE { GRAPH ?g { ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?x } } }",
            true,
        ),
        (
            "SELECT * WHERE { GRAPH ?g { ?s <urn:p> <urn:o> . ?s <urn:q> ?g } }",
            true,
        ),
        (
            "SELECT * WHERE { GRAPH ?g { ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?g } } }",
            false,
        ),
        (
            "SELECT * WHERE { GRAPH ?g { ?s <urn:p> <urn:o> \
             OPTIONAL { ?s <urn:p> ?y OPTIONAL { ?s <urn:q> ?g } } } }",
            false,
        ),
    ] {
        let (inner, variable) = under_graph(text);
        assert_eq!(
            scans_everywhere(&inner, &variable, false),
            once,
            "evaluated once for every graph: {text}"
        );
    }
}
