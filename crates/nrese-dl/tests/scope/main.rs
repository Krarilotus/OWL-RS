//! The small-scope check in the gate (docs/design/owl2-dl.md §11): every OWL 2 DL ontology
//! of the scope `3,3` over `nrese_owl::scope`'s universe (35,505 cases: each one-axiom
//! ontology up to size 3, each two-axiom one up to total size 3, every constructor and
//! axiom type, symmetric ones once), read as the references read it and answered by
//! NRESE's consistency, classification and realisation (`nrese_dl::scope::answer`),
//! answers as the references do: `reference-3-3.txt`, written by
//! `benches/reasoning/dl/scope.py record` from HermiT's and Openllet's answers, the 51
//! cases they don't settle adjudicated by hand (`benches/reasoning/dl/scope-adjudicated.tsv`).
//!
//! It also counts the certificates: of the claims a model can show (consistency, a
//! satisfiable class, a non-entailment) in the ontologies without data values, those no
//! checked model shows must stay at most [`UNCERTIFIED`] (the measured count: counting
//! through a nominal, where neither the fold nor the one-step unravelling is a model).
//!
//! The recorded campaign over the larger scope `4,4,4` (2,761,335 cases) is in the lab log
//! (docs/design/performance.md §6).

mod check;

/// Claims a model can show that no checked model shows, at most (measured 12 October
/// 2026: 88 of 140,448, all in one-axiom ontologies).
const UNCERTIFIED: usize = 88;

#[test]
fn every_small_ontology_answers_as_the_references_do() {
    check::check(usize::MAX, UNCERTIFIED);
}
