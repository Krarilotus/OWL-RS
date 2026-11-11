//! The DL's own terms are apart from every term the data holds (D16-051, C-502). An IRI
//! is the resource it names (RDF 1.1 Concepts §3.2) wherever it sits in the IRI space: the
//! IRIs the DL once spelled its own terms with (`urn:nrese:dl:fresh:{i}`, `urn:nrese:u1:…`)
//! are, in the data, individuals like any other. Each case states its answer by OWL 2's
//! Direct Semantics alone.

use nrese_rdf::{NamedNode, TermRef};
use nrese_store::ReadScope;
use nrese_store::dl::entailment::Entailed;

use super::entailment::entailed;
use super::queries::query;
use super::{insert, pipeline};

/// The IRIs the entailment tests' individuals were once spelled with.
fn old_fresh(i: usize) -> String {
    format!("<urn:nrese:dl:fresh:{i}>")
}

/// `A ⊑ D` is not entailed by `D(a)` for any individual `a`: a model with an `A` that is
/// not a `D` (domain {a, y}, A = {y}, D = {a}) satisfies the premise.
#[test]
fn a_subsumption_is_not_entailed_by_a_membership_whatever_its_individual() {
    let classes = ":A a owl:Class . :D a owl:Class . ";
    for i in 0..8 {
        let premise = format!("{classes}{} a :D .", old_fresh(i));
        assert_eq!(
            entailed(&premise, ":A rdfs:subClassOf :D ."),
            Entailed::No,
            "D({})",
            old_fresh(i)
        );
    }
    let all: String = (0..8)
        .map(|i| format!("{} a :D . ", old_fresh(i)))
        .collect();
    assert_eq!(
        entailed(&format!("{classes}{all}"), ":A rdfs:subClassOf :D ."),
        Entailed::No,
        "all eight"
    );
    // The same premise under an ordinary IRI: the answer doesn't depend on the spelling.
    assert_eq!(
        entailed(&format!("{classes}:a a :D ."), ":A rdfs:subClassOf :D ."),
        Entailed::No
    );
    // Control: a subsumption the premise does give is still entailed.
    let premise =
        format!("{classes}:B a owl:Class . :A rdfs:subClassOf :B . :B rdfs:subClassOf :D . {all}");
    assert_eq!(entailed(&premise, ":A rdfs:subClassOf :D ."), Entailed::Yes);
}

/// C-502's case: `¬A(a)` with A and B declared doesn't entail `A ⊑ B` (a model with
/// another individual in A and not in B satisfies it).
#[test]
fn a_complement_membership_does_not_entail_a_subsumption() {
    let premise = format!(
        ":A a owl:Class . :B a owl:Class . {} a [ a owl:Class ; owl:complementOf :A ] .",
        old_fresh(0)
    );
    assert_eq!(entailed(&premise, ":A rdfs:subClassOf :B ."), Entailed::No);
}

/// An explanation exists only for a statement that holds; `A ⊑ D` doesn't (as above).
#[test]
fn a_subsumption_that_does_not_hold_has_no_explanation() {
    let dl = pipeline();
    insert(
        &dl,
        &format!(":A a owl:Class . :D a owl:Class . {} a :D .", old_fresh(0)),
    )
    .expect("data");
    let (a, d) = (
        NamedNode::new_unchecked("http://example.com/A"),
        NamedNode::new_unchecked("http://example.com/D"),
    );
    let sub = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf");
    let statement: [TermRef<'_>; 3] = [a.as_ref().into(), sub.as_ref().into(), d.as_ref().into()];
    let explained = dl.store().explain_dl(&ReadScope::All, statement);
    assert!(matches!(explained, Ok(None)), "{explained:?}");
}

/// `w a (B ⊔ C)`, `B ⊑ D`, `C ⊑ D`: w is a D in every model, whatever its IRI; the answer
/// comes from the bounds' gap (U1 has it, L doesn't).
#[test]
fn a_gap_answer_is_returned_whatever_its_iri() {
    let dl = pipeline();
    insert(
        &dl,
        ":B rdfs:subClassOf :D . :C rdfs:subClassOf :D . \
         <urn:nrese:u1:w> a [ owl:unionOf ( :B :C ) ] . :y a :B .",
    )
    .expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(rows, ["urn:nrese:u1:w", "y"], "{:?}", status.reasons());
    assert!(status.is_complete(), "{:?}", status.reasons());
}

/// `ann :takesCourse :c1` is asserted: ann is an answer, whatever its IRI, on the path that
/// answers from L alone.
#[test]
fn an_asserted_answer_is_returned_whatever_its_iri() {
    let dl = pipeline();
    insert(
        &dl,
        ":Student rdfs:subClassOf [ a owl:Restriction ; \
           owl:onProperty :takesCourse ; owl:someValuesFrom :Course ] . \
         <urn:nrese:u1:ann> a :Student ; :takesCourse :c1 . :bob a :Student . :c1 a :Course .",
    )
    .expect("data");
    for q in [
        "SELECT ?x { ?x :takesCourse :c1 }",
        "SELECT ?x ?y { ?x :takesCourse ?y }",
    ] {
        let (rows, status) = query(&dl, q);
        assert_eq!(rows, ["urn:nrese:u1:ann"], "{q}: {:?}", status.reasons());
        assert!(status.is_complete(), "{q}: {:?}", status.reasons());
    }
    // A query naming the IRI reads it as the term it is.
    let (rows, status) = query(&dl, "SELECT ?x { <urn:nrese:u1:ann> :takesCourse ?x }");
    assert_eq!(rows, ["c1"], "{:?}", status.reasons());
}
