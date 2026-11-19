//! The premise of theorem PR1 (OWL 2 Profiles §4.3), which the OWL 2 RL rules route rests
//! on: the graph is exactly the RDF mapping of the ontology the reader read. A name read as
//! two kinds against OWL 2 DL's typing constraints (OWL 2 Structural Specification §5.8.1),
//! read by an axiom of one of them, and a structure no axiom uses that the rules may read more
//! of than one structure says, break it: the rules then derive what OWL 2 DL doesn't entail,
//! and an answer drawn from them is never told sound. A valid ontology keeps the route.

use std::time::Duration;

use nrese_store::{DlConfig, MutationError, MutationPipeline, RejectKind};

use super::queries::query;
use super::{insert, pipeline, pipeline_with};

/// `data` committed under `owl2-dl` with `config`: the store, how its upper bound got to the
/// commit's revision (`rules` on the RL route), and the commit's consistency verdict.
fn committed_with(config: DlConfig, data: &str) -> (MutationPipeline, &'static str, String) {
    let dl = pipeline_with(config);
    insert(&dl, data).expect("the commit is admitted");
    let route = dl.store().dl_bounds().last;
    let verdict = dl.store().dl().status().expect("a DL status");
    (dl, route, verdict.consistency.verdict.as_str().to_owned())
}

fn committed(data: &str) -> (MutationPipeline, &'static str, String) {
    committed_with(DlConfig::default(), data)
}

/// Asserts that `ASK { pattern }` is answered `true`, which the rules derive and OWL 2 DL
/// doesn't entail, and isn't told sound; its status's reasons.
fn true_never_sound(dl: &MutationPipeline, pattern: &str) -> Vec<String> {
    let (answers, status) = query(dl, &format!("ASK {{ {pattern} }}"));
    assert_eq!(answers, ["true"], "{pattern}: the rules' closure holds it");
    assert!(
        !status.shared.sound,
        "{pattern}: OWL 2 DL doesn't entail it, and it is told sound: {:?}",
        status.reasons()
    );
    status.reasons()
}

/// Asserts that `ASK { pattern }` is answered `answer`, sound and complete.
fn exact(dl: &MutationPipeline, pattern: &str, answer: bool) {
    let (answers, status) = query(dl, &format!("ASK {{ {pattern} }}"));
    assert_eq!(answers, [answer.to_string()], "{pattern}");
    assert!(
        status.shared.sound && status.is_complete(),
        "{pattern}: sound and complete: {:?}",
        status.reasons()
    );
}

/// The witness F2: `:p`'s domain reads it as an object property, its literal as a data
/// property. prp-dom types `:a` by the literal's statement, which OWL 2 DL never reads with
/// the domain (§5.8.1 lets a name be one of the two only). The reading is no model of the
/// source: off the route, not sound, consistency unknown.
#[test]
fn a_property_used_with_literals_and_resources_is_two_kinds() {
    let (dl, route, verdict) =
        committed(":C a owl:Class . :p rdfs:domain :C . :a :p \"x\" . :b :p :c .");
    assert_ne!(route, "rules", "a name of two kinds leaves the rules route");
    assert_eq!(
        verdict, "unknown",
        "no model of the read ontology decides it"
    );
    true_never_sound(&dl, ":a a :C");
}

/// The witness (i): `:q` is declared an annotation property and given a characteristic of
/// an object property. prp-fp equates `:a`'s two annotation values, which OWL 2 DL never
/// reads as property values; a statement copied along that equality is no more entailed.
#[test]
fn a_functional_annotation_property_equates_nothing_soundly() {
    let (dl, route, _) = committed(
        ":q a owl:AnnotationProperty , owl:FunctionalProperty . :a :q :x , :y . :x a :C .",
    );
    assert_ne!(route, "rules", "a name of two kinds leaves the rules route");
    true_never_sound(&dl, ":x owl:sameAs :y");
    true_never_sound(&dl, ":y a :C");
}

/// The witness (k): an annotation property in a `hasValue` restriction. cls-hv2 reads the
/// annotation as the restriction's property value; OWL 2 DL doesn't.
#[test]
fn an_annotation_property_in_a_restriction_is_two_kinds() {
    let (dl, route, _) = committed(
        ":q a owl:AnnotationProperty . \
         :C owl:equivalentClass [ a owl:Restriction ; owl:onProperty :q ; owl:hasValue :b ] . \
         :a :q :b .",
    );
    assert_ne!(route, "rules", "a name of two kinds leaves the rules route");
    true_never_sound(&dl, ":a a :C");
}

/// The witness (l): a declared data property given IRIs. No data property assertion has an
/// IRI for its value (Mapping Table 16), so the reader leaves the statements out, named as
/// such; prp-fp still equates the IRIs.
#[test]
fn a_data_property_with_an_iri_value_is_left_out() {
    let (dl, route, _) =
        committed(":d a owl:DatatypeProperty , owl:FunctionalProperty . :a :d :x , :y .");
    assert_ne!(
        route, "rules",
        "a statement left out leaves the rules route"
    );
    let reasons = true_never_sound(&dl, ":x owl:sameAs :y");
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("an IRI where a data value belongs")),
        "the part left out is named: {reasons:?}"
    );
}

