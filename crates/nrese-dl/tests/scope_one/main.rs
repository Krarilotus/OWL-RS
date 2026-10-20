//! The commit tier's part of the small-scope check (`tests/scope`, the push tier's, has the
//! rest): the 8,578 one-axiom ontologies of the scope `3,3`, each answered as the
//! references do, and the claims a model can show that no checked model shows counted:
//! at most [`UNCERTIFIED`] (counting through a nominal, where neither the fold nor the
//! one-step unravelling is a model).

#[path = "../scope/check.rs"]
mod check;

/// Measured 12 October 2026: 88 claims of the one-axiom cases (of 140,448 in the scope)
/// have no checked model.
const UNCERTIFIED: usize = 88;

#[test]
fn every_one_axiom_ontology_answers_as_the_references_do() {
    check::check(0..=1, UNCERTIFIED);
}
