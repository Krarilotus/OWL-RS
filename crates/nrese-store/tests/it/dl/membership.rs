use nrese_store::dl::entailment::Entailed;

use super::entailment::{turtle, unchecked_inconsistent};
use super::queries::query;
use super::{PREFIXES, insert, pipeline};

const FIXED_PREDICATE: &str = ":p a owl:ObjectProperty . :x a owl:NamedIndividual ; :p :y .
    :y a owl:NamedIndividual .";
const FIXED_READ: &str = "ASK { :x :p ?o }";
const ANONYMOUS_TREE_AND_SCHEMA: &str =
    ":q a owl:ObjectProperty . :z a owl:NamedIndividual . _:b :q :y, :z .
     :A a owl:Class ; rdfs:subClassOf [
         a owl:Restriction ; owl:onProperty :q ; owl:someValuesFrom :B
     ] . :B a owl:Class .";

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
/// The same functional property alone is simple and the read stays complete.
#[test]
fn a_property_above_the_bottom_property_is_not_simple() {
    let functional = ":q a owl:ObjectProperty, owl:FunctionalProperty .
        :x a owl:NamedIndividual ; :q :y . :y a owl:NamedIndividual .";
    let select = "SELECT ?x { :x :q ?x }";
    let (_, control) = query(&preloaded(functional), select);
    assert!(control.is_complete(), "{control:?}");
    let dl = preloaded(&format!(
        "{functional} owl:bottomObjectProperty rdfs:subPropertyOf :q ."
    ));
    let (_, status) = query(&dl, select);
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
        reason.contains("a non-simple property in a functional property"),
        "{reason}"
    );
}

