//! Step 7's service: axiom entailment under OWL 2 DL (`StoreService::entails_dl`), on the
//! forms the OWL 2 RL rules can't show (the W3C cases' kinds), and their negatives.

use nrese_rdf::Triple;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_store::dl::entailment::Entailed;

use super::{PREFIXES, pipeline};

pub(super) fn turtle(text: &str) -> Vec<Triple> {
    let prefixes = PREFIXES
        .split("PREFIX ")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("@prefix {} .\n", p.trim()))
        .collect::<String>();
    RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(format!("{prefixes}{text}").as_bytes())
        .map(|quad| Triple::from(quad.expect("turtle")))
        .collect()
}

pub(super) fn entailed(premise: &str, conclusion: &str) -> Entailed {
    let dl = pipeline();
    super::insert(&dl, premise).expect("premise");
    dl.store()
        .entails_dl(&turtle(conclusion))
        .expect("entailment")
        .answer
}

#[test]
fn a_chain_of_a_property_with_itself_makes_it_transitive() {
    let premise = ":p a owl:ObjectProperty ; owl:propertyChainAxiom ( :p :p ) .";
    assert_eq!(
        entailed(premise, ":p a owl:TransitiveProperty ."),
        Entailed::Yes
    );
    assert_eq!(
        entailed(":p a owl:ObjectProperty .", ":p a owl:TransitiveProperty ."),
        Entailed::No
    );
}

#[test]
fn reflexivity_gives_a_self_loop() {
    let premise =
        ":knows a owl:ObjectProperty , owl:ReflexiveProperty . :peter a owl:NamedIndividual .";
    let conclusion = ":knows a owl:ObjectProperty . :peter :knows :peter .";
    assert_eq!(entailed(premise, conclusion), Entailed::Yes);
    let conclusion = ":knows a owl:ObjectProperty . :peter :knows :paul .";
    assert_eq!(entailed(premise, conclusion), Entailed::No);
}

#[test]
fn datatype_ranges_are_subsumed_and_intersected() {
    let byte = ":p a owl:DatatypeProperty ; rdfs:range xsd:byte .";
    let short = ":p a owl:DatatypeProperty ; rdfs:range xsd:short .";
    assert_eq!(entailed(byte, short), Entailed::Yes);
    assert_eq!(entailed(short, byte), Entailed::No);
    let both = ":p a owl:DatatypeProperty ; rdfs:range xsd:short , xsd:unsignedInt .";
    let conclusion = ":p a owl:DatatypeProperty ; rdfs:range xsd:unsignedShort .";
    assert_eq!(entailed(both, conclusion), Entailed::Yes);
}

#[test]
fn a_subsumption_through_a_union_and_an_instance_of_a_restriction() {
    let premise = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
                   :C rdfs:subClassOf :D . :x a :A .";
    assert_eq!(entailed(premise, ":A rdfs:subClassOf :D ."), Entailed::Yes);
    assert_eq!(entailed(premise, ":A rdfs:subClassOf :B ."), Entailed::No);
    assert_eq!(
        entailed(premise, ":x a [ owl:unionOf ( :B :C ) ] ."),
        Entailed::Yes
    );
    assert_eq!(entailed(premise, ":x a :D ."), Entailed::Yes);
    // A bare class expression holds no axiom: nothing to show.
    assert_eq!(
        entailed(premise, "[ a owl:Class ; owl:unionOf ( :B ) ] ."),
        Entailed::Yes
    );
}

/// A premise only the DL check finds inconsistent: the OWL 2 RL rules don't reach `:x`'s
/// disjointness through the union, so a store with that check off commits it.
pub(super) const INCONSISTENT: &str = ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :D . :C owl:disjointWith :D . :x a :A , :D .";

#[test]
fn an_inconsistent_premise_entails_everything() {
    let dl = super::pipeline_with(nrese_store::DlConfig {
        consistency: nrese_store::DlConsistency::Off,
        ..nrese_store::DlConfig::default()
    });
    super::insert(&dl, INCONSISTENT).expect("premise, unchecked");
    let answer = dl
        .store()
        .entails_dl(&turtle(":y a :Q ."))
        .expect("entailment");
    assert_eq!(answer.answer, Entailed::Yes);
    assert!(answer.holds());
    assert_eq!(answer.premise.as_str(), "inconsistent");
}

/// A store holding [`INCONSISTENT`], committed with the DL check off.
fn unchecked_inconsistent() -> nrese_store::MutationPipeline {
    let dl = super::pipeline_with(nrese_store::DlConfig {
        consistency: nrese_store::DlConsistency::Off,
        ..nrese_store::DlConfig::default()
    });
    super::insert(&dl, INCONSISTENT).expect("premise, unchecked");
    dl
}

/// A restriction with no filler: not well-formed OWL 2 DL, so the conclusion is never read.
const UNREAD: &str = ":y a [ a owl:Restriction ; owl:onProperty :r ] .";

/// Spec (`entails_dl`: a conclusion that isn't well-formed OWL 2 DL is unknown; an answer
/// only after the conclusion is admitted): over an inconsistent premise, [`UNREAD`] is
/// unknown and doesn't hold; the premise's verdict stays as evidence.
#[test]
fn an_unread_conclusion_over_an_inconsistent_premise_is_unknown() {
    let dl = unchecked_inconsistent();
    let answer = dl.store().entails_dl(&turtle(UNREAD)).expect("entailment");
    assert!(matches!(answer.answer, Entailed::Unknown(_)), "{answer:?}");
    assert!(!answer.holds());
    assert_eq!(answer.premise.as_str(), "inconsistent");
}

