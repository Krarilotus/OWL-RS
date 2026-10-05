//! The W3C OWL 2 test cases of the QL profile under the Direct Semantics, on the store with
//! the `owl2-ql` ruleset and the tree-witness rewriting of queries
//! (docs/design/ql-rewriting.md §6).
//!
//! - The test cases: `.cache/owl-test/all.rdf` (scripts/fetch-w3c-tests.sh), or the file
//!   `NRESE_W3C_OWL_TESTS` points to. Without it the test is skipped, unless
//!   `NRESE_W3C_REQUIRED` is set (as in CI).
//! - Each premise is bulk loaded into a fresh in-memory store and materialised. Consistency
//!   tests pass when no consistency rule fires, inconsistency tests when one does.
//! - An entailment test's conclusion is **query-shaped** when, without its ontology header,
//!   its declarations and `owl:Thing` memberships, it states only class memberships and
//!   property values (blank nodes are anonymous individuals): it is asked as a SPARQL `ASK`
//!   with the blank nodes as existential terms, so the rewriting answers it. Any other
//!   conclusion goes to the store's entailment check (the closure, and refutation for the
//!   negative statements), as in the RL runner.
//! - The tests the store can't pass are listed in `expected-failures.txt`; the run fails on
//!   any failure not in the list, and on any listed test that passes.

use std::collections::{BTreeMap, BTreeSet};

use nrese_rdf::{Term, Triple};
use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};

#[path = "../w3c_owl2_suite/mod.rs"]
mod suite;

use suite::{Case, RDF_TYPE, cases, parse_rdf_xml, quoted_entities, suite_path};

const EXPECTED_FAILURES: &str = include_str!("expected-failures.txt");
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// What a test needed of the store.
#[derive(Default)]
struct Tally {
    queries: usize,
    existential: usize,
}

/// The statements of a conclusion that say something about individuals, if all of its
/// statements are of that kind or carry no meaning under the Direct Semantics (the header,
/// declarations, annotations, `owl:Thing` memberships). Empty: nothing to entail.
fn query_shaped(conclusion: &[Triple]) -> Option<Vec<&Triple>> {
    let vocabulary = |iri: &str| [OWL, RDFS, RDF].iter().any(|ns| iri.starts_with(ns));
    let annotation_property = |p: &str| {
        ["comment", "label", "seeAlso", "isDefinedBy"]
            .iter()
            .any(|local| p == format!("{RDFS}{local}"))
            || conclusion.iter().any(|t| {
                t.subject.to_string() == format!("<{p}>")
                    && t.predicate.as_str() == RDF_TYPE
                    && t.object.to_string() == format!("<{OWL}AnnotationProperty>")
            })
    };
    let mut out = Vec::new();
    for t in conclusion {
        if annotation_property(t.predicate.as_str()) {
            continue;
        }
        if t.predicate.as_str() == RDF_TYPE {
            match &t.object {
                Term::NamedNode(class) if !vocabulary(class.as_str()) => out.push(t),
                // Header, declarations, `owl:Thing`.
                Term::NamedNode(class)
                    if [
                        "Ontology",
                        "Class",
                        "ObjectProperty",
                        "DatatypeProperty",
                        "AnnotationProperty",
                        "NamedIndividual",
                        "Thing",
                    ]
                    .iter()
                    .any(|k| class.as_str() == format!("{OWL}{k}"))
                        || class.as_str() == format!("{RDFS}Datatype") => {}
                _ => return None,
            }
        } else if vocabulary(t.predicate.as_str()) {
            return None;
        } else {
            out.push(t);
        }
    }
    Some(out)
}

fn run(name: &str, case: &Case, dir: &std::path::Path, tally: &mut Tally) -> Result<(), String> {
    let stem = name.replace(|c: char| !c.is_ascii_alphanumeric(), "_");
    let premise = case.premise.as_ref().ok_or("no RDF premise")?;
    let file = dir.join(format!("{stem}.rdf"));
    std::fs::write(&file, quoted_entities(premise)).map_err(|e| e.to_string())?;
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
        .rematerialise(Ruleset::Owl2Ql)
        .map_err(|e| e.to_string())?;
    let inconsistent = report.violations > 0;
    let mut entailed = |text: &str| -> Result<bool, String> {
        let conclusion = parse_rdf_xml(text)?;
        if let Some(statements) = query_shaped(&conclusion) {
            tally.queries += 1;
            if statements
                .iter()
                .any(|t| t.subject.is_blank_node() || t.object.is_blank_node())
            {
                tally.existential += 1;
            }
            if statements.is_empty() {
                return Ok(true);
            }
            let body: Vec<String> = statements
                .iter()
                .map(|t| format!("{} {} {} .", t.subject, t.predicate, t.object))
                .collect();
            let query = format!("ASK {{ {} }}", body.join(" "));
            let result = store
                .execute_query_str(&query)
                .map_err(|e| format!("{query}: {e}"))?;
            let text = String::from_utf8(result.payload).map_err(|e| e.to_string())?;
            return Ok(text.contains("true"));
        }
        store
            .entails(Ruleset::Owl2Ql, &conclusion)
            .map(nrese_store::Entailment::holds)
            .map_err(|e| e.to_string())
    };
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
                    return Err("the conclusion isn't entailed".to_owned());
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
    Ok(())
}

#[test]
fn w3c_owl2_ql_test_cases() {
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
    let mut tally = Tally::default();
    for (name, case) in &all {
        if !(case.profiles.contains("QL") && case.semantics.contains("DIRECT")) || case.rejected {
            continue;
        }
        if case.imports {
            skipped.push(format!("{name}: imports another ontology"));
            continue;
        }
        if case.premise.is_none() {
            skipped.push(format!("{name}: its premise is in functional syntax only"));
            continue;
        }
        match run(name, case, dir.path(), &mut tally) {
            Ok(()) => passed += 1,
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
        "W3C OWL 2 QL test cases (Direct Semantics): {passed} passed, {} failed ({} expected), {} skipped; {} conclusions asked as queries, {} through anonymous individuals",
        failed.len(),
        failed.len() - new.len(),
        skipped.len(),
        tally.queries,
        tally.existential
    );
    for s in &skipped {
        eprintln!("  skipped {s}");
    }
    assert!(
        new.is_empty() && fixed.is_empty(),
        "failures not in expected-failures.txt: {new:#?}\nlisted but now passing (remove from expected-failures.txt): {fixed:#?}"
    );
}
