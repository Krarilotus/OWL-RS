//! The counterexample of a terminological entailment is an unnamed individual.
//!
//! `C ⊑ D` is not entailed when some model has an element in `C` and outside `D`. That
//! element needn't be named. A key holds only for named individuals (*OWL 2 Direct
//! Semantics* §2.3.5, `HasKey`), so it can't merge such an element with a named one.

use nrese_store::dl::entailment::Entailed;

use super::entailment::entailed;

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
