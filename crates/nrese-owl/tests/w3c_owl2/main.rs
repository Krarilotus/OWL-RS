//! The W3C OWL 2 test cases' ontologies, read by the reverse mapping: every RDF/XML
//! ontology of a test of species DL reads without a fatal diagnostic, and writes back to
//! the same axioms (docs/design/owl2-dl.md §2, the gate of work package 2.1).
//!
//! - The test cases: `.cache/owl-test/all.rdf` (scripts/fetch-w3c-tests.sh), or the file
//!   `NRESE_W3C_OWL_TESTS` points to. Without it the test is skipped, unless
//!   `NRESE_W3C_REQUIRED` is set (as in CI).
//! - Ontologies the reader can't take yet are listed in `expected-failures.txt`, with why;
//!   the run fails on any failure not listed, and on any listed one that passes.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use nrese_owl::{
    Diagnostic, Make, Ontology, Statement, Term, TermKind, Terms, Vocabulary, normalise, read,
    write,
};
use nrese_rdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term as RdfTerm, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};

const EXPECTED_FAILURES: &str = include_str!("expected-failures.txt");
const TEST: &str = "http://www.w3.org/2007/OWL/testOntology#";

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

fn parse_rdf_xml(text: &str) -> Result<Vec<Triple>, String> {
    RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri("http://www.w3.org/2007/OWL/test-base/")
        .map_err(|e| e.to_string())?
        .for_reader(quoted_entities(text).as_bytes())
        .map(|quad| quad.map(Triple::from).map_err(|e| e.to_string()))
        .collect()
}

/// Terms by id.
#[derive(Default)]
struct Table {
    terms: Vec<RdfTerm>,
    ids: HashMap<RdfTerm, u64>,
    blanks: u64,
}

impl Table {
    fn id(&mut self, term: RdfTerm) -> u64 {
        if let Some(&id) = self.ids.get(&term) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(term.clone());
        self.ids.insert(term, id);
        id
    }

    fn name(&self, id: Term) -> String {
        match &self.terms[id as usize] {
            RdfTerm::BlankNode(_) => format!("_:b{id}"),
            other => other.to_string(),
        }
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match &self.terms[term as usize] {
            RdfTerm::NamedNode(_) => TermKind::Iri,
            RdfTerm::BlankNode(_) => TermKind::Blank,
            _ => TermKind::Literal,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids
            .get(&RdfTerm::NamedNode(NamedNode::new_unchecked(iri)))
            .copied()
    }
}

impl Make for Table {
    fn blank(&mut self) -> Term {
        self.blanks += 1;
        self.id(BlankNode::new_unchecked(format!("w{}", self.blanks)).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        self.id(Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)).into())
    }
}

fn rendered(ontology: &Ontology, table: &Table) -> Vec<String> {
    let mut lines: Vec<String> = ontology
        .axioms
        .iter()
        .map(|a| ontology.functional(a, &|t| table.name(t)))
        .collect();
    lines.sort();
    lines
}

