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

/// F2's consistency is unknown by its own cause: the engines read the name of two kinds as one
/// name. Nothing of the data was left out, and the reason doesn't say so.
#[test]
fn a_name_of_two_kinds_leaves_the_consistency_unknown_by_its_own_cause() {
    let (dl, _, verdict) =
        committed(":C a owl:Class . :p rdfs:domain :C . :a :p \"x\" . :b :p :c .");
    assert_eq!(verdict, "unknown");
    let status = dl.store().dl().status().expect("a DL status");
    let why = status
        .consistency
        .verdict
        .reason()
        .unwrap_or("none")
        .to_owned();
    assert!(
        why.contains("two kinds") && why.contains("one name") && !why.contains("left out"),
        "{why}"
    );
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
            assert_eq!(
                reject.kind,
                RejectKind::Undecided(nrese_store::UndecidedCause::ReadsDifferently),
                "{}",
                reject.detail
            );
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
        timeout: Some(Duration::from_nanos(1)),
        ..DlConfig::default()
    };
    let (dl, _, _) = committed_with(
        config,
        ":C a owl:Class . :p rdfs:domain :C . :b :p :c . \
         :Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] .",
    );
    insert(&dl, ":a :p \"x\" .").expect("admitted");
    assert_eq!(dl.store().dl_bounds().last, "kept", "setup: the bound kept");
    // The record is the gate's, of the commit's revision: without one a read would read the
    // ontology again and tell the same.
    let status = dl.store().dl().status().expect("a DL status");
    assert_eq!(
        status.revision,
        dl.store().current_revision(),
        "the commit's record"
    );
    let recorded = status.left_out.clone().flatten().unwrap_or_default();
    assert!(
        recorded.contains("two kinds"),
        "the record names the pair: {:?}",
        status.left_out
    );
    true_never_sound(&dl, ":a a :C");
}

/// A gate read of the committed ontology reports a tree with two named attachments
/// even when the upper bound is kept. One attachment satisfies the forest restriction.
#[test]
fn a_kept_upper_bound_reports_anonymous_findings_from_the_gates_read() {
    for (attachments, expected) in [(":y", false), (":y, :z", true)] {
        let config = DlConfig {
            timeout: Some(Duration::from_nanos(1)),
            ..DlConfig::default()
        };
        let (dl, _, _) = committed_with(
            config,
            &format!(
                ":C a owl:Class . :p rdfs:domain :C . :b :p :c .
                 :Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] .
                 :q a owl:ObjectProperty . :y a owl:NamedIndividual .
                 :z a owl:NamedIndividual . _:b :q {attachments} ."
            ),
        );
        let before = dl.store().current_revision();
        let revision = insert(&dl, ":a :p \"x\" .").expect("admitted");
        assert!(revision > before);
        assert_eq!(dl.store().dl_bounds().last, "kept");
        let status = dl.store().dl().status().expect("a DL status");
        assert_eq!(status.revision, revision);
        let recorded = status.left_out.flatten().expect("the gate's report");
        assert!(recorded.contains("two kinds"), "{recorded}");
        let reasons = true_never_sound(&dl, ":a a :C");
        let anonymous: Vec<_> = reasons
            .iter()
            .filter(|reason| reason.contains("anonymous"))
            .collect();
        if expected {
            assert!(
                anonymous
                    .iter()
                    .any(|reason| reason.contains("anonymous tree has no root")),
                "{attachments}: missing anonymous tree report in {reasons:?}"
            );
        } else {
            assert!(anonymous.is_empty(), "{attachments}: {anonymous:?}");
        }
    }
}

