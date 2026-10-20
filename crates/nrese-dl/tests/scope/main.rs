//! The small-scope check in the gate (docs/design/owl2-dl.md §11): every OWL 2 DL ontology
//! of the scope `3,3` over `nrese_owl::scope`'s universe (35,505 cases: each one-axiom
//! ontology up to size 3, each two-axiom one up to total size 3, every constructor and
//! axiom type, symmetric ones once; the one-axiom ones are `../scope_one`'s, the commit
//! tier's, and this binary, the push tier's, has the rest), read as the references read it
//! and answered by
//! NRESE's consistency, classification and realisation (`nrese_dl::scope::answer`),
//! answers as the references do: `reference-3-3.txt`, written by
//! `benches/reasoning/dl/scope.py record` from HermiT's and Openllet's answers, the 51
//! cases they don't settle adjudicated by hand (`benches/reasoning/dl/scope-adjudicated.tsv`).
//!
//! It also counts the certificates: of the claims a model can show (consistency, a
//! satisfiable class, a non-entailment) in the ontologies without data values, every one
//! is shown by a checked model here (the claims without one are all one-axiom cases,
//! `../scope_one`'s allowance).
//!
//! The recorded campaign over the larger scope `4,4,4` (2,761,335 cases) is in the lab log
//! (docs/design/performance.md §6).

mod check;

#[test]
fn every_ontology_of_more_axioms_answers_as_the_references_do() {
    check::check(2..=usize::MAX, 0);
}