/// Reads one ontology and writes it back; why it fails, if it does.
fn check(text: &str) -> Result<(), String> {
    let triples = parse_rdf_xml(text)?;
    let mut table = Table::default();
    let statements: Vec<Statement> = triples
        .into_iter()
        .map(|t| Statement {
            triple: [
                table.id(t.subject.into()),
                table.id(t.predicate.into()),
                table.id(t.object),
            ],
            graph: 0,
        })
        .collect();
    let ontology = read(&statements, &table);
    let fatal: Vec<&Diagnostic> = ontology
        .diagnostics
        .iter()
        .filter(|d| d.is_fatal())
        .collect();
    if !fatal.is_empty() {
        let named: Vec<String> = fatal
            .iter()
            .take(3)
            .map(|d| match d {
                Diagnostic::NotOwl { triple } => format!(
                    "not OWL: {} {} {}",
                    table.name(triple[0]),
                    table.name(triple[1]),
                    table.name(triple[2])
                ),
                Diagnostic::Malformed { node, what } => {
                    let about: Vec<String> = statements
                        .iter()
                        .filter(|s| s.triple[0] == *node)
                        .map(|s| format!("{} {}", table.name(s.triple[1]), table.name(s.triple[2])))
                        .collect();
                    format!("{what}: {} [{}]", table.name(*node), about.join("; "))
                }
                Diagnostic::SharedBlankNode { node } => {
                    let uses: Vec<String> = statements
                        .iter()
                        .filter(|s| s.triple[2] == *node)
                        .map(|s| format!("{} {}", table.name(s.triple[0]), table.name(s.triple[1])))
                        .collect();
                    format!(
                        "shared {}: used by [{}]",
                        table.name(*node),
                        uses.join("; ")
                    )
                }
                other => format!("{other:?}"),
            })
            .collect();
        return Err(named.join(" | "));
    }
    for (_, iri) in Vocabulary::iris() {
        table.id(NamedNode::new_unchecked(iri).into());
    }
    let vocabulary = Vocabulary::new(&|iri| table.iri(iri));
    let back: Vec<Statement> = write(&ontology, &vocabulary, &mut table)
        .into_iter()
        .map(|triple| Statement { triple, graph: 0 })
        .collect();
    let again = read(&back, &table);
    let (before, after) = (rendered(&ontology, &table), rendered(&again, &table));
    if before != after {
        let missing: Vec<&String> = before.iter().filter(|l| !after.contains(l)).collect();
        return Err(format!("round trip lost {missing:?}"));
    }
    // Normalises (work package 2.2): no construct left unhandled.
    std::panic::catch_unwind(|| normalise(&ontology))
        .map_err(|_| "the normalisation panicked".to_owned())?;
    Ok(())
}

#[test]
fn the_dl_test_ontologies_read_and_round_trip() {
    let path = suite_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none(),
            "the W3C OWL 2 test cases are required: {}",
            path.display()
        );
        eprintln!(
            "skipped: no {} (scripts/fetch-w3c-tests.sh)",
            path.display()
        );
        return;
    };
    let triples = parse_rdf_xml(&text).expect("the test case collection parses");
    // Per test case: its name, whether it is of species DL, its RDF/XML ontologies.
    let mut names: HashMap<NamedOrBlankNode, String> = HashMap::new();
    let mut dl: HashMap<NamedOrBlankNode, bool> = HashMap::new();
    let mut ontologies: HashMap<NamedOrBlankNode, Vec<(String, String)>> = HashMap::new();
    for t in &triples {
        let Some(local) = t.predicate.as_str().strip_prefix(TEST) else {
            continue;
        };
        let text = match &t.object {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        };
        match local {
            "identifier" => {
                if let Some(name) = text {
                    names.insert(t.subject.clone(), name);
                }
            }
            "species" => {
                if matches!(&t.object, RdfTerm::NamedNode(n) if n.as_str() == format!("{TEST}DL")) {
                    dl.insert(t.subject.clone(), true);
                }
            }
            "rdfXmlPremiseOntology"
            | "rdfXmlConclusionOntology"
            | "rdfXmlNonConclusionOntology"
            | "rdfXmlInputOntology" => {
                if let Some(text) = text {
                    ontologies
                        .entry(t.subject.clone())
                        .or_default()
                        .push((local.to_owned(), text));
                }
            }
            _ => {}
        }
    }
    let expected: BTreeMap<&str, &str> = EXPECTED_FAILURES
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.split_once('\t')
                .map_or((l.trim(), ""), |(n, why)| (n.trim(), why))
        })
        .collect();
    let (mut checked, mut unexpected, mut passing_listed) = (0, Vec::new(), Vec::new());
    let mut cases: Vec<(&String, &Vec<(String, String)>)> = ontologies
        .iter()
        .filter(|(node, _)| dl.get(*node).copied().unwrap_or(false))
        .filter_map(|(node, list)| Some((names.get(node)?, list)))
        .collect();
    cases.sort();
    for (name, list) in cases {
        for (role, text) in list {
            let id = format!("{name} ({role})");
            checked += 1;
            match (check(text), expected.contains_key(id.as_str())) {
                (Ok(()), true) => passing_listed.push(id),
                (Err(why), false) => unexpected.push(format!("{id}: {why}")),
                _ => {}
            }
        }
    }
    eprintln!("{checked} ontologies of species DL checked");
    assert!(checked > 300, "only {checked} ontologies");
    assert!(
        unexpected.is_empty() && passing_listed.is_empty(),
        "failures not listed:\n{}\nlisted but passing:\n{}",
        unexpected.join("\n"),
        passing_listed.join("\n")
    );
}
