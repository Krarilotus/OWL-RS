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

/// Which route each shape takes; the route is the engine's choice, not the specification's,
/// and the answers (SPARQL 1.1 §18.6) are held in `tests/it/graph_variable_tests.rs`. The
/// single evaluation is kept where it gives the same rows as §18.6: every part's rows come
/// from scans, and `?g` is named only where the rows before it bind `?g` from data. The
/// common shapes on the `true` side keep the route they had before this test.
#[test]
fn a_pattern_under_graph_is_evaluated_once_only_where_that_gives_the_same_rows() {
    let mut wrong = Vec::new();
    for (text, once) in [
        // An OPTIONAL that does not name `?g`.
        ("{ ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?x } }", true),
        // A required pattern binding `?g`.
        ("{ ?s <urn:p> <urn:o> . ?s <urn:q> ?g }", true),
        // A FILTER reading `?g` after a pattern that binds it.
        ("{ ?s <urn:q> ?g FILTER(?g != <urn:x>) }", true),
        // An OPTIONAL naming `?g` after a pattern that binds it.
        ("{ ?s <urn:q> ?g OPTIONAL { ?s <urn:r> ?g } }", true),
        // An EXISTS that does not name `?g`.
        ("{ ?s ?p ?o FILTER EXISTS { ?s <urn:q> ?x } }", true),
        // A FILTER or BIND reading `?g` where nothing binds it: free there.
        ("{ ?s ?p ?o FILTER(?g != <urn:x>) }", false),
        ("{ ?s ?p ?o BIND(STR(?g) AS ?name) }", false),
        (
            "{ ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?x FILTER(?g = <urn:g>) } }",
            false,
        ),
        // An OPTIONAL binding `?g` where the rows it extends lack it.
        ("{ ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:q> ?g } }", false),
        (
            "{ ?s <urn:p> <urn:o> OPTIONAL { ?s <urn:p> ?y OPTIONAL { ?s <urn:q> ?g } } }",
            false,
        ),
        // One UNION branch binds `?g`; the other reads it free.
        (
            "{ { ?s <urn:q> ?g } UNION { ?s <urn:p> ?x FILTER(!BOUND(?g)) } }",
            false,
        ),
        // A part whose row comes from no scan: one row for every graph at once.
        (
            "{ <urn:s> <urn:p> ?g OPTIONAL { BIND(BNODE() AS ?b) } }",
            false,
        ),
        (
            "{ ?s ?p ?o FILTER EXISTS { OPTIONAL { ?t <urn:q> ?x } } }",
            false,
        ),
        // MINUS: refused whatever it names.
        ("{ ?s ?p ?o MINUS { ?s <urn:q> ?x } }", false),
    ] {
        let text = format!("SELECT * WHERE {{ GRAPH ?g {text} }}");
        let (inner, variable) = under_graph(&text);
        if scans_everywhere(&inner, &variable, false) != once {
            wrong.push(format!(
                "evaluated once for every graph: {once} expected: {text}"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
