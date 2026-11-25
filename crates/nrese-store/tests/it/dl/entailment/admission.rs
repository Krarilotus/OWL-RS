use nrese_reasoner::RuleProgram;
use nrese_store::{
    DlConfig, DlConsistency, DlEntailment, Entailment, MutationPipeline, StoreError,
};

use super::super::{PREFIXES, insert, pipeline_with};
use super::{Entailed, INCONSISTENT, entailed, turtle};

const CLASH_DECLARATIONS: &str =
    ":A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class . :Q a owl:Class .";
const NAMED: &str = ":A a owl:Class . :a a :A .";
const DL_PREFIX: &str = "under OWL 2 DL: ";

fn rules(dl: &MutationPipeline) -> RuleProgram {
    dl.reasoner()
        .config()
        .materialised_program()
        .expect("rules")
}

fn unchecked(data: &str) -> MutationPipeline {
    let dl = pipeline_with(DlConfig {
        consistency: DlConsistency::Off,
        ..DlConfig::default()
    });
    if !data.is_empty() {
        insert(&dl, data).expect("premise");
    }
    dl
}

fn preloaded(data: &str) -> MutationPipeline {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("premise.ttl");
    let prefixes = PREFIXES
        .split("PREFIX ")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("@prefix {} .\n", p.trim()))
        .collect::<String>();
    std::fs::write(
        &file,
        format!("@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n{prefixes}{data}"),
    )
    .expect("write premise");
    let store = std::sync::Arc::new(
        nrese_store::StoreService::new(nrese_store::StoreConfig {
            dl: DlConfig {
                consistency: DlConsistency::Off,
                ..DlConfig::default()
            },
            ontology_path: Some(file),
            ..nrese_store::StoreConfig::in_memory()
        })
        .expect("store"),
    );
    let reasoner = std::sync::Arc::new(nrese_reasoner::ReasonerService::new(
        nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Dl),
    ));
    let dl = MutationPipeline::new(store, reasoner);
    dl.store()
        .rematerialise(rules(&dl))
        .expect("rematerialisation");
    dl
}

fn check_encodings(
    dl: &MutationPipeline,
    conclusion: &str,
    expected: &str,
    inconsistent_premise: bool,
) -> Result<DlEntailment, String> {
    let triples = turtle(conclusion);
    let result = dl.store().entails_dl(&triples).expect("entails_dl");
    let third = dl.store().entails(rules(dl), &triples);
    if result.answer.as_str() != expected {
        return Err(format!("expected {expected}, got {result:?}"));
    }
    let expected_holds = expected == "entailed";
    if result.holds() != expected_holds {
        return Err(format!("holds mismatch: {result:?}"));
    }
    let third_ok = match expected {
        "entailed" if inconsistent_premise => {
            matches!(third, Ok(Entailment::InconsistentPremise))
        }
        "entailed" => matches!(third, Ok(Entailment::Entailed)),
        "not-entailed" => matches!(third, Ok(Entailment::NotEntailed)),
        "unknown" => {
            matches!(&third, Err(StoreError::Undecided(why)) if why.starts_with(DL_PREFIX))
        }
        _ => unreachable!(),
    };
    if !third_ok {
        return Err(format!("third encoding mismatch for {expected}: {third:?}"));
    }
    Ok(result)
}