/// OWL 2 Structural Specification §11.2: two distinct definitions of the same
/// datatype put the stored ontology outside OWL 2 DL, even if no assertion uses it.
/// One definition is permitted. The test uses a fixed predicate because an
/// unbound predicate already makes a read incomplete over a valid ontology.
#[test]
fn two_datatype_definitions_withhold_the_word() {
    let mut wrong = Vec::new();
    for (definitions, complete) in [("xsd:integer", true), ("xsd:integer, xsd:string", false)] {
        let dl = preloaded(&format!(
            "{FIXED_PREDICATE} :dt a rdfs:Datatype ; owl:equivalentClass {definitions} ."
        ));
        let (_, status) = query(&dl, FIXED_READ);
        if status.is_complete() != complete {
            wrong.push(format!("{definitions}: {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
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

/// OWL 2 Structural Specification §11.2 permits a singleton anonymous tree
/// with one named attachment. Two attachments give a membership report on a
/// read of a fixed user predicate.
#[test]
fn anonymous_tree_attachments_are_reported_on_a_fixed_predicate_read() {
    let fixture = format!("{FIXED_PREDICATE} :q a owl:ObjectProperty . :z a owl:NamedIndividual .");
    let (_, control) = query(&preloaded(&format!("{fixture} _:b :q :y .")), FIXED_READ);
    assert!(control.is_complete(), "{control:?}");
    assert!(
        !control.reasons().iter().any(|r| r.contains("anonymous")),
        "{control:?}"
    );
    let (_, status) = query(
        &preloaded(&format!("{fixture} _:b :q :y, :z .")),
        FIXED_READ,
    );
    assert!(
        status
            .reasons()
            .iter()
            .any(|r| r.contains("anonymous tree has no root")),
        "{status:?}"
    );
}

/// A non-fatal anonymous-tree finding belongs to the committed revision.
/// An empty update keeps that revision and its query report.
#[test]
fn an_empty_update_keeps_the_anonymous_report() {
    let dl = pipeline();
    insert(
        &dl,
        &format!(
            "{FIXED_PREDICATE} :q a owl:ObjectProperty . :z a owl:NamedIndividual .
             _:b :q :y, :z .
             :A a owl:Class ; rdfs:subClassOf [
                 a owl:Restriction ; owl:onProperty :q ; owl:someValuesFrom :B
             ] . :B a owl:Class ."
        ),
    )
    .expect("admitted");
    let bounds = dl.store().dl_bounds();
    assert_eq!(bounds.last, "rebuilt");
    assert!(bounds.unavailable.is_none());
    let report = || {
        let (answers, status) = query(&dl, FIXED_READ);
        assert_eq!(answers, ["true"]);
        status
            .reasons()
            .into_iter()
            .find(|reason| reason.contains("anonymous tree has no root"))
    };
    let before = report().expect("the original anonymous report");
    let revision = dl.store().current_revision();
    assert_eq!(insert(&dl, "").expect("empty update"), revision);
    assert_eq!(report().as_deref(), Some(before.as_str()));
}

/// The anonymous report identifies the revision whose ontology established it.
/// A data-only write keeps the earlier evidence. Subsequent schema and blank-node
/// edits reread the ontology and refresh or clear the report.
#[test]
fn an_anonymous_report_names_the_revision_that_was_read() {
    use nrese_store::{DlConfig, DlConsistency};

    let dl = super::pipeline_with(DlConfig {
        consistency: DlConsistency::Off,
        ..DlConfig::default()
    });
    let data = format!("{FIXED_PREDICATE} {ANONYMOUS_TREE_AND_SCHEMA}");
    let first = insert(&dl, &data).expect("admitted");
    let report = |revision| {
        assert_eq!(dl.store().current_revision(), revision);
        let (answers, status) = query(&dl, FIXED_READ);
        assert_eq!(answers, ["true"]);
        status
            .reasons()
            .into_iter()
            .find(|reason| reason.contains("anonymous tree has no root"))
    };
    assert!(
        report(first).is_some(),
        "warm the first revision's taxonomy"
    );
    let named = insert(&dl, ":x :p :z .").expect("named assertion");
    assert!(named > first);
    assert_eq!(dl.store().dl_bounds().last, "delta");
    assert!(
        report(named).is_some_and(|text| text.ends_with(&format!("as of revision {first}"))),
        "named assertion must keep the report from revision {first}"
    );
    let schema = insert(&dl, ":D a owl:Class .").expect("schema edit");
    assert!(schema > named);
    assert_eq!(dl.store().dl_bounds().last, "rebuilt");
    assert!(
        report(schema).is_some_and(|text| text.ends_with(&format!("as of revision {schema}"))),
        "schema edit must refresh the report at revision {schema}"
    );
    let cleared = super::update(&dl, "DELETE WHERE { ?b :q :z }").expect("remove one attachment");
    assert!(cleared > schema);
    assert_eq!(dl.store().dl_bounds().last, "rebuilt");
    assert_eq!(report(cleared), None);
}

/// An anonymous-tree report is evidence from the ontology read that established
/// it. A committed query that needs the full ontology refreshes this evidence,
/// both while building the taxonomy and while checking an absent individual.
#[test]
fn a_committed_full_read_refreshes_the_carried_anonymous_report() {
    use nrese_store::{DlConfig, DlConsistency};

    let mut wrong = Vec::new();
    for warm in [false, true] {
        let dl = super::pipeline_with(DlConfig {
            consistency: DlConsistency::Off,
            ..DlConfig::default()
        });
        let first = insert(
            &dl,
            &format!("{FIXED_PREDICATE} {ANONYMOUS_TREE_AND_SCHEMA}"),
        )
        .expect("premise");
        if warm {
            let (answers, _) = query(&dl, FIXED_READ);
            assert_eq!(answers, ["true"]);
        }
        let second = insert(&dl, ":x :p :z .").expect("named assertion");
        assert!(second > first);
        let (answers, status) = query(&dl, FIXED_READ);
        assert_eq!(answers, ["true"]);
        let check = |status: &super::Status, revision| {
            let suffix = format!("as of revision {revision}");
            status.reasons().iter().any(|reason| {
                reason.contains("anonymous tree has no root") && reason.ends_with(&suffix)
            })
        };
        if warm {
            assert!(check(&status, first), "{status:?}");
            let (answers, status) = query(&dl, "ASK { :missing :p ?o }");
            assert_eq!(answers, ["false"]);
            if !check(&status, second) {
                wrong.push(("absent individual", second, status));
            }
            let (answers, status) = query(&dl, FIXED_READ);
            assert_eq!(answers, ["true"]);
            if !check(&status, second) {
                wrong.push(("next fixed read", second, status));
            }
        } else if !check(&status, second) {
            wrong.push(("cold taxonomy", second, status));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// OWL 2 Mapping to RDF Graphs Table 12: one member cannot form a data
/// intersection or union. Two members, and a one-literal enumeration, can.
/// The test uses a fixed predicate because an unbound predicate already
/// makes a read incomplete over a valid ontology.
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
            "{FIXED_PREDICATE} :d a owl:DatatypeProperty ;
             rdfs:range [ a rdfs:Datatype ; {expression} ] ."
        ));
        let (_, status) = query(&dl, FIXED_READ);
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

/// Mapping to RDF Graphs Table 12 gives no reading to a one-member data
/// intersection. OWL 2 Conformance §2.2.1 permits an unknown entailment answer.
/// The premise's inconsistency remains established.
#[test]
fn a_singleton_data_intersection_in_the_conclusion_is_unknown() {
    let dl = unchecked_inconsistent();
    let report = dl
        .store()
        .entails_dl(&turtle(
            ":d a owl:DatatypeProperty ; rdfs:range [
         a rdfs:Datatype ; owl:intersectionOf ( xsd:integer ) ] .",
        ))
        .expect("entailment");
    assert!(matches!(report.answer, Entailed::Unknown(_)), "{report:?}");
    assert_eq!(report.premise.as_str(), "inconsistent");
}
