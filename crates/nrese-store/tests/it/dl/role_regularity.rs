use nrese_store::dl::{Verdict, entailment::Entailed};

use super::entailment::turtle;
use super::queries::query;
use super::{insert, pipeline};

const INVERSE_CHAIN: &str = ":p a owl:ObjectProperty . :q a owl:ObjectProperty .
    [ owl:inverseOf :p ] owl:propertyChainAxiom ( :p :q ) .
    :a a owl:NamedIndividual . :b a owl:NamedIndividual . :c a owl:NamedIndividual .
    :d a owl:NamedIndividual . :e a owl:NamedIndividual . :f a owl:NamedIndividual .";
const RESTRICTION: &str = ":C rdfs:subClassOf [
    a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :D ] .";
const CLASSES: &str = ":C a owl:Class . :D a owl:Class .";
const DISJUNCTION: &str = ":E a owl:Class .
    :C rdfs:subClassOf [ a owl:Class ; owl:unionOf ( :D :E ) ] .";

/// By OWL 2 Direct Semantics, x p y and y q z entail z p x here. Repeated uses
/// of that inclusion and C ⊑ ∀p.D entail the stated class assertions at each depth.
/// A consistency proof from the rules still stands.
#[test]
fn nested_inverse_chain_entailments_are_never_denied_and_rules_can_certify_consistency() {
    let depths = [
        (1, ":c a :C .", ":a a :D ."),
        (2, ":a :q :d . :d a :C .", ":c a :D ."),
        (3, ":a :q :d . :c :q :e . :e a :C .", ":d a :D ."),
        (4, ":a :q :d . :c :q :e . :d :q :f . :f a :C .", ":e a :D ."),
    ];
    let mut wrong = Vec::new();
    for (depth, data, conclusion) in depths {
        let dl = pipeline();
        let premise =
            format!("{INVERSE_CHAIN} {CLASSES} {RESTRICTION} :a :p :b . :b :q :c . {data}");
        insert(&dl, &premise).expect("premise committed");
        let store = dl.store();
        let answer = store
            .entails_dl(&turtle(conclusion))
            .expect("entailment")
            .answer;
        if !matches!(answer, Entailed::Yes | Entailed::Unknown(_)) {
            wrong.push(format!("depth {depth}: entailment {answer:?}"));
        }
        let verdict = store
            .dl()
            .status()
            .expect("consistency status")
            .consistency
            .verdict;
        if verdict != Verdict::Consistent {
            wrong.push(format!("depth {depth}: consistency {verdict:?}"));
        }
        eprintln!("depth {depth}: entailment {answer:?}, consistency {verdict:?}");
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The regularity design requires an engine consistency judgment that needs the
/// non-regular automaton to be unknown. The unrelated choice of D or E forces a DL check.
#[test]
fn engine_consistency_depends_on_the_regularity_of_the_automaton_it_uses() {
    let dl = pipeline();
    insert(
        &dl,
        &format!(
            "{INVERSE_CHAIN} {CLASSES} {RESTRICTION} {DISJUNCTION}
                  :a :p :b . :b :q :c . :a :q :d . :d a :C ."
        ),
    )
    .expect("premise committed");
    let verdict = dl
        .store()
        .dl()
        .status()
        .expect("consistency status")
        .consistency
        .verdict;
    assert!(matches!(verdict, Verdict::Unknown(_)), "{verdict:?}");
}

/// Without C ⊑ ∀p.D, a model can put a p-successor of a outside D. The probe can
/// witness this with ∃p.¬D, so its definite negative needs no role automaton.
#[test]
fn a_non_entailment_whose_probe_needs_no_automaton_stays_definite() {
    let dl = pipeline();
    insert(
        &dl,
        &format!("{INVERSE_CHAIN} {CLASSES} :a :p :b . :b :q :c . :a :q :d . :d a :C ."),
    )
    .expect("premise committed");
    let answer = dl
        .store()
        .entails_dl(&turtle(
            ":a a [ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :D ] .",
        ))
        .expect("entailment")
        .answer;
    assert_eq!(answer, Entailed::No);
}

/// OWL 2 Structural Specification §11.2 requires a strict order for property chains.
/// An impossible order is reported and lowers the commit's consistency verdict and
/// a read's completeness, as a non-simple property in a cardinality restriction does.
/// A hierarchy satisfying the structural order has no such diagnostic, even if the
/// engine can't use one of its automata to decide consistency.
#[test]
fn structural_regularity_violations_are_reported_and_lower_consistency_and_completeness() {
    let two_chains = ":u owl:propertyChainAxiom ( :f :b ) .
                      :b owl:propertyChainAxiom ( :c :u ) .";
    let chain_into_b = ":b owl:propertyChainAxiom ( :a :f ) .";
    let wide = ":p owl:propertyChainAxiom ( :p :q ) .";
    let cases = [
        ("two chains", two_chains.to_owned(), true, "irregular"),
        (
            "two chains with a universal",
            format!(
                "{two_chains} :C rdfs:subClassOf [
          a owl:Restriction ; owl:onProperty :b ; owl:allValuesFrom :D ] ."
            ),
            true,
            "irregular",
        ),
        (
            "inverse hierarchy link",
            format!("{chain_into_b} :b rdfs:subPropertyOf [ owl:inverseOf :a ] ."),
            true,
            "irregular",
        ),
        (
            "named hierarchy link",
            format!("{chain_into_b} :b rdfs:subPropertyOf :a ."),
            true,
            "irregular",
        ),
        (
            "inverse before a recursive end",
            ":p owl:propertyChainAxiom ( [ owl:inverseOf :p ] :p ) .".to_owned(),
            true,
            "irregular",
        ),
        (
            "regular recursive end",
            format!("{wide} {RESTRICTION}"),
            false,
            "",
        ),
        (
            "cycles through single inclusions",
            format!(
                "{chain_into_b} :b rdfs:subPropertyOf :c .
            :d owl:propertyChainAxiom ( :c :g ) . :d rdfs:subPropertyOf :a .
            :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :b ; owl:allValuesFrom :D ] .
            {DISJUNCTION}"
            ),
            true,
            "",
        ),
        (
            "regular inverse target",
            format!("{INVERSE_CHAIN} {RESTRICTION} {DISJUNCTION}"),
            true,
            "",
        ),
        (
            "non-simple cardinality",
            ":p a owl:TransitiveProperty .
            :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
            owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] ."
                .to_owned(),
            true,
            "a cardinality restriction",
        ),
    ];
    let names: String = ["a", "b", "c", "d", "f", "g", "p", "q", "u"]
        .into_iter()
        .map(|p| format!(":{p} a owl:ObjectProperty . "))
        .collect();
    let mut wrong = Vec::new();
    for (name, data, unknown, diagnostic) in cases {
        let dl = pipeline();
        insert(
            &dl,
            &format!(
                "{CLASSES} {names} {data} :x a owl:NamedIndividual, :C .
                              :y a owl:NamedIndividual . :x :p :y ."
            ),
        )
        .expect("premise committed");
        let verdict = dl
            .store()
            .dl()
            .status()
            .expect("consistency status")
            .consistency
            .verdict;
        let (_, status) = query(&dl, "ASK { :x :p :y }");
        if matches!(verdict, Verdict::Unknown(_)) != unknown {
            wrong.push(format!(
                "{name}: consistency {verdict:?}, expected unknown={unknown}"
            ));
        }
        if status.is_complete() == unknown {
            wrong.push(format!(
                "{name}: read completeness {:?}, expected complete={}",
                status.shared, !unknown
            ));
        }
        let reason = verdict.reason().unwrap_or_default();
        let reported =
            reason.contains("left out") && reason.contains("irregular") && reason.contains(" < ");
        let expected = diagnostic == "irregular";
        if reported != expected
            || (diagnostic == "a cardinality restriction" && !reason.contains(diagnostic))
        {
            wrong.push(format!(
                "{name}: diagnostic {reason:?}, expected {diagnostic:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
