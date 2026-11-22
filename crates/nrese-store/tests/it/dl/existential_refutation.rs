//! Negative class assertions that only an existential's successor refutes, under `owl2-dl`.
//!
//! `:C ⊑ ∃p.:A`, the range of `:p` is `:B`, and `:A` is disjoint with `:B`: a `:p`-successor
//! of a `:C` would be both `:A` and `:B`, so `:C` has no instance in any model and every
//! individual is in its complement (*OWL 2 Direct Semantics* §2.2–2.3). The premise is
//! consistent; `:a a [ owl:complementOf :C ]` is entailed. Without the range, `:C` is
//! satisfiable and the same conclusion is not entailed.
//!
//! The OWL 2 RL rules build no successor, so they can't refute `:a a :C` (OWL 2 Profiles
//! §4.3: they are complete under the Direct Semantics only for RL premises). Under `owl2-dl`
//! the store's entailment check (`StoreService::entails`) decides what they leave unshown
//! with the DL engines (`entails_dl`).

use nrese_reasoner::{ReasonerConfig, ReasoningMode, RuleProgram};
use nrese_store::dl::entailment::Entailed;

use super::entailment::turtle;
use super::{insert, pipeline};

const SOME_A: &str = "[ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :A ]";

/// (name, premise with `{some}` for `∃p.A`): each makes `:C` empty, so `:a a ¬C` follows.
const EMPTY_C: [(&str, &str); 5] = [
    (
        "range on the successor",
        ":p a owl:ObjectProperty ; rdfs:range :B . :A owl:disjointWith :B . \
         :C rdfs:subClassOf {some} . :a a :D .",
    ),
    (
        "allValuesFrom on the successor",
        ":p a owl:ObjectProperty . :A owl:disjointWith :B . :C rdfs:subClassOf {some} , \
         [ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :B ] . :a a :D .",
    ),
    (
        "a functional property's two successors",
        ":p a owl:ObjectProperty , owl:FunctionalProperty . :A owl:disjointWith :B . \
         :C rdfs:subClassOf {some} , \
         [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] . :a a :D .",
    ),
    (
        "at most no successor",
        ":p a owl:ObjectProperty . :C rdfs:subClassOf {some} , [ a owl:Restriction ; \
         owl:onProperty :p ; owl:maxCardinality \"0\"^^xsd:nonNegativeInteger ] . :a a :D .",
    ),
    (
        "a chain through the successor",
        ":p a owl:ObjectProperty . :q a owl:ObjectProperty . \
         :r a owl:ObjectProperty ; owl:propertyChainAxiom ( :p :q ) . :A owl:disjointWith :B . \
         :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom \
         [ a owl:Restriction ; owl:onProperty :q ; owl:someValuesFrom :A ] ] , \
         [ a owl:Restriction ; owl:onProperty :r ; owl:allValuesFrom :B ] . :a a :D .",
    ),
];

/// The first row without the range: `:C` is satisfiable.
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

/// Both entailment checks of a store under `owl2-dl` with `premise` committed, as text.
fn both(premise: &str) -> (String, Entailed) {
    let dl = pipeline();
    insert(&dl, premise).expect("the premise is consistent and commits");
    let store = dl.store();
    let conclusion = turtle(CONCLUSION);
    let rules = format!("{:?}", store.entails(rules(), &conclusion));
    let answer = store.entails_dl(&conclusion).expect("entailment");
    assert_eq!(answer.premise.as_str(), "consistent");
    (rules, answer.answer)
}

/// Spec (module docs): each premise entails the complement; both checks say so.
#[test]
fn a_complement_only_an_existentials_successor_refutes_is_entailed() {
    let mut wrong = Vec::new();
    for (name, premise) in EMPTY_C {
        let (rules, dl) = both(&premise.replace("{some}", SOME_A));
        if rules != "Ok(Entailed)" {
            wrong.push(format!("{name}: entails under owl2-dl: {rules}"));
        }
        if dl != Entailed::Yes {
            wrong.push(format!("{name}: entails_dl: {dl:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (`entailment` module docs: what the DL engines can't decide is undecided): with no
/// time for the DL check (the shortest timeout the store takes), a complement only it shows
/// is undecided, never denied; one the rules show (`cax-dw` on `:x a :A`, `:A` disjoint with
/// `:B`) is entailed as before.
#[test]
fn a_dl_check_out_of_time_leaves_the_answer_undecided() {
    let dl = super::pipeline_with(nrese_store::DlConfig {
        consistency: nrese_store::DlConsistency::Off,
        timeout: Some(std::time::Duration::from_nanos(1)),
        ..nrese_store::DlConfig::default()
    });
    let premise = EMPTY_C[0].1.replace("{some}", SOME_A);
    insert(&dl, &format!("{premise} :x a :A .")).expect("premise");
    let store = dl.store();
    let only_dl = store.entails(rules(), &turtle(CONCLUSION));
    assert!(
        matches!(only_dl, Err(nrese_store::StoreError::Undecided(_))),
        "{only_dl:?}"
    );
    let by_rules = store.entails(rules(), &turtle(":x a [ owl:complementOf :B ] ."));
    assert!(
        matches!(by_rules, Ok(nrese_store::Entailment::Entailed)),
        "{by_rules:?}"
    );
}

/// Spec (module docs): without the range the complement is not entailed; both checks say
/// so.
#[test]
fn without_the_range_the_complement_is_not_entailed() {
    let (rules, dl) = both(CONTROL);
    assert_eq!(
        (rules.as_str(), dl),
        ("Ok(NotEntailed)", Entailed::No),
        "entails under owl2-dl, entails_dl"
    );
}

/// Spec (module docs; *OWL 2 Mapping to RDF Graphs*: the complement's node carries a second
/// axiom, `¬C ⊑ ⊥`): `:a` is a `:B`, disjoint with `:C`, so `:a a ¬C` is entailed and `¬C`
/// has an instance; `¬C ⊑ ⊥` is not entailed, so neither is the conclusion. The rules'
/// refutation of `:a a :C` shows only its first half.
#[test]
fn a_complement_the_conclusion_also_says_is_empty_is_not_entailed() {
    let dl = pipeline();
    insert(&dl, ":a a :B . :B owl:disjointWith :C .").expect("premise");
    let conclusion =
        turtle(":a a _:n . _:n a owl:Class ; owl:complementOf :C ; rdfs:subClassOf owl:Nothing .");
    let by_store = format!("{:?}", dl.store().entails(rules(), &conclusion));
    let by_dl = dl
        .store()
        .entails_dl(&conclusion)
        .expect("entailment")
        .answer;
    assert_eq!(
        (by_store.as_str(), by_dl),
        ("Ok(NotEntailed)", Entailed::No),
        "entails under owl2-dl, entails_dl"
    );
}
