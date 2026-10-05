//! The W3C OWL 2 test cases of species DL under the direct semantics, through the
//! hypertableau (docs/design/owl2-dl.md §6's gate): consistency and inconsistency tests
//! directly, positive and negative entailment tests as the (un)satisfiability of the
//! premise with each case of the conclusion's negation.
//!
//! - The suite: `.cache/owl-test/all.rdf` (scripts/fetch-w3c-tests.sh) or the file
//!   `NRESE_W3C_OWL_TESTS` points to; skipped without it unless `NRESE_W3C_REQUIRED`.
//! - Documents are read with their `owl:imports` closure, from the documents the suite
//!   gives for import (`test:importedOntologyIRI`); an import it doesn't give is
//!   `not-run`. A role without an RDF/XML document, or whose RDF/XML can't be read, is
//!   read from its functional-syntax one (`nrese_owl::read_functional`).
//! - A conclusion's assertions about anonymous individuals are an existential: each
//!   tree of them is rolled up into a class expression (`rollup`).
//! - `NRESE_W3C_SWITCHES` (`no-disjunct-learning`, `full-blocking`,
//!   `no-lazy-definitions`, comma-separated)
//!   switches optimisations off, for A/B runs.
//! - Every test ends as `pass`, `wrong`, or not decided with the reason (`unsupported`,
//!   `gave-up`, `not-run`). A wrong answer fails the run; the rest is reported by test
//!   type and fragment. `NRESE_W3C_OUT` names a TSV file for the per-test results.
//! - Known wrong answers are listed in `expected-wrong.txt` with their cause; the run
//!   fails on any other wrong answer, and on a listed one that is no longer wrong.
//! - Tests whose expected answer is wrong under the direct semantics are listed in
//!   `disputed.txt` with a witness model (checked by `disputed_witnesses_are_models`);
//!   they are counted apart, and the run fails if one of them changes its answer.

mod negate;
mod rdf;
mod rollup;
#[allow(
    dead_code,
    reason = "the fuzz test's semantics; the witness check uses a part"
)]
#[path = "../tableau_fuzz/semantics.rs"]
mod semantics;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use nrese_dl::tableau::{Answer, Config, Features, Outcome, consistency};
use nrese_owl::{Axiom, ClassExpr, ExprId, Ontology, Term};
use nrese_rdf::{NamedOrBlankNode, Term as RdfTerm};

use negate::{Negator, import};
use rdf::{Table, parse_rdf_xml};

const TEST: &str = "http://www.w3.org/2007/OWL/testOntology#";
const EXPECTED_WRONG: &str = include_str!("expected-wrong.txt");
const DISPUTED: &str = include_str!("disputed.txt");

/// The lines of a list: `(id, rest)`.
fn listed(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (id, rest) = l.split_once('\t').unwrap_or((l, ""));
            (id.trim(), rest.trim())
        })
        .collect()
}

fn suite_path() -> PathBuf {
    std::env::var_os("NRESE_W3C_OWL_TESTS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/owl-test/all.rdf"),
        PathBuf::from,
    )
}

#[derive(Debug, Default, Clone)]
struct Case {
    name: String,
    dl: bool,
    direct: bool,
    types: Vec<String>,
    /// By role: `Premise`, `Conclusion`, `NonConclusion`, `Input`.
    documents: HashMap<String, String>,
    /// The same roles' functional-syntax documents.
    functional: HashMap<String, String>,
    /// `test:importedOntologyIRI`, of an imported document.
    iri: Option<String>,
    /// Every document the suite gives for import, by ontology IRI (`test:importedOntology`
    /// lists them per case, not always all a case's premise imports).
    imports: HashMap<String, String>,
}

/// How a test ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Pass,
    Wrong(String),
    Open(String),
}

