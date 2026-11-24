use nrese_store::dl::entailment::Entailed;

use super::entailment::{turtle, unchecked_inconsistent};
use super::queries::query;
use super::{PREFIXES, insert, pipeline};

fn preloaded(data: &str) -> nrese_store::MutationPipeline {
    let dl = pipeline();
    crate::support::load_past_the_pipeline(dl.store(), &format!("{PREFIXES}{data}"));
    dl.store()
        .rematerialise(
            dl.reasoner()
                .config()
                .materialised_program()
                .expect("rules"),
        )
        .expect("rematerialisation");
    dl
}

/// OWL 2 Structural Specification §§11.1–11.2: the bottom object property
/// is composite, so a functional superproperty lies outside OWL 2 DL.
#[test]
fn a_property_above_the_bottom_property_is_not_simple() {
    let dl = preloaded(
        ":q a owl:ObjectProperty, owl:FunctionalProperty .
         owl:bottomObjectProperty rdfs:subPropertyOf :q .
         :x a owl:NamedIndividual ; :q :y . :y a owl:NamedIndividual .",
    );
    let (_, status) = query(&dl, "SELECT ?x { :x :q ?x }");
    assert!(!status.is_complete(), "{status:?}");
    let verdict = dl
        .store()
        .dl()
        .status()
        .expect("DL status")
        .consistency
        .verdict;
    let reason = verdict.reason().unwrap_or_default();
    assert!(
        reason.contains("non-simple") && reason.contains("/q"),
        "{reason}"
    );
}

/// OWL 2 Structural Specification §11.2: two distinct definitions of the same
/// datatype put the stored ontology outside OWL 2 DL, even without data assertions.
#[test]
fn two_datatype_definitions_withhold_the_word() {
    let dl = preloaded(":dt a rdfs:Datatype ; owl:equivalentClass xsd:integer, xsd:string .");
    let (_, status) = query(&dl, "ASK { ?s ?p ?o }");
    assert!(!status.is_complete(), "{status:?}");
}

/// OWL 2 Structural Specification §§4 and 7.5: minInclusive has no facet space
/// on xsd:string. The store reports this restriction on its input by name.
#[test]
fn a_facet_its_datatype_does_not_take_is_reported() {
    let dl = preloaded(
        ":d a owl:DatatypeProperty ; rdfs:range [ a rdfs:Datatype ;
         owl:onDatatype xsd:string ; owl:withRestrictions ( [ xsd:minInclusive 5 ] ) ] .",
    );
    let (_, status) = query(&dl, "ASK { ?s ?p ?o }");
    let reasons = status.reasons();
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("facet is not applicable")),
        "{status:?}"
    );
}

/// OWL 2 Structural Specification §11.2: anonymous individuals cannot occur
/// in equality, inequality or negative assertions, and their graph must be
/// a forest with one assertion per pair and a root with at most one named attachment.
#[test]
fn anonymous_individuals_outside_owl2_dl_are_reported() {
    let cases = [
        (
            "_:a :p _:b . _:b :p _:c . _:c :p _:a . :n :p _:a .",
            "anonymous individual cycle",
        ),
        (
            "_:b owl:sameAs :a . _:b a :C .",
            "anonymous individual in SameIndividual",
        ),
        (
            "_:b owl:differentFrom :a .",
            "anonymous individual in DifferentIndividuals",
        ),
        (
            "[] a owl:NegativePropertyAssertion ; owl:sourceIndividual _:b ;
          owl:assertionProperty :p ; owl:targetIndividual :x .",
            "anonymous individual in a negative assertion",
        ),
        (
            "[] a owl:NegativePropertyAssertion ; owl:sourceIndividual _:b ;
          owl:assertionProperty :d ; owl:targetValue 1 .",
            "anonymous individual in a negative assertion",
        ),
        (
            "_:a :p _:b . _:a :q _:b .",
            "multiple assertions between anonymous individuals",
        ),
        (
            "_:b :p :x . _:b :p :y .",
            "anonymous tree has no root with at most one named attachment",
        ),
        (":a :p :b . :b :p :c . :c :p :a . :n :p :a .", ""),
        ("GRAPH :g1 { _:a :p _:b } GRAPH :g2 { _:a :p _:b }", ""),
    ];
    let mut wrong = Vec::new();
    for (data, expected) in cases {
        let dl = preloaded(&format!(
            ":C a owl:Class . :p a owl:ObjectProperty . :q a owl:ObjectProperty .
             :d a owl:DatatypeProperty . :x a owl:NamedIndividual . {data}"
        ));
        let (_, status) = query(&dl, "ASK { ?s ?p ?o }");
        let reasons = status.reasons();
        let actual = reasons.iter().find(|r| r.contains("anonymous"));
        if !match actual {
            Some(reason) => !expected.is_empty() && reason.contains(expected),
            None => expected.is_empty(),
        } {
            wrong.push(format!("{data}: expected {expected:?}, got {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// OWL 2 Mapping to RDF Graphs Table 12: one member cannot form a data
/// intersection or union. Two members, and a one-literal enumeration, can.
#[test]
fn a_one_member_data_intersection_or_union_has_no_reading() {
    let cases = [
        ("owl:intersectionOf ( xsd:integer )", false),
        ("owl:unionOf ( xsd:integer )", false),
        ("owl:intersectionOf ( xsd:integer xsd:decimal )", true),
        ("owl:unionOf ( xsd:integer xsd:decimal )", true),
        ("owl:oneOf ( 1 )", true),
    ];
    let mut wrong = Vec::new();
    for (expression, complete) in cases {
        let dl = preloaded(&format!(
            ":d a owl:DatatypeProperty ; rdfs:range [ a rdfs:Datatype ; {expression} ] ."
        ));
        let (_, status) = query(&dl, "ASK { ?s ?p ?o }");
        if status.is_complete() != complete
            || (!complete
                && !status
                    .reasons()
                    .iter()
                    .any(|r| r.contains("at least two data ranges")))
        {
            wrong.push(format!("{expression}: {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The conclusion can use a datatype defined in the premise's ontology.
/// The premise has no model, so it entails this conclusion.
#[test]
fn conclusion_can_use_a_datatype_defined_in_the_premise() {
    let dl = unchecked_inconsistent();
    insert(
        &dl,
        "<urn:test:p> a owl:Ontology . :dt a rdfs:Datatype ; owl:equivalentClass xsd:integer .",
    )
    .expect("datatype definition");
    let report = dl
        .store()
        .entails_dl(&turtle(
            "<urn:test:c> a owl:Ontology ; owl:imports <urn:test:p> .
             :d a owl:DatatypeProperty ; rdfs:range :dt .",
        ))
        .expect("entailment");
    assert_eq!(report.answer, Entailed::Yes);
    assert!(report.holds());
    assert_eq!(report.premise.as_str(), "inconsistent");
}
