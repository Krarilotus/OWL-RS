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
