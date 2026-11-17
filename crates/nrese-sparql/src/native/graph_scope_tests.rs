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

/// Spec: SPARQL 1.1 §18.6. A pattern that never names `?g`, or names it only where the
/// rows before it bind `?g` from data, gives the same rows either way and keeps the single
/// evaluation; an OPTIONAL, FILTER or BIND that names `?g` where those rows may lack it
/// must see it free, which only the graph-by-graph evaluation gives.
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
