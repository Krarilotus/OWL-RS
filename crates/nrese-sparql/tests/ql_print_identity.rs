//! The QL printer contract (docs/design/ql-rewriting.md §8): printed queries over
//! assertions give the answers of reasoning, without capturing legal user variables.
use nrese_engine::{Engine, EngineConfig, Snapshot};
use nrese_rdf::{Literal, NamedNode, Term};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::ql::{PrintForm, Printed};
use nrese_sparql::{QueryOptions, QueryResults};
use nrese_sparql_syntax::SparqlParser;

fn snapshot() -> Snapshot {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let data = "@prefix : <urn:ex:> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        :Sub rdfs:subClassOf :Super . :a a :Sub .";
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(data.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine.snapshot()
}

fn printed_rows(query: &str, form: PrintForm, column: &str) -> Vec<Option<Term>> {
    let snapshot = snapshot();
    let parser = SparqlParser::new();
    let query = parser
        .parse_query(&format!("PREFIX : <urn:ex:> {query}"))
        .unwrap();
    let printed = nrese_sparql::ql::print(&snapshot, &query, form).unwrap();
    let Printed::Query { text, .. } = printed else {
        panic!("expressible: {printed:?}")
    };
    let query = parser.parse_query(&text).unwrap();
    let QueryResults::Solutions(rows) =
        nrese_sparql_reference::evaluate_query(&snapshot, &query, &QueryOptions::default())
            .unwrap()
    else {
        panic!("SELECT: {text}")
    };
    rows.map(|row| row.unwrap().get(column).cloned()).collect()
}

#[test]
fn ql_values_does_not_capture_p1() {
    // Spec: a legal user variable remains the instance column when VALUES introduces
    // its own class column; :a is the one instance of :Super through :Sub.
    assert_eq!(
        printed_rows("SELECT ?_p1 { ?_p1 a :Super }", PrintForm::Values, "_p1"),
        [Some(NamedNode::new("urn:ex:a").unwrap().into())]
    );
}

#[test]
fn ql_blank_pattern_does_not_capture_a_user_variable() {
    // Spec: _:b is existential, independent of the user's ?_b_b.
    for form in [PrintForm::Paths, PrintForm::Values] {
        assert_eq!(
            printed_rows(
                "SELECT ?_b_b { _:b a :Super . VALUES ?_b_b { :answer } }",
                form,
                "_b_b",
            ),
            [Some(NamedNode::new("urn:ex:answer").unwrap().into())],
            "{form:?}",
        );
    }
}

#[test]
fn ql_aggregate_exists_is_expanded() {
    // Spec: the single subclass instance contributes one to SUM through EXISTS.
    for form in [PrintForm::Paths, PrintForm::Values] {
        assert_eq!(
            printed_rows(
                "SELECT (SUM(IF(EXISTS { ?x a :Super },1,0)) AS ?n) \
                 { VALUES ?x { :a } }",
                form,
                "n",
            ),
            [Some(Literal::from(1).into())],
            "{form:?}",
        );
    }
}

#[test]
fn ql_order_exists_is_expanded() {
    // Spec: :a satisfies EXISTS through :Sub and sorts before the non-instance :z.
    for form in [PrintForm::Paths, PrintForm::Values] {
        assert_eq!(
            printed_rows(
                "SELECT ?x { VALUES ?x { :z :a } } \
                 ORDER BY DESC(EXISTS { ?x a :Super }) LIMIT 1",
                form,
                "x",
            ),
            [Some(NamedNode::new("urn:ex:a").unwrap().into())],
            "{form:?}",
        );
    }
}
