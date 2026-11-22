//! Who is named under a key.
//!
//! `C ⊑ D` is not entailed when some model has an element in `C` and outside `D`. That
//! element needn't be named. A key holds only for named individuals (*OWL 2 Direct
//! Semantics* §2.3.5, `HasKey`), so it can't merge such an element with a named one. An
//! individual the question names is named, though the premise doesn't mention it
//! (entailment is over the vocabulary of both, §2.5), so the key does merge it.

use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::dl::entailment::Entailed;

use super::entailment::{entailed, turtle};
use super::{insert, pipeline};

/// Every `:C` has `:k` = 7 and `:k` is a key of `:C`; `:a` is a `:C` and a `:D`.
const KEYED: &str = ":k a owl:DatatypeProperty . :C owl:hasKey ( :k ) ; rdfs:subClassOf \
     [ a owl:Restriction ; owl:onProperty :k ; owl:hasValue 7 ] . :a a :C , :D .";

/// Spec (module docs): an unnamed `:C` with `:k` = 7 outside `:D` is a model, so `C ⊑ D`
/// is not entailed; the DL check may say no or that it can't decide, never yes.
#[test]
fn a_key_does_not_merge_the_counterexample_of_a_subsumption_with_a_named_individual() {
    let answer = entailed(KEYED, ":C rdfs:subClassOf :D .");
    assert!(
        matches!(answer, Entailed::No | Entailed::Unknown(_)),
        "{answer:?}"
    );
}

/// Spec (module docs): the key still merges named individuals: a named `:b` in `:C` has
/// `:k` = 7 as `:a` has, so it is `:a` and in `:D`.
#[test]
fn a_key_merges_a_named_individual_with_another() {
    let premise = format!("{KEYED} :b a :C .");
    assert_eq!(entailed(&premise, ":b a :D ."), Entailed::Yes);
}

/// Spec (module docs): every individual has `:r` = `:a` and `:r` is a key of every class,
/// so every named individual is `:a`; `:b`, named only by the question, is too, and in
/// `:A`. Both checks under `owl2-dl` say so: the rules don't, so the store's check asks
/// the DL engines.
#[test]
fn an_individual_named_only_by_the_question_is_merged_by_a_key() {
    let premise = ":r a owl:ObjectProperty . :A a owl:Class . :a a owl:NamedIndividual , :A . \
                   owl:Thing owl:hasKey ( :r ) ; \
                   rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:hasValue :a ] .";
    let dl = pipeline();
    insert(&dl, premise).expect("the keys");
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .expect("rules");
    let conclusion = turtle(":b a :A .");
    let by_store = format!("{:?}", dl.store().entails(rules, &conclusion));
    let by_dl = dl
        .store()
        .entails_dl(&conclusion)
        .expect("entailment")
        .answer;
    assert_eq!(
        (by_store.as_str(), by_dl),
        ("Ok(Entailed)", Entailed::Yes),
        "entails under owl2-dl, entails_dl"
    );
}