/// The rules the store under `owl2-dl` maintains for `dl`.
fn rules(dl: &nrese_store::MutationPipeline) -> nrese_reasoner::RuleProgram {
    dl.reasoner()
        .config()
        .materialised_program()
        .expect("rules")
}

/// Spec (`entails` under `owl2-dl`: the DL engines decide; where they don't, undecided):
/// over an inconsistent premise the rules don't find, [`UNREAD`] is undecided, not
/// `InconsistentPremise`.
#[test]
fn entails_reports_an_unread_conclusion_over_an_inconsistent_premise_as_undecided() {
    let dl = unchecked_inconsistent();
    let answer = dl.store().entails(rules(&dl), &turtle(UNREAD));
    assert!(
        matches!(answer, Err(nrese_store::StoreError::Undecided(_))),
        "{answer:?}"
    );
}

/// Spec (OWL 2 Direct Semantics: an inconsistent ontology entails every conclusion): over the
/// same premise, an admitted conclusion is `InconsistentPremise` through `entails`.
#[test]
fn entails_reports_an_admitted_conclusion_over_an_inconsistent_premise_as_inconsistent() {
    let dl = unchecked_inconsistent();
    let answer = dl.store().entails(rules(&dl), &turtle(":y a :Q ."));
    assert!(
        matches!(answer, Ok(nrese_store::Entailment::InconsistentPremise)),
        "{answer:?}"
    );
}

/// Spec (Mapping to RDF Table 10 reads only annotation properties on a header): `:p` is a
/// declared object property, so `:o :p :b` on the ontology node `:o` is an object-property
/// assertion, which the unrelated premise doesn't entail.
#[test]
fn a_declared_object_propertys_assertion_on_an_ontology_node_is_read() {
    let conclusion = ":o a owl:Ontology , owl:NamedIndividual . :b a owl:NamedIndividual . \
                      :p a owl:ObjectProperty . :o :p :b .";
    assert_eq!(entailed(":q a owl:Class .", conclusion), Entailed::No);
}

/// - Input: a premise with an anonymous `hasValue` filler, `A ⊑ ∃p.{_:b}` and `_:b` a
///   `B`, which OWL 2 DL forbids (*OWL 2 Structural Specification* §11.2) and the Direct
///   Semantics reads as one individual, existential over the premise (§2.3.7–2.4).
/// - Expected: `A ⊑ ∃p.B` entailed; `A ⊑ ∃p.C` not.
#[test]
fn a_premises_anonymous_filler_is_one_individual() {
    let premise = ":p a owl:ObjectProperty . _:b a :B . :A rdfs:subClassOf \
                   [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue _:b ] .";
    let some = |c: &str| {
        format!(
            ":p a owl:ObjectProperty . :A rdfs:subClassOf \
             [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom {c} ] ."
        )
    };
    assert_eq!(entailed(premise, &some(":B")), Entailed::Yes);
    assert_eq!(entailed(premise, &some(":C")), Entailed::No);
}

/// - Input: the premise `A ⊑ ∃p.{n}` and `x` an `A`; the conclusion `x : ∃p.{_:b}`.
/// - Expected: undecided. A conclusion's anonymous individual is an existential to prove
///   (it holds here, `_:b` being `n`); read as an individual of its own, as a premise's
///   is, it would ask for more than the conclusion says and be answered "not entailed".
#[test]
fn a_conclusions_anonymous_filler_leaves_it_undecided() {
    let premise = ":p a owl:ObjectProperty . :x a :A . :A rdfs:subClassOf \
                   [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue :n ] .";
    let conclusion = ":p a owl:ObjectProperty . \
                      :x a [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue _:b ] .";
    let answer = entailed(premise, conclusion);
    assert!(matches!(answer, Entailed::Unknown(_)), "{answer:?}");
}

/// - Input: conclusions `B(_:c)`, which say that some individual is a `B` (*OWL 2 Direct
///   Semantics* §2.3.7–2.4), over premises where an individual is one: `B(x)`, and a
///   premise's own anonymous filler `_:b` asserted `B`; and over `⊤ ⊑ B`.
/// - Expected: never "not entailed" over the first two (both entail it), "entailed" over
///   the last (every individual is a `B`).
#[test]
fn a_conclusions_anonymous_individual_is_some_individual() {
    let conclusion = "_:c a :B .";
    let filler = ":p a owl:ObjectProperty . _:b a :B . :x a :A . :A rdfs:subClassOf \
                  [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue _:b ] .";
    for premise in [":x a :B .", filler] {
        let answer = entailed(premise, conclusion);
        assert_ne!(answer, Entailed::No, "{premise}");
    }
    assert_eq!(
        entailed("owl:Thing rdfs:subClassOf :B .", conclusion),
        Entailed::Yes
    );
}

/// Spec (OWL 2 Direct Semantics: a declaration carries no meaning, an annotation property's
/// none either; premise and conclusion are each read on their own): a conclusion declaring
/// `:note` an annotation property, which the premise uses as an object property, is decided by
/// its other statement: `:x a :A`, which holds in every model of the premise, is entailed;
/// `:x a :B` is not.
#[test]
fn a_conclusions_annotation_declaration_of_a_premises_property_carries_no_meaning() {
    let premise = ":x :note :y . :x a :A .";
    let declared = ":note a owl:AnnotationProperty .";
    assert_eq!(
        entailed(premise, &format!("{declared} :x a :A .")),
        Entailed::Yes
    );
    assert_eq!(
        entailed(premise, &format!("{declared} :x a :B .")),
        Entailed::No
    );
}
