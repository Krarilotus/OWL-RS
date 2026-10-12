//! The commit tier's part of the small-scope check (`tests/scope`, the push tier's): the
//! 8,578 one-axiom ontologies of the scope `3,3`, each answered as the references do.

#[path = "../scope/check.rs"]
mod check;

/// Measured 12 October 2026: 88 claims of the one-axiom cases have no checked model.
const UNCERTIFIED: usize = 88;

#[test]
fn every_one_axiom_ontology_answers_as_the_references_do() {
    check::check(1, UNCERTIFIED);
}
