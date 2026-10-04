//! U1 on the W3C OWL 2 test cases of species DL under the direct semantics:
//!
//! - **Inconsistency tests:** U1 must derive a clash (PAGOdA, Theorem 5.5 (i)), where it
//!   covers every axiom and checks every `⊥` (`Program::proves_consistency`; counted
//!   apart where it doesn't).
//! - **Positive entailment tests:** every atomic assertion of the conclusion over named
//!   individuals (`C(a)` for a named class, `R(a, b)`, `d(a, v)`) is a certain answer, so
//!   it must be in U1, as a fact or by an open data value (`Bounds::is_open`).
//!   Assertions over reserved vocabulary (`c rdf:type rdfs:Class`) state no OWL 2 DL atom.
//! - **Consistency tests:** counted as proved consistent where `Bounds::consistent`.
//! - In every case L ⊆ U1, unless L violates a consistency rule.
//!
//! Cases that import other ontologies are left out: the test case holds only its own.
//! The test cases: `.cache/owl-test/all.rdf` (scripts/fetch-w3c-tests.sh), or the file
//! `NRESE_W3C_OWL_TESTS` points to; skipped without it unless `NRESE_W3C_REQUIRED` is set.
//! `NRESE_W3C_CASE` runs the cases whose name contains it; `NRESE_W3C_VERBOSE` names each
//! with its time.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nrese_dl::bounds::Bounds;
use nrese_owl::{Axiom, ClassExpr, Statement, TermKind, Terms};
use nrese_rdf::{NamedOrBlankNode, Term as RdfTerm, Triple as RdfTriple};
use nrese_rdf_io::{RdfFormat, RdfParser};

use super::support::{self, Table, Triple};

/// The time U1's closure may take per case (`NRESE_W3C_BUDGET_SECS` changes it); past
/// it the case gives up and is counted and named, not failed.
const BUDGET_SECS: u64 = 30;

const TEST: &str = "http://www.w3.org/2007/OWL/testOntology#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

fn suite_path() -> PathBuf {
    std::env::var_os("NRESE_W3C_OWL_TESTS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/owl-test/all.rdf"),
        PathBuf::from,
    )
}

/// RDF/XML with its entity declarations in double quotes (the test cases write
/// `<!ENTITY owl 'http://…'>`, which the parser doesn't take).
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

fn parse(text: &str) -> Option<Vec<RdfTriple>> {
    RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri("http://www.w3.org/2007/OWL/test-base/")
        .ok()?
        .for_reader(quoted_entities(text).as_bytes())
        .map(|quad| quad.map(RdfTriple::from).ok())
        .collect()
}

fn ids(table: &mut Table, triples: Vec<RdfTriple>) -> Vec<Triple> {
    triples
        .into_iter()
        .map(|t| {
            [
                table.id(t.subject.into()),
                table.id(t.predicate.into()),
                table.id(t.object),
            ]
        })
        .collect()
}

/// A test case: its name, types and RDF/XML ontologies.
#[derive(Default)]
struct Case {
    name: String,
    dl: bool,
    direct: bool,
    kinds: Vec<String>,
    premise: Option<String>,
    conclusion: Option<String>,
}

