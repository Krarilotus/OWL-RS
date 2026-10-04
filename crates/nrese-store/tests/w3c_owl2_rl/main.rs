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
//!   premises (theorem PR1 of the profiles document); the tests they can't pass are listed
//!   in `expected-failures.txt`. The run fails on any failure not in the list, and on any
//!   listed test that passes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use nrese_rdf::{NamedOrBlankNode, Term, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};

const EXPECTED_FAILURES: &str = include_str!("expected-failures.txt");

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
const TEST: &str = "http://www.w3.org/2007/OWL/testOntology#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

fn suite_path() -> PathBuf {
    std::env::var_os("NRESE_W3C_OWL_TESTS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/owl-test/all.rdf"),
        PathBuf::from,
    )
}

/// RDF/XML with its entity declarations in double quotes: the test cases write
/// `<!ENTITY owl 'http://…'>`, which the parser doesn't take.
fn quoted_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!ENTITY") {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let end = from.find('>').map_or(from.len(), |e| e + 1);
        let declaration = &from[..end];
        if declaration.contains('"') {
            out.push_str(declaration);
        } else {
            out.push_str(&declaration.replace('\'', "\""));
        }
        rest = &from[end..];
    }
    out.push_str(rest);
    out
}

fn parse_rdf_xml(text: &str) -> Result<Vec<Triple>, String> {
    let text = quoted_entities(text);
    RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri("http://www.w3.org/2007/OWL/test-base/")
        .map_err(|e| e.to_string())?
        .for_reader(text.as_bytes())
        .map(|quad| quad.map(Triple::from).map_err(|e| e.to_string()))
        .collect()
}

/// One test case: its kinds (the local names of its types) and ontologies.
#[derive(Default)]
struct Case {
    kinds: BTreeSet<String>,
    premise: Option<String>,
    conclusion: Option<String>,
    non_conclusion: Option<String>,
    imports: bool,
    rl: bool,
    rdf_based: bool,
    rejected: bool,
}

fn cases(triples: &[Triple]) -> BTreeMap<String, Case> {
    let mut by_node: HashMap<NamedOrBlankNode, Case> = HashMap::new();
    let mut names: HashMap<NamedOrBlankNode, String> = HashMap::new();
    for t in triples {
        let Some(local) = t.predicate.as_str().strip_prefix(TEST) else {
            if t.predicate.as_str() == RDF_TYPE
                && let Term::NamedNode(class) = &t.object
                && let Some(kind) = class.as_str().strip_prefix(TEST)
            {
                by_node
                    .entry(t.subject.clone())
                    .or_default()
                    .kinds
                    .insert(kind.to_owned());
            }
            continue;
        };
        let case = by_node.entry(t.subject.clone()).or_default();
        let text = || match &t.object {
            Term::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        };
        let object = |name: &str| matches!(&t.object, Term::NamedNode(n) if n.as_str() == format!("{TEST}{name}"));
        match local {
            "identifier" => {
                if let Some(name) = text() {
                    names.insert(t.subject.clone(), name);
                }
            }
            "rdfXmlPremiseOntology" => case.premise = text(),
            "rdfXmlConclusionOntology" => case.conclusion = text(),
            "rdfXmlNonConclusionOntology" => case.non_conclusion = text(),
            "importedOntology" => case.imports = true,
            "profile" => case.rl |= object("RL"),
            "semantics" => case.rdf_based |= object("RDF-BASED"),
            "status" => case.rejected |= object("Rejected") || object("Extracredit"),
            _ => {}
        }
    }
    by_node
        .into_iter()
        .filter_map(|(node, case)| Some((names.get(&node)?.clone(), case)))
        .collect()
}

fn run(name: &str, case: &Case, dir: &std::path::Path) -> Result<(), String> {
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
    for (name, case) in &all {
        if !(case.rl && case.rdf_based) || case.rejected {
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
        "W3C OWL 2 RL test cases (RDF-based semantics): {passed} passed, {} failed ({} expected), {} skipped",
        failed.len(),
        failed.len() - new.len(),
        skipped.len()
    );
    for s in &skipped {
        eprintln!("  skipped {s}");
    }
    assert!(
        new.is_empty() && fixed.is_empty(),
        "failures not in expected-failures.txt: {new:#?}\nlisted but now passing (remove from expected-failures.txt): {fixed:#?}"
    );
}
