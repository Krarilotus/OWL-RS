//! The store's entailment check (`StoreService::entails`): positive statements through the
//! closure, negative ones by refutation in a speculative transaction that is never
//! committed.

use nrese_rdf::Triple;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{Entailment, StoreConfig, StoreService};

const PREFIXES: &str = "@prefix : <http://example.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
";

fn store(premise: &str) -> StoreService {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .execute_update_str(&format!(
            "PREFIX : <http://example.org/> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA {{ {premise} }}"
        ))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store
}

fn turtle(text: &str) -> Vec<Triple> {
    RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(format!("{PREFIXES}{text}").as_bytes())
        .map(|t| Triple::from(t.unwrap()))
        .collect()
}

fn entails(store: &StoreService, conclusion: &str) -> Entailment {
    store.entails(Ruleset::Owl2Rl, &turtle(conclusion)).unwrap()
}

#[test]
fn positive_statements_come_from_the_closure() {
    let store = store(":Boy rdfs:subClassOf :Person . :stewie a :Boy .");
    assert_eq!(entails(&store, ":stewie a :Person ."), Entailment::Entailed);
    assert_eq!(
        entails(&store, ":stewie a :Girl ."),
        Entailment::NotEntailed
    );
    // Blank nodes are variables.
    assert_eq!(entails(&store, "[] a :Person ."), Entailment::Entailed);
}

#[test]
fn difference_is_decided_by_refutation() {
    // Two values of a functional property, known to differ: their subjects differ.
    let store = store(
        ":hasMother a owl:FunctionalProperty . :a :hasMother :m1 . :b :hasMother :m2 .
         :m1 owl:differentFrom :m2 .",
    );
    assert_eq!(
        entails(&store, ":a owl:differentFrom :b ."),
        Entailment::Entailed
    );
    assert_eq!(
        entails(&store, ":b owl:differentFrom :a ."),
        Entailment::Entailed
    );
    assert_eq!(
        entails(&store, ":a owl:differentFrom :m1 ."),
        Entailment::NotEntailed
    );
    assert_eq!(
        entails(&store, "[] a owl:AllDifferent ; owl:members ( :a :b ) ."),
        Entailment::Entailed
    );
    assert_eq!(
        entails(
            &store,
            "[] a owl:AllDifferent ; owl:members ( :a :b :m1 ) ."
        ),
        Entailment::NotEntailed
    );
}

#[test]
fn complements_and_negative_assertions_are_decided_by_refutation() {
    let store = store(
        ":Boy owl:disjointWith :Girl . :stewie a :Boy .
         :hates a owl:AsymmetricProperty . :tom :hates :jerry .",
    );
    assert_eq!(
        entails(&store, ":stewie a [ owl:complementOf :Girl ] ."),
        Entailment::Entailed
    );
    assert_eq!(
        entails(&store, ":stewie a [ owl:complementOf :Boy ] ."),
        Entailment::NotEntailed
    );
    assert_eq!(
        entails(
            &store,
            "[] a owl:NegativePropertyAssertion ; owl:sourceIndividual :jerry ;
                owl:assertionProperty :hates ; owl:targetIndividual :tom ."
        ),
        Entailment::Entailed
    );
    // A refutation is never committed: the store still holds what it held.
    assert_eq!(
        entails(&store, ":stewie a :Girl ."),
        Entailment::NotEntailed
    );
}

#[test]
fn an_inconsistent_premise_entails_everything_and_a_stale_closure_is_refused() {
    let store = store(":Boy owl:disjointWith :Girl . :pat a :Boy , :Girl .");
    assert_eq!(
        entails(&store, ":x a :Y ."),
        Entailment::InconsistentPremise
    );

    let rdfs_only = StoreService::new(StoreConfig::in_memory()).unwrap();
    rdfs_only.rematerialise(Ruleset::Rdfs).unwrap();
    assert!(
        rdfs_only
            .entails(Ruleset::Owl2Rl, &turtle(":x a :Y ."))
            .is_err()
    );
}