/// Case (b): one blank node with two structures' forms (an intersection and an `allValuesFrom`
/// restriction), which no axiom uses. OWL 2 DL reads neither, so `:y a :D` isn't entailed;
/// the rules type `:x` by the intersection (cls-int1) and apply the restriction (cls-avf).
/// The consistency stays decided: an unused structure says nothing.
const TWO_FORMS: &str = ":A a owl:Class . :B a owl:Class . :D a owl:Class . \
     :p a owl:ObjectProperty . \
     _:x owl:intersectionOf ( :A :B ) ; owl:onProperty :p ; owl:allValuesFrom :D . \
     :x a :A , :B . :x :p :y .";

#[test]
fn an_unused_structure_with_two_forms_leaves_the_rules_route() {
    let (dl, route, verdict) = committed(TWO_FORMS);
    assert_ne!(
        route, "rules",
        "an unused structure the rules read leaves the route"
    );
    assert_eq!(verdict, "consistent", "an unused structure says nothing");
    true_never_sound(&dl, ":y a :D");
}

/// Case (b) beside a union (off the route by the profile), where the upper bound proves the
/// data consistent: that proof says nothing of what the rules read beyond the ontology, so
/// a read decided on it (a) and one after a commit the upper bound took as a delta (b) are
/// told unsound. (The rewriting's own read is not covered here.)
#[test]
fn an_unused_structure_is_told_unsound_where_the_upper_bound_proves_consistency() {
    let data = format!("{TWO_FORMS} :E rdfs:subClassOf [ owl:unionOf ( :F :G ) ] .");
    let (dl, route, verdict) = committed(&data);
    let engine = dl
        .store()
        .dl()
        .status()
        .expect("a status")
        .consistency
        .engine;
    assert_eq!(
        (route, verdict.as_str(), engine),
        ("rebuilt", "consistent", "upper-bound"),
        "setup: the upper bound proves the data consistent"
    );
    true_never_sound(&dl, ":y a :D");
    insert(&dl, ":m a :A .").expect("admitted");
    assert_eq!(
        dl.store().dl_bounds().last,
        "delta",
        "setup: a delta commit"
    );
    true_never_sound(&dl, ":y a :D");
}

/// The negative property assertion's three statements without its type: OWL 2 DL reads no
/// axiom from them (an unused structure), while prp-npa1 reads them and clashes with
/// `:a :p :b`. The clash is the rules' alone: the commit is refused as undecided, never as
/// inconsistent.
#[test]
fn an_unused_negative_assertion_clash_is_refused_as_undecided() {
    let dl = pipeline();
    let refused = insert(
        &dl,
        "_:n owl:sourceIndividual :a ; owl:assertionProperty :p ; owl:targetIndividual :b . \
         :a :p :b .",
    );
    match refused {
        Err(MutationError::Rejected(reject)) => {
            assert_eq!(reject.kind, RejectKind::Undecided, "{}", reject.detail);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Two structures no axiom uses, each of one form, made equal as individuals: OWL 2 DL reads
/// neither, and equates two anonymous individuals; the rules copy the intersection's type
/// to the restriction (eq-rep-o) and apply it (cls-avf). Without the equality they derive
/// nothing.
#[test]
fn unused_structures_equated_as_individuals_leave_the_rules_route() {
    let data = "_:x owl:intersectionOf ( :A :B ) . _:z owl:onProperty :p ; owl:allValuesFrom :D . \
         :x a :A , :B . :x :p :y .";
    let (control, _, _) = committed(data);
    exact(&control, ":y a :D", false);
    let (dl, route, _) = committed(&format!("{data} _:x owl:sameAs _:z ."));
    assert_ne!(
        route, "rules",
        "an unused structure the rules read leaves the route"
    );
    true_never_sound(&dl, ":y a :D");
}

/// Off the route, with the upper bound given up (no time for it), a commit that only adds an
/// assertion keeps it as it was; `:a :p "x"` makes `:p`, which has a domain, a name of two
/// kinds. The record of that revision comes from the gate's own read of the data, never
/// from "nothing left out".
#[test]
fn a_kept_upper_bound_takes_the_premise_from_the_gates_read() {
    let config = DlConfig {
        timeout: Some(Duration::ZERO),
        ..DlConfig::default()
    };
    let (dl, _, _) = committed_with(
        config,
        ":C a owl:Class . :p rdfs:domain :C . :b :p :c . \
         :Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] .",
    );
    insert(&dl, ":a :p \"x\" .").expect("admitted");
    assert_eq!(dl.store().dl_bounds().last, "kept", "setup: the bound kept");
    true_never_sound(&dl, ":a a :C");
}

/// Neighbours of the reds that keep the route, each read sound and complete: a header
/// statement of an object property (read as an assertion, Mapping Table 10), a one-form
/// restriction no axiom uses, and a property given a literal and an IRI with no axiom on it
/// (a bare two-kinds use: only the rules for equality read it, which OWL 2 DL entails alike).
#[test]
fn valid_neighbours_keep_the_rules_route() {
    let cases = [
        (
            ":p a owl:ObjectProperty ; rdfs:domain :C . :ont a owl:Ontology ; :p :b .",
            ":ont a :C",
            true,
        ),
        (
            ":C rdfs:subClassOf :D . :a a :C . \
             _:r owl:onProperty :p ; owl:someValuesFrom :C .",
            ":a a :D",
            true,
        ),
        (
            ":a :creator \"N\" . :b :creator :n .",
            ":a :creator \"N\"",
            true,
        ),
    ];
    for (data, pattern, answer) in cases {
        let (dl, route, _) = committed(data);
        assert_eq!(route, "rules", "{data}: keeps the rules route");
        exact(&dl, pattern, answer);
    }
}