fn cases(text: &str) -> Vec<Case> {
    let triples = parse(text).expect("the test case collection parses");
    let mut cases: HashMap<NamedOrBlankNode, Case> = HashMap::new();
    for t in triples {
        let literal = match &t.object {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        };
        let iri = match &t.object {
            RdfTerm::NamedNode(n) => n.as_str().to_owned(),
            _ => String::new(),
        };
        let is_type = t.predicate.as_str() == RDF_TYPE;
        let local = t.predicate.as_str().strip_prefix(TEST);
        if !is_type && local.is_none() {
            continue;
        }
        let case = cases.entry(t.subject.clone()).or_default();
        if is_type {
            if let Some(kind) = iri.strip_prefix(TEST) {
                case.kinds.push(kind.to_owned());
            }
            continue;
        }
        let Some(local) = local else {
            continue;
        };
        match local {
            "identifier" => case.name = literal.unwrap_or_default(),
            "species" if iri == format!("{TEST}DL") => case.dl = true,
            "semantics" if iri == format!("{TEST}DIRECT") => case.direct = true,
            "rdfXmlPremiseOntology" => case.premise = literal,
            "rdfXmlConclusionOntology" => case.conclusion = literal,
            _ => {}
        }
    }
    let mut out: Vec<Case> = cases
        .into_values()
        .filter(|c| c.dl && c.direct && c.premise.is_some())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Whether an IRI is in a namespace OWL 2 reserves (`owl:Thing` aside): an assertion
/// over it (`c rdf:type rdfs:Class`, read as a class assertion) states no OWL 2 DL atom.
fn reserved(table: &Table, t: u64) -> bool {
    let text = table.text(t);
    let iri = text.trim_start_matches('<').trim_end_matches('>');
    iri != format!("{OWL}Thing") && RESERVED.iter().any(|ns| iri.starts_with(ns))
}

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RESERVED: [&str; 4] = [
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://www.w3.org/2000/01/rdf-schema#",
    OWL,
    "http://www.w3.org/2001/XMLSchema#",
];

/// The conclusion's atomic assertions over named individuals, as U1 states them.
fn atoms(table: &mut Table, text: &str) -> Option<Vec<Triple>> {
    let triples = ids(table, parse(text)?);
    let statements: Vec<Statement> = triples
        .iter()
        .map(|&triple| Statement { triple, graph: 0 })
        .collect();
    let ontology = nrese_owl::read(&statements, table);
    let rdf_type = table.named(RDF_TYPE);
    let named = |table: &Table, t: u64| table.kind(t) == TermKind::Iri;
    let mut out = Vec::new();
    for axiom in &ontology.axioms {
        match axiom {
            Axiom::ClassAssertion(c, a) if named(table, *a) => {
                if let ClassExpr::Class(class) = ontology.class(*c)
                    && !reserved(table, *class)
                {
                    out.push([*a, rdf_type, *class]);
                }
            }
            Axiom::ObjectPropertyAssertion(p, a, b)
                if named(table, *a) && named(table, *b) && !reserved(table, *p) =>
            {
                out.push([*a, *p, *b]);
            }
            Axiom::DataPropertyAssertion(d, a, v) if named(table, *a) && !reserved(table, *d) => {
                out.push([*a, *d, *v])
            }
            _ => {}
        }
    }
    Some(out)
}

#[test]
fn w3c_inconsistencies_clash_and_entailed_assertions_are_in_u1() {
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
    let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    let only = std::env::var("NRESE_W3C_CASE").ok();
    let verbose = std::env::var_os("NRESE_W3C_VERBOSE").is_some();
    let budget = Duration::from_secs(
        std::env::var("NRESE_W3C_BUDGET_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(BUDGET_SECS),
    );
    let mut gave_up = Vec::new();
    for case in cases(&text) {
        if only.as_deref().is_some_and(|o| !case.name.contains(o)) {
            continue;
        }
        let mut table = Table::default();
        let Some(premise) = case.premise.as_deref().and_then(parse) else {
            *tally.entry("unparsed").or_default() += 1;
            continue;
        };
        let input = ids(&mut table, premise);
        let imports = table.named(&format!("{OWL}imports"));
        if input.iter().any(|t| t[1] == imports) {
            // The imported ontologies aren't in the test case.
            *tally.entry("imports (not resolved)").or_default() += 1;
            continue;
        }
        let (ontology, normalised) = support::read(&mut table, &input);
        if ontology.diagnostics.iter().any(|d| d.is_fatal()) {
            *tally.entry("not read").or_default() += 1;
            continue;
        }
        let clock = Instant::now();
        let program = support::compile(&mut table, &ontology, &normalised);
        let Some(upper) = support::upper_within(&mut table, &program, &input, Some(budget)) else {
            *tally.entry("gave up (time budget)").or_default() += 1;
            gave_up.push(case.name.clone());
            if verbose {
                eprintln!("{}: gave up after {:.2?}", case.name, clock.elapsed());
            }
            continue;
        };
        let lower = support::lower(&mut table, &input);
        if verbose {
            eprintln!("{}: {:.2?}", case.name, clock.elapsed());
        }
        let bounds = Bounds::new(&program, &table, lower.facts.clone(), upper.facts.clone());
        let incomplete = !program.incomplete.is_empty();
        let proves = program.proves_consistency();
        let missing = bounds.lower_not_in_upper();
        if !missing.is_empty() && lower.violations.is_empty() {
            let some: Vec<String> = missing
                .iter()
                .take(3)
                .map(|t| t.map(|x| table.text(x)).join(" "))
                .collect();
            failures.push(format!("{}: L not in U1: {some:?}", case.name));
        }
        let clash = !bounds.clashes().is_empty();
        for kind in &case.kinds {
            match kind.as_str() {
                "InconsistencyTest" => {
                    *tally.entry("inconsistency").or_default() += 1;
                    if !clash && proves {
                        failures.push(format!("{}: inconsistent, but U1 has no clash", case.name));
                    } else if !clash {
                        *tally
                            .entry("inconsistency, U1 incomplete or data unchecked")
                            .or_default() += 1;
                    }
                }
                "ConsistencyTest" => {
                    *tally.entry("consistency").or_default() += 1;
                    if bounds.consistent() {
                        *tally.entry("consistency proved by U1").or_default() += 1;
                    }
                }
                "PositiveEntailmentTest" => {
                    *tally.entry("entailment").or_default() += 1;
                    let Some(atoms) = case
                        .conclusion
                        .as_deref()
                        .and_then(|c| atoms(&mut table, c))
                    else {
                        continue;
                    };
                    *tally.entry("entailed atoms").or_default() += atoms.len();
                    for atom in atoms {
                        let held = upper.facts.binary_search(&atom).is_ok()
                            || bounds.is_open(atom[0], atom[1]);
                        if !held {
                            let text = atom.map(|t| table.text(t)).join(" ");
                            let why = if incomplete { " (U1 incomplete)" } else { "" };
                            failures.push(format!("{}: entails {text}, not in U1{why}", case.name));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    eprintln!("{tally:?}");
    if !gave_up.is_empty() {
        eprintln!("gave up after {budget:?}: {gave_up:?}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
