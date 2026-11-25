use nrese_rdf::{Literal, NamedNode};
use nrese_shacl::{Component, SH};

use super::{Row, core_key, predicate};
use crate::change_differential::case::ex;
use crate::change_differential::case::{Case, Step};
use crate::change_differential::observe::{ResultKey, ResultTerm};
use crate::validation_tests::FRESH_VALUE_SHAPES;

fn sparql_key(shape: &str, focus: &str, source: &str, value: ResultTerm) -> ResultKey {
    ResultKey {
        focus: ResultTerm::Rdf(ex(focus).into()),
        path: None,
        value: Some(value),
        shape: ResultTerm::Rdf(ex(shape).into()),
        constraint: Some(ResultTerm::Rdf(ex(source).into())),
        component: NamedNode::new_unchecked(Component::Sparql.iri()),
        severity: NamedNode::new_unchecked(format!("{SH}Violation")),
        messages: vec![],
    }
}

// Spec: report identity includes severity, source constraint and RDF messages.
// A literal's storage and the label minted for a fresh value are incidental.
pub(super) fn rows() -> [Row; 5] {
    let property = |metadata| {
        format!(
            "ex:T sh:targetNode ex:n0 ; sh:property ex:Q .
         ex:Q sh:path ex:p0 ; sh:minCount 1 {metadata} ."
        )
    };
    let shapes = |before, after, data, add| {
        let mut step = Step::insert(add);
        step.shapes = Some(after);
        Case::new(before, data, step)
    };
    let core = core_key("n0", predicate("p0"), None, "Q", Component::MinCount);
    let language = |language| ResultKey {
        messages: vec![Literal::new_language_tagged_literal("a", language).unwrap()],
        ..core.clone()
    };
    let query = "SELECT $this WHERE { $this <http://example.com/p0> ?x }";
    let one = format!(
        "ex:T sh:targetNode ex:n0 ; sh:sparql ex:A .
        ex:A sh:select '''{query}''' ."
    );
    let two = format!("{one} ex:T sh:sparql ex:B . ex:B sh:select '''{query}''' .");
    let focus_value = ResultTerm::Rdf(ex("n0").into());
    let a = sparql_key("T", "n0", "A", focus_value.clone());
    let b = sparql_key("T", "n0", "B", focus_value.clone());
    let message_query = "ex:T sh:targetNode ex:n0 ; sh:sparql ex:A .
        ex:A sh:select '''SELECT $this ?message WHERE {
            BIND(CONCAT('wr', 'ong') AS ?message)
        }''' .";
    let computed = ResultKey {
        messages: vec![Literal::from("wrong")],
        ..a.clone()
    };
    let mut fresh = shapes(
        FRESH_VALUE_SHAPES.to_owned(),
        FRESH_VALUE_SHAPES.to_owned(),
        "ex:a ex:p ex:b .",
        "",
    );
    fresh.fresh_value = true;
    let blank = sparql_key("S", "a", "Check", ResultTerm::FreshBlank);
    [
        Row::new(
            "identity-severity",
            "Changing only the severity introduces a different result.",
            shapes(property("; sh:severity sh:Warning"), property(""), "", ""),
            [ResultKey {
                severity: NamedNode::new_unchecked(format!("{SH}Warning")),
                ..core.clone()
            }],
            [core.clone()],
        )
        .for_shapes(),
        Row::new(
            "identity-source",
            "A second SPARQL constraint can report an otherwise identical result.",
            shapes(one, two, "ex:n0 ex:p0 ex:v1 .", ""),
            [a.clone()],
            [a, b],
        )
        .for_shapes(),
        Row::new(
            "identity-message-language",
            "The same message text in another language gives a new result.",
            shapes(
                property("; sh:message \"a\"@en"),
                property("; sh:message \"a\"@de"),
                "",
                "",
            ),
            [language("en")],
            [language("de")],
        )
        .for_shapes(),
        Row::new(
            "identity-message-storage",
            "Storing the literal of a computed message leaves the result the same.",
            shapes(
                message_query.to_owned(),
                message_query.to_owned(),
                "",
                "ex:n0 ex:msg \"wrong\" .",
            ),
            [computed.clone()],
            [computed],
        )
        .for_shapes(),
        Row::new(
            "identity-fresh-value",
            "A new label for the same fresh blank value leaves the result the same.",
            fresh,
            [blank.clone()],
            [blank],
        )
        .for_shapes(),
    ]
}