/// Structural Specification §§5.8.1 and 11: the union must be in OWL 2 DL.
/// A transitive functional property, incompatible property declarations, and
/// a pair of chains requiring both s < u and u < s withhold the answer.
#[test]
fn a_conclusion_breaking_joint_membership_is_unknown() {
    let chain = ":s a owl:ObjectProperty . :t a owl:ObjectProperty . :u a owl:ObjectProperty .";
    let clash = format!("{CLASH_DECLARATIONS} {INCONSISTENT} {chain}");
    let first = ":u owl:propertyChainAxiom ( :s :t ) .";
    let second = ":s owl:propertyChainAxiom ( :u :t ) .";
    let cases = [
        (
            "non-simple functional property",
            ":p a owl:ObjectProperty, owl:FunctionalProperty . :a :p :b .".to_owned(),
            ":p a owl:TransitiveProperty .",
            "unknown",
            false,
        ),
        (
            "property declaration clash",
            ":p a owl:ObjectProperty . :a :p :b .".to_owned(),
            ":p a owl:DatatypeProperty .",
            "unknown",
            false,
        ),
        (
            "joint cycle",
            format!("{clash} {first}"),
            second,
            "unknown",
            true,
        ),
        (
            "premise chain alone",
            format!("{clash} {first}"),
            ":y a :Q .",
            "entailed",
            true,
        ),
        ("conclusion chain alone", clash, second, "entailed", true),
    ];
    let mut wrong = Vec::new();
    for (case, data, conclusion, expected, inconsistent) in cases {
        let dl = unchecked(&data);
        match check_encodings(&dl, conclusion, expected, inconsistent) {
            Ok(result) => {
                if inconsistent && result.premise.as_str() != "inconsistent" {
                    wrong.push(format!(
                        "{case}: expected inconsistent premise, got {result:?}"
                    ));
                }
            }
            Err(err) => wrong.push(format!("{case}: {err}")),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

const DEFINITION: &str = ":dt a rdfs:Datatype ; owl:equivalentClass xsd:integer .";
const USE_DEFINITION: &str = "<urn:test:c> a owl:Ontology ; owl:imports <urn:test:y> .
                              :d a owl:DatatypeProperty ; rdfs:range :dt .";

/// Mapping to RDF Table 12, Structural Specification §§4 and 11.2: unread
/// arity, ill-typed literals, anonymous equality in a conclusion, and an
/// undefined datatype all prevent vacuous entailment. An admitted conclusion
/// can use a datatype from the import closure, including one copy per axiom.
#[test]
fn an_inadmissible_conclusion_over_an_inconsistent_premise_is_unknown() {
    let base = format!("{CLASH_DECLARATIONS} {INCONSISTENT}");
    let imports = format!("{base} <urn:test:y> a owl:Ontology . {DEFINITION}");
    let repeated = format!("{USE_DEFINITION} {DEFINITION}");
    let cases = [
        (
            &base,
            ":d a owl:DatatypeProperty ; rdfs:range [
             a rdfs:Datatype ; owl:intersectionOf ( xsd:integer ) ] .",
            false,
        ),
        (
            &base,
            ":d a owl:DatatypeProperty . :a :d \"abc\"^^xsd:integer .",
            false,
        ),
        (&base, "_:b owl:sameAs :a .", false),
        (&base, ":dt a rdfs:Datatype .", false),
        (
            &base,
            ":d a owl:DatatypeProperty ; rdfs:range [ a rdfs:Datatype ; owl:oneOf ( 1 ) ] .",
            true,
        ),
        (&base, "xsd:integer a rdfs:Datatype .", true),
        (&imports, USE_DEFINITION, true),
        (&imports, repeated.as_str(), true),
    ];
    let mut wrong = Vec::new();
    for (data, conclusion, admitted) in cases {
        let dl = unchecked(data);
        let expected = if admitted { "entailed" } else { "unknown" };
        match check_encodings(&dl, conclusion, expected, true) {
            Ok(result) => {
                if result.premise.as_str() != "inconsistent" {
                    wrong.push(format!("{conclusion}: premise {:?}", result.premise));
                }
            }
            Err(err) => wrong.push(format!("{conclusion}: {err}")),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Structural Specification §§5.6.2 and 11.2: conclusion blanks are fresh
/// individuals and repeats within a document name the same individual. Two
/// named attachments on one conclusion blank fail admission, whereas two
/// one-attachment trees are admissible. Premise-only findings allow a word.
#[test]
fn a_conclusions_blank_node_is_its_own() {
    let properties = ":p a owl:ObjectProperty . :q a owl:ObjectProperty .";
    let one = |label| format!("{properties} _:{label} :p :x . :z :p :y .");
    let collision = one("nresec0xb");
    let control = one("q0");
    let clash = format!("{CLASH_DECLARATIONS} {INCONSISTENT} {properties}");
    let with_collision = format!("{CLASH_DECLARATIONS} {INCONSISTENT} {collision}");
    let premise_only = ":addr a owl:ObjectProperty . :city a owl:ObjectProperty . :p :addr _:a .
         _:a :city :berlin . :A a owl:Class . :i a :A .";
    let cases = [
        (collision.as_str(), "_:b :p :x .", "unknown", false),
        (&control, "_:b :p :x .", "unknown", false),
        (&clash, "_:b :p :x . _:b :q :y .", "unknown", true),
        (&clash, "_:b1 :p :x . _:b2 :q :y .", "entailed", true),
        (&with_collision, "_:b :p :y .", "entailed", true),
        (premise_only, ":i a :A .", "entailed", false),
    ];
    let mut wrong = Vec::new();
    for (data, conclusion, expected, inconsistent) in cases {
        let dl = preloaded(data);
        if let Err(err) = check_encodings(&dl, conclusion, expected, inconsistent) {
            wrong.push(format!("{conclusion} over {data}: {err}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Admission of a conclusion with a shared data-range DAG must return within
/// the watchdog even when the request deadline withholds a decision.
#[test]
fn admitting_a_shared_expression_dag_stays_within_the_deadline() {
    use std::fmt::Write;
    use std::sync::mpsc;
    use std::time::Duration;

    let mut range = String::from(":d a owl:DatatypeProperty ; rdfs:range _:r32 .");
    let mut prev = "xsd:integer".to_owned();
    for i in 1..=32 {
        write!(
            range,
            " _:r{i} a rdfs:Datatype ; owl:intersectionOf ( {prev} _:n{i} ) .
              _:n{i} a rdfs:Datatype ; owl:datatypeComplementOf {prev} ."
        )
        .unwrap();
        prev = format!("_:r{i}");
    }
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let dl = pipeline_with(DlConfig {
            timeout: Some(Duration::from_secs(1)),
            ..DlConfig::default()
        });
        insert(&dl, NAMED).expect("premise");
        let result = dl.store().entails_dl(&turtle(&range)).expect("entails_dl");
        send.send(result).ok();
    });
    let result = receive
        .recv_timeout(Duration::from_secs(20))
        .expect("range DAG answer within the watchdog");
    worker.join().unwrap();
    assert!(
        !matches!(&result.answer, Entailed::Unknown(why) if !why.contains("time") && !why.contains("dl.timeout")),
        "{result:?}"
    );
}

/// Structural Specification §3.1: an out-of-position reserved name in the
/// asserted ontology withholds vacuous entailment even if its axiom was
/// filtered out of the premise used by the reasoner.
#[test]
fn a_premise_finding_dropped_by_the_filter_still_blocks() {
    let clash = format!("{CLASH_DECLARATIONS} {INCONSISTENT}");
    let bad = format!("{clash} :p a owl:ObjectProperty . :i :p rdf:type .");
    let dl_bad = preloaded(&bad);
    let result = check_encodings(&dl_bad, ":y a :Q .", "unknown", true).expect("reserved blocks");
    assert!(
        matches!(
            &result.answer,
            Entailed::Unknown(why)
                if why.contains("reserved") && why.contains("22-rdf-syntax-ns#type")
        ),
        "{result:?}"
    );
    assert_eq!(result.premise.as_str(), "inconsistent");
    let dl_ok = unchecked(&clash);
    check_encodings(&dl_ok, ":y a :Q .", "entailed", true).expect("control");
}

/// Structural Specification §11.2 counts axioms as a set, so adding fresh
/// declarations cannot cause a repeated definition to count twice. Direct
/// Semantics tests a conclusion against the premise alone.
#[test]
fn the_joint_union_keeps_premise_and_conclusion_apart() {
    let dl = unchecked(&format!(
        "{CLASH_DECLARATIONS} {INCONSISTENT} <urn:test:y> a owl:Ontology . {DEFINITION}"
    ));
    let duplicate = format!("{USE_DEFINITION} {DEFINITION}");
    let declarations: String = (0..128)
        .map(|i| format!(":fresh{i} a owl:Class . "))
        .collect();
    for conclusion in [format!("{declarations}{duplicate}"), duplicate] {
        let result = check_encodings(&dl, &conclusion, "entailed", true).expect("set union");
        assert_eq!(result.premise.as_str(), "inconsistent");
    }
    let simple = unchecked(":A a owl:Class . :B a owl:Class . :a a :A . :b a :A .");
    check_encodings(&simple, ":A rdfs:subClassOf :B .", "not-entailed", false)
        .expect("premise kept intact");
    assert_eq!(
        entailed(
            ":A a owl:Class . :B a owl:Class . :a a :A . :b a :A .",
            ":A rdfs:subClassOf :B .",
        ),
        Entailed::No,
    );
}

/// Spec (Structural Specification §11.2, Table 5): `owl:topDataProperty` is forbidden
/// outside the superproperty position of `SubDataPropertyOf`, including on a cold store
/// before any write or rematerialisation; a plain class declaration is admissible.
#[test]
fn joint_membership_uses_builtin_names_on_a_cold_store() {
    let bad = turtle("owl:topDataProperty a owl:DatatypeProperty .");
    let store = nrese_store::StoreService::new(nrese_store::StoreConfig::in_memory()).unwrap();
    assert_eq!(store.current_revision(), 0);
    assert_eq!(store.engine_stats().quads, 0);
    let cold = store.entails_dl(&bad).unwrap();
    assert!(
        matches!(&cold.answer, Entailed::Unknown(_)),
        "cold store must reject owl:topDataProperty declaration: {cold:?}"
    );
    assert!(!cold.holds());

    // Warm repeat: system vocabulary is now interned in the store's snapshot.
    let warm = store.entails_dl(&bad).unwrap();
    assert!(
        matches!(&warm.answer, Entailed::Unknown(_)),
        "warm store must reject owl:topDataProperty declaration: {warm:?}"
    );
    assert!(!warm.holds());

    // Positive control on a fresh cold store: a plain class declaration is admitted.
    let fresh = nrese_store::StoreService::new(nrese_store::StoreConfig::in_memory()).unwrap();
    let control = fresh.entails_dl(&turtle(":C a owl:Class .")).unwrap();
    assert_eq!(control.answer, Entailed::Yes, "control: {control:?}");
    assert!(control.holds());
}

/// Spec (Structural Specification §§11.1–11.2): `owl:bottomObjectProperty` is non-simple,
/// so declaring it functional lies outside OWL 2 DL on both cold and warm requests;
/// declaring it an object property is admissible.
#[test]
fn joint_membership_keeps_builtins_on_the_first_empty_store_request() {
    let store = nrese_store::StoreService::new(nrese_store::StoreConfig::in_memory()).unwrap();
    let bad = turtle("owl:bottomObjectProperty a owl:FunctionalProperty .");
    let cold = store.entails_dl(&bad).unwrap();
    let warm = store.entails_dl(&bad).unwrap();
    assert!(
        matches!(&cold.answer, Entailed::Unknown(_)),
        "cold: {cold:?}"
    );
    assert!(
        matches!(&warm.answer, Entailed::Unknown(_)),
        "warm: {warm:?}"
    );
    assert!(!cold.holds());
    assert!(!warm.holds());

    // Positive control on a fresh cold store: object-property declaration is admitted.
    let fresh = nrese_store::StoreService::new(nrese_store::StoreConfig::in_memory()).unwrap();
    let ok = turtle("owl:bottomObjectProperty a owl:ObjectProperty .");
    let cold_ok = fresh.entails_dl(&ok).unwrap();
    let warm_ok = fresh.entails_dl(&ok).unwrap();
    assert_eq!(cold_ok.answer, Entailed::Yes, "cold control: {cold_ok:?}");
    assert_eq!(warm_ok.answer, Entailed::Yes, "warm control: {warm_ok:?}");
    assert!(cold_ok.holds());
    assert!(warm_ok.holds());
}

/// Spec (OWL 2 Structural Specification; RDF 1.2 triple terms): OWL 2 defines no reading
/// for RDF triple terms, so a conclusion carrying a triple term (even wrapping a premise
/// blank label) is answered Unknown before renaming or interning; a string label is admitted.
#[test]
fn a_conclusion_triple_term_cannot_reuse_a_premise_blank() {
    use nrese_rdf::{BlankNode, NamedNode, Term, Triple};

    let dl = preloaded(&format!(
        "{CLASH_DECLARATIONS} {INCONSISTENT} :p a owl:ObjectProperty . _:b :p :x ."
    ));
    let nested = Triple::new(
        BlankNode::new_unchecked("b"),
        NamedNode::new_unchecked("http://example.com/p"),
        NamedNode::new_unchecked("http://example.com/x"),
    );
    let conclusion = [Triple::new(
        NamedNode::new_unchecked("http://example.com/a"),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Term::Triple(Box::new(nested)),
    )];
    let result = dl.store().entails_dl(&conclusion).expect("entails_dl");
    assert!(
        matches!(&result.answer, Entailed::Unknown(why) if why.contains("triple term")),
        "triple term must answer typed Unknown: {result:?}"
    );
    assert!(!result.holds());
    assert_eq!(result.premise.as_str(), "inconsistent");
    let third = dl.store().entails(rules(&dl), &conclusion);
    assert!(
        matches!(&third, Err(StoreError::Undecided(why)) if why.starts_with(DL_PREFIX)),
        "third encoding must be Undecided: {third:?}"
    );

    // Control: an ordinary string label over the same inconsistent premise is admitted.
    let ok = check_encodings(&dl, ":a rdfs:label \"plain\" .", "entailed", true)
        .expect("string label control");
    assert_eq!(ok.premise.as_str(), "inconsistent");
}

/// Spec (Structural Specification §11.2): a conclusion using an anonymous individual in a
/// `hasValue` restriction withholds vacuous entailment over an inconsistent premise, while
/// a named filler or a premise-only anonymous filler is admitted.
#[test]
fn a_conclusions_anonymous_has_value_over_an_inconsistent_premise_is_unknown() {
    let clash = format!("{CLASH_DECLARATIONS} {INCONSISTENT}");
    let dl = unchecked(&clash);
    let anon = ":p a owl:ObjectProperty . :x a [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue _:b ] .";
    let named = ":p a owl:ObjectProperty . :x a [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue :n ] .";

    let blocked = check_encodings(&dl, anon, "unknown", true).expect("anonymous hasValue");
    assert_eq!(blocked.premise.as_str(), "inconsistent");
    let control = check_encodings(&dl, named, "entailed", true).expect("named filler control");
    assert_eq!(control.premise.as_str(), "inconsistent");

    // Premise-only anonymous hasValue over the inconsistent premise still admits a named conclusion.
    let dl_premise_anon = unchecked(&format!(
        "{clash} :p a owl:ObjectProperty . :z a [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue _:p ] ."
    ));
    let allowed = check_encodings(&dl_premise_anon, ":y a :Q .", "entailed", true)
        .expect("premise-only anonymous hasValue control");
    assert_eq!(allowed.premise.as_str(), "inconsistent");
}
