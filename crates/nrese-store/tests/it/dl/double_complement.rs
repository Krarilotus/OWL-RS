use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::{DlAnswers, DlConfig, MutationPipeline};

use super::{PREFIXES, Status, insert, pipeline, pipeline_with, queries::query};
use crate::support::load_past_the_pipeline;

const COMPLEMENTS: &str = ":A a owl:Class .
    _:b0 owl:complementOf _:b1 . _:b1 owl:complementOf :A .";
const TYPES: &str = "_:b0 a owl:Class . _:b1 a owl:Class .";
const MEMBER: &str = ":x a owl:NamedIndividual , _:b0 .";
const ASK_MEMBER: &str = "ASK { :x a :A }";

fn complete_membership(dl: &MutationPipeline, expected: &str) -> Status {
    let (answers, status) = query(dl, ASK_MEMBER);
    assert_eq!(
        (answers, status.is_complete()),
        (vec![expected.to_owned()], true),
        "{status:?}"
    );
    status
}

#[test]
fn a_double_complement_entails_membership_after_a_write() {
    let dl = pipeline();
    insert(&dl, &format!("{COMPLEMENTS} {MEMBER}")).unwrap();
    complete_membership(&dl, "true");
}

#[test]
fn a_double_complement_entails_membership_after_a_bulk_load() {
    let dl = pipeline();
    load_past_the_pipeline(dl.store(), &format!("{PREFIXES} {COMPLEMENTS} {MEMBER}"));
    let program = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    dl.store().rematerialise(program).unwrap();
    complete_membership(&dl, "true");
}

#[test]
fn typed_complements_also_entail_membership() {
    let dl = pipeline();
    insert(&dl, &format!("{COMPLEMENTS} {TYPES} {MEMBER}")).unwrap();
    complete_membership(&dl, "true");
}

#[test]
fn the_double_complement_membership_reaches_an_equal_individual() {
    let dl = pipeline();
    insert(
        &dl,
        &format!("{COMPLEMENTS} :y owl:sameAs :x . :y a owl:NamedIndividual , _:b0 ."),
    )
    .unwrap();
    complete_membership(&dl, "true");
}

#[test]
fn exact_reasoning_refutes_membership_in_one_disjunct() {
    let dl = pipeline();
    insert(
        &dl,
        ":A a owl:Class . :B a owl:Class .
         :x a owl:NamedIndividual , [ a owl:Class ; owl:unionOf ( :A :B ) ] .",
    )
    .unwrap();
    let status = complete_membership(&dl, "false");
    assert!(
        status.paths.contains(&"exact-ground-entailment"),
        "{status:?}"
    );
}

#[test]
fn a_single_complement_leaves_the_named_class_closed() {
    let dl = pipeline();
    insert(
        &dl,
        ":A a owl:Class . :x a owl:NamedIndividual , [ owl:complementOf :A ] .",
    )
    .unwrap();
    let status = complete_membership(&dl, "false");
    assert!(status.paths.contains(&"closed-predicates"), "{status:?}");
}

#[test]
fn a_membership_already_in_the_view_is_complete_in_sound_mode() {
    let dl = pipeline_with(DlConfig {
        answers: DlAnswers::Sound,
        ..DlConfig::default()
    });
    insert(&dl, &format!("{COMPLEMENTS} {MEMBER} :x a :A .")).unwrap();
    complete_membership(&dl, "true");
}

#[test]
fn different_individuals_are_things_when_the_upper_bound_is_needed() {
    let dl = pipeline();
    insert(
        &dl,
        &format!("{COMPLEMENTS} {TYPES} {MEMBER} :a owl:differentFrom :b ."),
    )
    .unwrap();
    let (answers, status) = query(&dl, "ASK { :a a owl:Thing }");
    assert_eq!(answers, ["true"], "{status:?}");
}

#[test]
fn the_nonempty_domain_answers_an_existential_thing_query() {
    let dl = pipeline();
    insert(
        &dl,
        &format!("{COMPLEMENTS} {TYPES} :C a owl:Class ; rdfs:subClassOf _:b0 ."),
    )
    .unwrap();
    let (answers, status) = query(&dl, "ASK { ?x a owl:Thing }");
    assert_eq!(answers, ["true"], "{status:?}");
}

#[test]
fn program_fact_typing_preserves_the_answer_for_distinct_individuals() {
    let dl = pipeline();
    insert(
        &dl,
        &format!("{COMPLEMENTS} {TYPES} {MEMBER} :a owl:differentFrom :b ."),
    )
    .unwrap();
    let (answers, status) = query(&dl, "ASK { :a owl:sameAs :b }");
    assert_eq!(answers, ["false"], "{status:?}");
}