/// The treatment of each kind is a setting (`dl.treatments`): F2's name of two kinds taken
/// off the route instead of fatal is off the route all the same, its answer still isn't told
/// sound, and its consistency stays unknown, as no verdict of engines that read the name as
/// one speaks of the source: neither a model (F2) nor a clash (a cardinality the two kinds'
/// values together break, which the data property's values alone keep); case (b)'s unused
/// structure taken as fatal leaves it unknown.
#[test]
fn the_treatment_of_each_kind_is_a_setting() {
    use nrese_owl::{Strict, Treatments};
    let config = |treatments| DlConfig {
        treatments,
        ..DlConfig::default()
    };
    let off = Treatments {
        two_kinds_by_axiom: Strict::OffRoute,
        ..Treatments::default()
    };
    let (dl, route, verdict) = committed_with(
        config(off),
        ":C a owl:Class . :p rdfs:domain :C . :a :p \"x\" . :b :p :c .",
    );
    assert_eq!((route, verdict.as_str()), ("rebuilt", "unknown"));
    true_never_sound(&dl, ":a a :C");
    let two = "\"2\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>";
    let (_, route, verdict) = committed_with(
        config(off),
        &format!(
            ":C rdfs:subClassOf [ owl:onProperty :p ; owl:maxCardinality {two} ] . \
             :x a :C ; :p \"u\", \"v\", :v ."
        ),
    );
    assert_eq!((route, verdict.as_str()), ("rebuilt", "unknown"));
    let fatal = Treatments {
        unused_reads_more: Strict::Fatal,
        ..Treatments::default()
    };
    let (dl, route, verdict) = committed_with(config(fatal), TWO_FORMS);
    assert_eq!((route, verdict.as_str()), ("rebuilt", "unknown"));
    true_never_sound(&dl, ":y a :D");
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

/// A statement about the ontology's node is an annotation where its property is declared no
/// object or data property (Mapping Table 10, as the W3C cases read it): `:ont :p :b` added
/// beside `:p`'s domain makes `:p` a name of two kinds an axiom reads, which the rules apply
/// (prp-dom types `:ont`). Neither the rules' route nor the upper bound's delta keeps it: on
/// the route and off it (a union), the read isn't told sound.
#[test]
fn a_statement_about_the_ontology_added_later_is_read_as_the_reader_reads_it() {
    for extra in ["", ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] ."] {
        let (dl, _, _) = committed(&format!(
            ":ont a owl:Ontology . :p rdfs:domain :C . :a :p :b . {extra}"
        ));
        insert(&dl, ":ont :p :b .").expect("admitted");
        let route = dl.store().dl_bounds().last;
        assert_ne!((extra, route), ("", "rules"), "the rules route left");
        true_never_sound(&dl, ":ont a :C");
    }
}

/// A clash the rules find through a part the reader left out (a data property's IRI values,
/// which prp-fp equates against `owl:differentFrom`) is the rules' alone: the commit is refused
/// as undecided, never as inconsistent.
#[test]
fn a_clash_through_a_part_left_out_is_refused_as_undecided() {
    let refused = insert(
        &pipeline(),
        ":d a owl:DatatypeProperty , owl:FunctionalProperty . :a :d :x , :y . \
         :x owl:differentFrom :y .",
    );
    match refused {
        Err(MutationError::Rejected(reject)) => {
            assert_eq!(
                reject.kind,
                RejectKind::Undecided(nrese_store::UndecidedCause::ReadsDifferently),
                "{}",
                reject.detail
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A header statement's undeclared property is an annotation property there and an object
/// property in an assertion: a bare pair, counted. An equality of the ontology's node copies
/// the header's statement (eq-rep-s), which OWL 2 DL doesn't entail of an annotation: the
/// copy isn't told sound.
#[test]
fn a_copy_of_a_header_annotation_is_never_told_sound() {
    let (dl, _, _) =
        committed(":ont a owl:Ontology . :ont :note :b . :ont owl:sameAs :alias . :a :note :c .");
    true_never_sound(&dl, ":alias :note :b");
}

/// A header statement of a declared object property is an assertion (*Mapping to RDF* Table
/// 10), and the equality of the ontology's node copies it as OWL 2 DL does (SameIndividual):
/// the copy is entailed, and the copy check doesn't mark it.
#[test]
fn a_copy_of_a_declared_property_on_the_header_is_no_annotation_copy() {
    let (dl, _, _) =
        committed(":p a owl:ObjectProperty . :ont a owl:Ontology ; :p :b ; owl:sameAs :alias .");
    let (answers, status) = query(&dl, "ASK { :alias :p :b }");
    assert_eq!(answers, ["true"], "the copy is in the closure");
    let reasons = status.reasons();
    assert!(
        !reasons
            .iter()
            .any(|r| r.contains("copies of annotation statements")),
        "{reasons:?}"
    );
}

/// A read inside a transaction whose pending statement types a node `owl:Ontology` reads
/// `:ont :p :b` as an annotation, which makes `:p` a name of two kinds beside its domain:
/// the committed record ("nothing read otherwise") says nothing of the pending data, and
/// `:ont a :C`, which the closure keeps, isn't told sound.
#[test]
fn a_pending_read_takes_no_clean_record_from_its_base() {
    use nrese_store::StatementOp;
    let (dl, route, _) = committed(":p rdfs:domain :C . :ont :p :b .");
    assert_eq!(route, "rules", "setup: on the rules route");
    let header = StatementOp::Add {
        data: super::versions::ntriples(
            "<http://example.com/ont> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
             <http://www.w3.org/2002/07/owl#Ontology> .",
        ),
        contexts: Vec::new(),
    };
    let (text, status) = super::versions::in_transaction(&dl, vec![header], "ASK { :ont a :C }");
    assert!(text.contains("true"), "the closure holds it: {text}");
    let status = status.expect("a status");
    assert!(!status.sound, "told sound: {:?}", status.reasons);
}
