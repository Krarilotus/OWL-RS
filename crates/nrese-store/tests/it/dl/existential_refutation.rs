//! A negative class assertion that only an existential's successor refutes, under `owl2-dl`.
//!
//! `:C ⊑ ∃p.:A`, the range of `:p` is `:B`, and `:A` is disjoint with `:B`: a `:p`-successor
//! of a `:C` would be both `:A` and `:B`, so `:C` has no instance in any model and every
//! individual is in its complement (*OWL 2 Direct Semantics* §2.2–2.3). The premise is
//! consistent; `:a a [ owl:complementOf :C ]` is entailed. Without the range, `:C` is
//! satisfiable and the same conclusion is not entailed.

use nrese_rdf::Triple;
use nrese_reasoner::{ReasonerConfig, ReasoningMode, RuleProgram};
use nrese_store::dl::entailment::Entailed;
use nrese_store::{Entailment, StoreError};

use super::entailment::turtle;
use super::{insert, pipeline};

const PREMISE: &str = ":p a owl:ObjectProperty ; rdfs:range :B . :A owl:disjointWith :B . \
     :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :A ] . \
     :a a :D .";

/// [`PREMISE`] without the range: `:C` is satisfiable.
const CONTROL: &str = ":p a owl:ObjectProperty . :A owl:disjointWith :B . \
     :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :A ] . \
     :a a :D .";

const CONCLUSION: &str = ":a a [ owl:complementOf :C ] .";

/// The rules the store under `owl2-dl` maintains.
fn rules() -> RuleProgram {
    ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .expect("owl2-dl maintains the OWL 2 RL closure")
}

/// Both entailment checks of a store under `owl2-dl` with `premise` committed: the rules'
/// (`entails`, with the program the mode maintains) and the DL engines' (`entails_dl`).
fn both(premise: &str, conclusion: &[Triple]) -> (Result<Entailment, StoreError>, Entailed) {
    let dl = pipeline();
    insert(&dl, premise).expect("the premise is consistent and commits");
    let store = dl.store();
    let rules = store.entails(rules(), conclusion);
    let answer = store.entails_dl(conclusion).expect("entailment");
    assert_eq!(answer.premise.as_str(), "consistent");
    (rules, answer.answer)
}

/// Spec: the conclusion is entailed (module docs). Either check may say so or say it
/// can't decide (undecided, unknown); neither may deny it.
#[test]
fn a_complement_only_an_existentials_successor_refutes_is_never_denied() {
    let (rules, dl) = both(PREMISE, &turtle(CONCLUSION));
    let mut wrong = Vec::new();
    if !matches!(
        rules,
        Ok(Entailment::Entailed) | Err(StoreError::Undecided(_))
    ) {
        wrong.push(format!("entails under owl2-dl: {rules:?}"));
    }
    if !matches!(dl, Entailed::Yes | Entailed::Unknown(_)) {
        wrong.push(format!("entails_dl: {dl:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec: without the range the conclusion is not entailed (module docs): neither check may
/// claim it.
#[test]
fn without_the_range_the_complement_is_not_claimed() {
    let (rules, dl) = both(CONTROL, &turtle(CONCLUSION));
    let mut wrong = Vec::new();
    if matches!(
        rules,
        Ok(Entailment::Entailed | Entailment::InconsistentPremise)
    ) {
        wrong.push(format!("entails under owl2-dl: {rules:?}"));
    }
    if dl == Entailed::Yes {
        wrong.push(format!("entails_dl: {dl:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
