//! Step 1: the mode itself. Its lower bound is the OWL 2 RL closure, maintained and
//! checked on commit as under `owl2-rl`.

use nrese_reasoner::ReasoningMode;
use nrese_store::MutationError;

use super::{ask, insert, pipeline};

#[test]
fn the_mode_is_named_and_materialises_owl2_rl() {
    assert_eq!(
        ReasoningMode::from_name("owl2-dl"),
        Some(ReasoningMode::Owl2Dl)
    );
    assert_eq!(ReasoningMode::Owl2Dl.as_str(), "owl2-dl");
    assert!(ReasoningMode::Owl2Dl.is_dl());
    assert!(!ReasoningMode::Owl2Rl.is_dl());
    assert_eq!(
        ReasoningMode::Owl2Dl.ruleset(),
        ReasoningMode::Owl2Rl.ruleset()
    );
}

#[test]
fn a_dl_pipeline_switches_the_store_into_the_mode() {
    let pipeline = pipeline();
    assert!(pipeline.store().dl().active());
    insert(&pipeline, ":Dog rdfs:subClassOf :Animal . :rex a :Dog .").expect("commit");
    // L: the OWL 2 RL closure, in the inferred stack.
    assert!(ask(&pipeline, ":rex a :Animal"));
}

#[test]
fn rl_violations_reject_commits_in_the_mode() {
    let pipeline = pipeline();
    insert(&pipeline, ":A owl:disjointWith :B . :x a :A .").expect("setup");
    let rejected = insert(&pipeline, ":x a :B .");
    assert!(matches!(rejected, Err(MutationError::Rejected(_))));
    assert!(!ask(&pipeline, ":x a :B"));
}
