//! The W3C OWL 2 test cases of the RL profile under the RDF-based semantics, on the store
//! with the OWL 2 RL ruleset (completion plan 1.6, the suite's `w3c-owl2-rl` workload).
//!
//! - The test cases: `.cache/owl-test/all.rdf` (scripts/fetch-w3c-tests.sh), or the file
//!   `NRESE_W3C_OWL_TESTS` points to. Without it the test is skipped, unless
//!   `NRESE_W3C_REQUIRED` is set (as in CI).
//! - Each test's premise (its RDF/XML premise ontology) is bulk loaded into a fresh
//!   in-memory store and materialised. A consistency test passes when no consistency rule
//!   fires, an inconsistency test when one does; a positive entailment test when its
//!   conclusion, blank nodes read as variables and the ontology header left out, is in the
//!   closure (or the premise is inconsistent, which entails everything); a negative
//!   entailment test when it isn't. Conclusions the rules never derive (`owl:differentFrom`,
//!   `owl:AllDifferent`, complements, negative property assertions) are decided by
//!   refutation (`StoreService::entails`).
//! - The OWL 2 RL/RDF rules are complete only for ground atomic conclusions from RL
//!   premises (theorem PR1 of the profiles document). A positive entailment they can't
//!   show goes to the store's OWL 2 DL path (`StoreService::entails_dl`, the owl2-dl
//!   mode's engines): the Direct Semantics' entailment implies the RDF-Based Semantics'
//!   for OWL 2 DL ontologies (the correspondence theorem, OWL 2 RDF-Based Semantics §7.2),
//!   and a conclusion or premise outside OWL 2 DL is never decided so. The tests neither
//!   path passes are listed in `expected-failures.txt`. The run fails on any failure not
//!   in the list, and on any listed test that passes.

use std::collections::{BTreeMap, BTreeSet};

use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};

#[path = "../w3c_owl2_suite/mod.rs"]
mod suite;

use suite::{Case, cases, parse_rdf_xml, quoted_entities, suite_path};

const EXPECTED_FAILURES: &str = include_str!("expected-failures.txt");

/// The positive entailments the rules can't show, decided through the OWL 2 DL path.
/// Exactly these: one more would hide a regression of the rules.
const THROUGH_DL: &[&str] = &[
    // Bare class expressions as the conclusion (a minCardinality restriction, a union):
    // no axiom under the Direct Semantics; their existence follows from the RDF-Based
    // Semantics' comprehension conditions.
    "WebOnt-I5.26-010",
    "WebOnt-I5.5-005",
    // Reflexivity, which OWL 2 RL leaves out.
    "New-Feature-ReflexiveProperty-001",
    // A chain p o p -> p makes p transitive.
    "chain2trans1",
    // Datatype subsumption and intersection (the datatype theory).
    "WebOnt-I5.8-006",
    "WebOnt-I5.8-008",
    "WebOnt-I5.8-009",
];

/// Premises the suite gives in functional syntax only, in RDF (Turtle) by the OWL 2
/// mapping to RDF graphs; the others of that kind are skipped.
const TRANSLATIONS: &[(&str, &str)] = &[
    (
        "Plus and Minus Zero are Distinct",
        r#"@prefix : <http://example.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
[] a owl:Ontology .
:Meg a owl:NamedIndividual .
:numberOfChildren a owl:DatatypeProperty , owl:FunctionalProperty .
:Meg :numberOfChildren "+0.0"^^xsd:float , "-0.0"^^xsd:float ."#,
    ),
    (
        "string-integer-clash",
        r#"@prefix : <http://example.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
[] a owl:Ontology .
:a a owl:NamedIndividual .
:hasAge a owl:DatatypeProperty ; rdfs:range xsd:integer .
:a a [ a owl:Restriction ; owl:onProperty :hasAge ; owl:hasValue "aString"^^xsd:string ] ."#,
    ),
    (
        "functionality-clash",
        r#"@prefix : <http://example.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
[] a owl:Ontology .
:a a owl:NamedIndividual .
:hasAge a owl:DatatypeProperty , owl:FunctionalProperty .
:a a [ a owl:Restriction ; owl:onProperty :hasAge ; owl:hasValue "18"^^xsd:integer ] .
:a a [ a owl:Restriction ; owl:onProperty :hasAge ; owl:hasValue "19"^^xsd:integer ] ."#,
    ),
];
/// How a test passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Passed {
    /// By the OWL 2 RL rules alone.
    Rules,
    /// A positive entailment through the OWL 2 DL path.
    Dl,
}

