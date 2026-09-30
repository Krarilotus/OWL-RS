//! The evaluator's indexes are only indexes: a triple found through the shortest list of
//! a pattern's known terms is still matched in full. Paths, where a lookup by object once
//! let triples of another predicate through.

use nrese_sparql::{QueryOptions, QueryResults};
use nrese_sparql_reference::Dataset;
use oxrdf::{GraphName, NamedNode, Quad};
use spargebra::SparqlParser;

fn ex(l: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://e/{l}"))
}

fn count(dataset: &Dataset, text: &str) -> usize {
    let query = SparqlParser::new().parse_query(text).unwrap();
    match dataset.query(&query, &QueryOptions::default()).unwrap() {
        QueryResults::Solutions(s) => s.count(),
        _ => unreachable!(),
    }
}

#[test]
fn paths_match_their_predicate_whatever_index_is_used() {
    let dataset = Dataset::new([
        Quad::new(ex("a"), ex("p3"), ex("e0"), GraphName::DefaultGraph),
        Quad::new(ex("b"), ex("p3"), ex("e1"), GraphName::DefaultGraph),
        Quad::new(ex("c"), ex("p1"), ex("d"), GraphName::DefaultGraph),
        // Into e0 by another predicate: the object's list is the shorter index.
        Quad::new(ex("f"), ex("p2"), ex("e0"), GraphName::DefaultGraph),
        Quad::new(ex("g"), ex("p3"), ex("x"), GraphName::DefaultGraph),
        Quad::new(ex("h"), ex("p3"), ex("x"), GraphName::DefaultGraph),
    ]);
    for (text, expected) in [
        ("SELECT * WHERE { ?z <http://e/p3>? <http://e/e0> }", 2),
        ("SELECT * WHERE { ?z <http://e/p3> <http://e/e0> }", 1),
        ("SELECT * WHERE { <http://e/a> <http://e/p3>? ?z }", 2),
        ("SELECT * WHERE { ?z <http://e/p3>* <http://e/e0> }", 2),
        ("SELECT * WHERE { ?x <http://e/p3>|<http://e/p1> ?y }", 5),
        (
            "SELECT * WHERE { ?c <http://e/p1> ?d . ?z <http://e/p3>? <http://e/e0> }",
            2,
        ),
    ] {
        assert_eq!(count(&dataset, text), expected, "{text}");
    }
}