fn config() -> Config {
    let secs = std::env::var("NRESE_W3C_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    // `NRESE_W3C_SWITCHES`: optimisations to switch off, for A/B runs.
    let off = std::env::var("NRESE_W3C_SWITCHES").unwrap_or_default();
    let off = |name: &str| off.split(',').any(|s| s.trim() == name);
    Config {
        timeout: Some(Duration::from_secs(secs)),
        max_memory: 2 << 30,
        disjunct_learning: !off("no-disjunct-learning"),
        incremental_blocking: !off("full-blocking"),
        lazy_definitions: !off("no-lazy-definitions"),
        ..Config::default()
    }
}

fn open_reason(a: &Answer) -> String {
    match a {
        Answer::Unsupported(why) => format!("unsupported: {why}"),
        Answer::GaveUp(why) => format!("gave-up: {why}"),
        other => format!("{other:?}"),
    }
}

/// A short class of the reason (for the tally).
fn reason_class(why: &str) -> String {
    if why.contains("data") || why.contains("datatype") {
        "datatypes".into()
    } else if why.contains("keys") {
        "keys".into()
    } else if why.contains("NI rule") {
        "NI rule".into()
    } else if why.contains("time budget") || why.contains("node budget") {
        "budget".into()
    } else if why.starts_with("not-run") {
        why.split(':').take(2).collect::<Vec<_>>().join(":")
    } else {
        why.chars().take(60).collect()
    }
}

/// Entailment of `conclusion` by `premise`: `Some(true)` if every case of every axiom's
/// negation is inconsistent, `Some(false)` if one is consistent, else why not.
fn entailed(
    premise: &Ontology,
    conclusion: &Ontology,
    table: &Table,
    features: &mut Features,
) -> Result<bool, String> {
    let mut open: Option<String> = None;
    let anonymous: Vec<&Axiom> = conclusion
        .axioms
        .iter()
        .filter(|a| axiom_mentions_blank(a, table))
        .collect();
    if !anonymous.is_empty() {
        let mut o = premise.clone();
        o.data.extend(&conclusion.data);
        match rollup::roll_up(conclusion, &anonymous, |t| table.is_blank(t), &mut o) {
            Err(why) => {
                open.get_or_insert(why);
            }
            Ok(groups) => {
                for c in groups {
                    // Some individual is a `c` iff `⊤ ⊑ ¬c` is inconsistent with the premise.
                    let mut test = o.clone();
                    let thing = ExprId(test.classes.intern(ClassExpr::Thing));
                    let not = ExprId(test.classes.intern(ClassExpr::Not(c)));
                    test.axioms.push(Axiom::SubClassOf(thing, not));
                    test.sources.push(Vec::new());
                    let out: Outcome = consistency(&test, &config());
                    merge_features(features, out.features);
                    match out.answer {
                        Answer::Inconsistent => {}
                        Answer::Consistent => return Ok(false),
                        other => {
                            open.get_or_insert(open_reason(&other));
                        }
                    }
                }
            }
        }
    }
    for axiom in &conclusion.axioms {
        if axiom_mentions_blank(axiom, table) {
            continue;
        }
        let mut o = premise.clone();
        o.data.extend(&conclusion.data);
        let imported = import(conclusion, axiom, &mut o);
        let Some(cases) = Negator::new(&mut o).cases(&imported) else {
            open.get_or_insert(format!(
                "not-run: no reduction for {}",
                format!("{axiom:?}").split('(').next().unwrap_or("")
            ));
            continue;
        };
        for case in cases {
            let mut test = o.clone();
            for a in case {
                test.axioms.push(a);
                test.sources.push(Vec::new());
            }
            let out: Outcome = consistency(&test, &config());
            merge_features(features, out.features);
            match out.answer {
                Answer::Inconsistent => {}
                Answer::Consistent => return Ok(false),
                other => {
                    open.get_or_insert(open_reason(&other));
                }
            }
        }
    }
    match open {
        Some(why) => Err(why),
        None => Ok(true),
    }
}

fn axiom_mentions_blank(axiom: &Axiom, table: &Table) -> bool {
    let b = |t: &Term| table.is_blank(*t);
    match axiom {
        Axiom::ClassAssertion(_, a) => b(a),
        Axiom::ObjectPropertyAssertion(_, x, y)
        | Axiom::NegativeObjectPropertyAssertion(_, x, y) => b(x) || b(y),
        Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => xs.iter().any(b),
        Axiom::DataPropertyAssertion(_, x, _) | Axiom::NegativeDataPropertyAssertion(_, x, _) => {
            b(x)
        }
        _ => false,
    }
}

fn merge_features(into: &mut Features, f: Features) {
    into.inverses |= f.inverses;
    into.numbers |= f.numbers;
    into.nominals |= f.nominals;
    into.disjunctions |= f.disjunctions;
    into.datatypes |= f.datatypes;
    into.weakened |= f.weakened;
}

fn fragment(f: &Features) -> String {
    let mut s = String::from("AL");
    if f.disjunctions {
        s.push('C');
    }
    if f.inverses {
        s.push('I');
    }
    if f.nominals {
        s.push('O');
    }
    if f.numbers {
        s.push('Q');
    }
    if f.datatypes {
        s.push_str("(D)");
    }
    if f.weakened {
        s.push_str("(D/keys)");
    }
    s
}

/// The ontology of a case's document of one of `roles` (the first it has): from RDF/XML,
/// else (or where the RDF/XML can't be read) from the functional syntax; `None` if it has
/// none.
fn document(case: &Case, roles: &[&str], table: &mut Table) -> Option<Result<Ontology, String>> {
    let functional = roles.iter().find_map(|r| case.functional.get(*r));
    if let Some(text) = roles.iter().find_map(|r| case.documents.get(*r)) {
        return Some(match (table.ontology(text, &case.imports), functional) {
            (Err(_), Some(text)) => table.functional(text),
            (read, _) => read,
        });
    }
    Some(table.functional(functional?))
}

fn run_case(case: &Case, kind: &str) -> (Verdict, Features) {
    let mut features = Features::default();
    let mut table = Table::default();
    let premise = match document(case, &["Premise", "Input"], &mut table) {
        None => return (Verdict::Open("not-run: no premise".into()), features),
        Some(Err(why)) => return (Verdict::Open(format!("not-run: {why}")), features),
        Some(Ok(o)) => o,
    };
    if std::env::var_os("NRESE_W3C_DUMP").is_some() {
        for a in &premise.axioms {
            eprintln!("  premise: {}", premise.functional(a, &|t| table.name(t)));
        }
        eprintln!("  diagnostics: {:?}", premise.diagnostics);
        let n = nrese_owl::normalise(&premise);
        eprintln!(
            "  unsupported: {:?}; {} clauses",
            n.unsupported,
            n.clauses.len()
        );
    }
    let verdict = match kind {
        "ConsistencyTest" | "InconsistencyTest" => {
            let out = consistency(&premise, &config());
            if std::env::var_os("NRESE_W3C_TRACE").is_some() {
                eprintln!("  {:?} {}", out.answer, out.telemetry);
            }
            features = out.features;
            let expected = if kind == "ConsistencyTest" {
                Answer::Consistent
            } else {
                Answer::Inconsistent
            };
            match out.answer {
                a if a == expected => Verdict::Pass,
                a @ (Answer::Consistent | Answer::Inconsistent) => {
                    Verdict::Wrong(format!("answered {a:?}"))
                }
                other => Verdict::Open(open_reason(&other)),
            }
        }
        _ => {
            let positive = kind == "PositiveEntailmentTest";
            let role = if positive {
                "Conclusion"
            } else {
                "NonConclusion"
            };
            let conclusion = match document(case, &[role], &mut table) {
                None => return (Verdict::Open(format!("not-run: no {role}")), features),
                Some(Err(why)) => return (Verdict::Open(format!("not-run: {why}")), features),
                Some(Ok(o)) => o,
            };
            match entailed(&premise, &conclusion, &table, &mut features) {
                Ok(e) if e == positive => Verdict::Pass,
                Ok(e) => Verdict::Wrong(format!("entailed: {e}")),
                Err(why) => Verdict::Open(why),
            }
        }
    };
    (verdict, features)
}

fn read_cases(text: &str) -> Vec<Case> {
    let triples = parse_rdf_xml(text).expect("the test case collection parses");
    let mut cases: HashMap<NamedOrBlankNode, Case> = HashMap::new();
    for t in &triples {
        let case = cases.entry(t.subject.clone()).or_default();
        let value = match &t.object {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        };
        let object_local = match &t.object {
            RdfTerm::NamedNode(n) => n.as_str().strip_prefix(TEST).map(str::to_owned),
            _ => None,
        };
        let predicate = t.predicate.as_str();
        if predicate == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type" {
            if let Some(local) = object_local {
                case.types.push(local);
            }
            continue;
        }
        let Some(local) = predicate.strip_prefix(TEST) else {
            continue;
        };
        match local {
            "identifier" => case.name = value.unwrap_or_default(),
            "importedOntologyIRI" => {
                if let RdfTerm::NamedNode(n) = &t.object {
                    case.iri = Some(n.as_str().to_owned());
                }
            }
            "species" => case.dl |= object_local.as_deref() == Some("DL"),
            "semantics" => case.direct |= object_local.as_deref() == Some("DIRECT"),
            _ => {
                if let (Some(role), Some(text)) = (
                    local
                        .strip_prefix("rdfXml")
                        .and_then(|r| r.strip_suffix("Ontology")),
                    value.clone(),
                ) {
                    case.documents.insert(role.to_owned(), text);
                } else if let (Some(role), Some(text)) = (
                    local
                        .strip_prefix("fs")
                        .and_then(|r| r.strip_suffix("Ontology")),
                    value,
                ) {
                    case.functional.insert(role.to_owned(), text);
                }
            }
        }
    }
    let library: HashMap<String, String> = cases
        .values()
        .filter_map(|c| Some((c.iri.clone()?, c.documents.get("Input")?.clone())))
        .collect();
    let mut out: Vec<Case> = cases
        .into_values()
        .filter(|c| c.dl && c.direct && !c.name.is_empty())
        .map(|mut c| {
            c.imports = library.clone();
            c
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// W3C tests that are hard for the search, decided within a budget: DL-202 (a modal
/// formula as definitions, 20 s and given up before disjunct learning; HermiT 0.3 s) and
/// DL-206 (HermiT 3 s).
#[test]
fn hard_search_tests_are_decided() {
    let Ok(text) = std::fs::read_to_string(suite_path()) else {
        return;
    };
    for name in [
        "WebOnt-description-logic-202",
        "WebOnt-description-logic-206",
    ] {
        let case = read_cases(&text)
            .into_iter()
            .find(|c| c.name == name)
            .expect("the suite has it");
        let (verdict, _) = run_case(&case, "ConsistencyTest");
        assert_eq!(verdict, Verdict::Pass, "{name}");
    }
}

/// The witness of every disputed test is a model of its premise.
#[test]
fn disputed_witnesses_are_models() {
    let Ok(text) = std::fs::read_to_string(suite_path()) else {
        return;
    };
    let cases = read_cases(&text);
    for (id, model) in listed(DISPUTED) {
        let name = id.split(" (").next().unwrap_or(id);
        let case = cases
            .iter()
            .find(|c| c.name == name)
            .expect("the suite has the test");
        let premise = case
            .documents
            .get("Premise")
            .or_else(|| case.documents.get("Input"))
            .expect("a premise");
        let mut table = Table::default();
        let o = table.ontology(premise, &case.imports).expect("reads");
        let i = witness(model, &table);
        let failing: Vec<String> = o
            .axioms
            .iter()
            .filter(|a| !semantics::holds(&o, a, &i))
            .map(|a| o.functional(a, &|t| table.name(t)))
            .collect();
        assert!(failing.is_empty(), "{id}: the witness fails {failing:#?}");
    }
}

/// The interpretation a `disputed.txt` model line describes.
fn witness(line: &str, table: &Table) -> semantics::Interp {
    use nrese_owl::Terms;
    let mut parts = line.split("; ");
    let n: u32 = parts
        .next()
        .and_then(|p| p.split(' ').next())
        .and_then(|k| k.parse().ok())
        .expect("N elements");
    let mut i = semantics::Interp {
        n,
        ..semantics::Interp::default()
    };
    for part in parts {
        let (iri, value) = part.split_once(" = ").expect("<iri> = value");
        let term = table
            .iri(iri.trim_start_matches('<').trim_end_matches('>'))
            .unwrap_or_else(|| panic!("{iri} is in the premise"));
        let value = value.trim();
        if let Some(set) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
            if set.contains('(') {
                for pair in set
                    .split("),")
                    .map(|p| p.trim_matches(|c| " ()".contains(c)))
                {
                    let (a, b) = pair.split_once(',').expect("(j, k)");
                    i.add_edge(term, a.trim().parse().unwrap(), b.trim().parse().unwrap());
                }
            } else {
                let mask = set
                    .split(',')
                    .filter(|k| !k.trim().is_empty())
                    .fold(0u128, |m, k| m | 1 << k.trim().parse::<u32>().unwrap());
                i.concepts.insert(term, mask);
            }
        } else {
            i.individuals
                .insert(term, value.parse().expect("an element"));
        }
    }
    i
}

#[test]
fn the_w3c_dl_suite() {
    let path = suite_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none(),
            "the W3C OWL 2 test cases are required: {}",
            path.display()
        );
        eprintln!("skipped: no {}", path.display());
        return;
    };
    let kinds = [
        "ConsistencyTest",
        "InconsistencyTest",
        "PositiveEntailmentTest",
        "NegativeEntailmentTest",
    ];
    let only = std::env::var("NRESE_W3C_ONLY").ok();
    let mut by_kind: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut by_fragment: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let (mut wrong, mut rows) = (Vec::new(), String::new());
    let disputed: Vec<&str> = listed(DISPUTED).into_iter().map(|(id, _)| id).collect();
    let mut changed = Vec::new();
    for case in read_cases(&text) {
        if only
            .as_ref()
            .is_some_and(|o| !case.name.contains(o.as_str()))
        {
            continue;
        }
        for kind in kinds.iter().filter(|k| case.types.iter().any(|t| t == *k)) {
            let started = std::time::Instant::now();
            if std::env::var_os("NRESE_W3C_TRACE").is_some() {
                eprintln!("running {} ({kind})", case.name);
            }
            let (verdict, features) = run_case(&case, kind);
            let ms = started.elapsed().as_millis();
            let id = format!("{} ({kind})", case.name);
            let is_disputed = disputed.contains(&id.as_str());
            if is_disputed && !matches!(verdict, Verdict::Wrong(_)) {
                changed.push(format!("{id}: {verdict:?}"));
            }
            let (class, detail) = match &verdict {
                Verdict::Wrong(why) if is_disputed => ("disputed", why.clone()),
                Verdict::Pass => ("pass", String::new()),
                Verdict::Wrong(why) => {
                    wrong.push(format!("{} ({kind}): {why}", case.name));
                    ("wrong", why.clone())
                }
                Verdict::Open(why) => {
                    *reasons.entry(reason_class(why)).or_default() += 1;
                    ("open", why.clone())
                }
            };
            *by_kind.entry((kind.to_string(), class.into())).or_default() += 1;
            *by_fragment
                .entry((fragment(&features), class.into()))
                .or_default() += 1;
            let _ = writeln!(
                rows,
                "{}\t{kind}\t{}\t{class}\t{ms}\t{detail}",
                case.name,
                fragment(&features)
            );
        }
    }
    eprintln!("by test type: {by_kind:#?}");
    eprintln!("by fragment: {by_fragment:#?}");
    eprintln!("open, by reason: {reasons:#?}");
    if let Some(out) = std::env::var_os("NRESE_W3C_OUT") {
        std::fs::write(out, rows).expect("writes the results");
    }
    let expected: Vec<&str> = listed(EXPECTED_WRONG)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(
        changed.is_empty(),
        "disputed tests that changed their answer:\n{changed:#?}"
    );
    let ids: Vec<String> = wrong
        .iter()
        .map(|w| w.split("): ").next().unwrap_or(w).to_owned() + ")")
        .collect();
    let unlisted: Vec<&String> = wrong
        .iter()
        .zip(&ids)
        .filter(|(_, id)| !expected.contains(&id.as_str()))
        .map(|(w, _)| w)
        .collect();
    let fixed: Vec<&&str> = expected
        .iter()
        .filter(|e| only.is_none() && !ids.iter().any(|id| id == *e))
        .collect();
    eprintln!(
        "wrong: {} ({} listed in expected-wrong.txt)",
        wrong.len(),
        wrong.len() - unlisted.len()
    );
    assert!(
        unlisted.is_empty() && fixed.is_empty(),
        "wrong answers not listed:\n{unlisted:#?}\nlisted but no longer wrong:\n{fixed:#?}"
    );
}