fn run(name: &str, case: &Case, dir: &std::path::Path) -> Result<Passed, String> {
    let stem = name.replace(|c: char| !c.is_ascii_alphanumeric(), "_");
    let file = match (&case.premise, TRANSLATIONS.iter().find(|(n, _)| *n == name)) {
        (Some(premise), _) => {
            let file = dir.join(format!("{stem}.rdf"));
            std::fs::write(&file, quoted_entities(premise)).map_err(|e| e.to_string())?;
            file
        }
        (None, Some((_, turtle))) => {
            let file = dir.join(format!("{stem}.ttl"));
            std::fs::write(&file, turtle).map_err(|e| e.to_string())?;
            file
        }
        (None, None) => return Err("no RDF premise".to_owned()),
    };
    let store = StoreService::new(StoreConfig::in_memory()).map_err(|e| e.to_string())?;
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })
        .map_err(|e| format!("premise: {e}"))?;
    let report = store
        .rematerialise(Ruleset::Owl2Rl)
        .map_err(|e| e.to_string())?;
    let inconsistent = report.violations > 0;
    // The store's entailment check: the closure for positive statements, refutation for
    // the negative ones (owl:differentFrom, owl:AllDifferent, complements, negative
    // property assertions).
    let entailed = |text: &str| -> Result<bool, String> {
        let conclusion = parse_rdf_xml(text)?;
        store
            .entails(Ruleset::Owl2Rl, &conclusion)
            .map(nrese_store::Entailment::holds)
            .map_err(|e| e.to_string())
    };
    let mut passed = Passed::Rules;
    for kind in &case.kinds {
        match kind.as_str() {
            "ConsistencyTest" if inconsistent => {
                return Err(format!(
                    "{} violations in a consistent ontology",
                    report.violations
                ));
            }
            "InconsistencyTest" if !inconsistent => {
                return Err("the inconsistency isn't found".to_owned());
            }
            "PositiveEntailmentTest" if !inconsistent => {
                let conclusion = case.conclusion.as_ref().ok_or("no RDF/XML conclusion")?;
                if !entailed(conclusion)? {
                    let dl = store
                        .entails_dl(&parse_rdf_xml(conclusion)?)
                        .map_err(|e| e.to_string())?;
                    if !dl.holds() {
                        return Err(format!(
                            "the conclusion isn't entailed (OWL 2 DL: {:?})",
                            dl.answer
                        ));
                    }
                    passed = Passed::Dl;
                }
            }
            "NegativeEntailmentTest" => {
                let conclusion = case
                    .non_conclusion
                    .as_ref()
                    .ok_or("no RDF/XML non-conclusion")?;
                if inconsistent || entailed(conclusion)? {
                    return Err("the non-conclusion is entailed".to_owned());
                }
            }
            _ => {}
        }
    }
    Ok(passed)
}

#[test]
fn w3c_owl2_rl_test_cases() {
    let path = suite_path();
    if !path.exists() {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|value| value.is_empty()),
            "NRESE_W3C_REQUIRED is set, but the OWL 2 test cases are missing: {}",
            path.display()
        );
        eprintln!("skipped: OWL 2 test cases not found (run scripts/fetch-w3c-tests.sh)");
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let all = cases(&parse_rdf_xml(&text).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut failed: BTreeMap<String, String> = BTreeMap::new();
    let (mut passed, mut skipped) = (0, Vec::new());
    let mut through_dl = Vec::new();
    for (name, case) in &all {
        if !(case.profiles.contains("RL") && case.semantics.contains("RDF-BASED")) || case.rejected
        {
            continue;
        }
        if case.imports {
            skipped.push(format!("{name}: imports another ontology"));
            continue;
        }
        if case.premise.is_none() && !TRANSLATIONS.iter().any(|(n, _)| n == name) {
            skipped.push(format!("{name}: its premise is in functional syntax only"));
            continue;
        }
        match run(name, case, dir.path()) {
            Ok(how) => {
                passed += 1;
                if how == Passed::Dl {
                    through_dl.push(name.clone());
                }
            }
            Err(reason) => {
                failed.insert(name.clone(), reason);
            }
        }
    }
    let expected: BTreeSet<&str> = EXPECTED_FAILURES
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|name| !name.is_empty())
        .collect();
    let failed_names: BTreeSet<&str> = failed.keys().map(String::as_str).collect();
    let new: Vec<_> = failed
        .iter()
        .filter(|(name, _)| !expected.contains(name.as_str()))
        .collect();
    let fixed: Vec<_> = expected.difference(&failed_names).collect();
    eprintln!(
        "W3C OWL 2 RL test cases (RDF-based semantics): {passed} passed, {} failed ({} expected), {} skipped",
        failed.len(),
        failed.len() - new.len(),
        skipped.len()
    );
    for s in &skipped {
        eprintln!("  skipped {s}");
    }
    for (name, reason) in &failed {
        eprintln!("  failed {name}: {reason}");
    }
    eprintln!(
        "  {} positive entailments through the OWL 2 DL path: {through_dl:?}",
        through_dl.len()
    );
    let mut expected_dl: Vec<&str> = THROUGH_DL.to_vec();
    expected_dl.sort_unstable();
    through_dl.sort();
    assert_eq!(
        through_dl, expected_dl,
        "the positive entailments decided through the OWL 2 DL path changed"
    );
    assert!(
        new.is_empty() && fixed.is_empty(),
        "failures not in expected-failures.txt: {new:#?}\nlisted but now passing (remove from expected-failures.txt): {fixed:#?}"
    );
}
