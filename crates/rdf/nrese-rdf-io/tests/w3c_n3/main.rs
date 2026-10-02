//! The W3C N3 Community Group's parser tests (w3c-cg/N3, `tests/N3Tests`): positive and
//! negative syntax, and evaluation against the expected statements.
//!
//! - **Source.** The pinned checkout `scripts/fetch-w3c-tests.sh` puts in `.cache/n3`, or
//!   wherever `NRESE_N3_TESTS` points. Without it the test is skipped, unless
//!   `NRESE_W3C_REQUIRED` is set.
//! - **Manifests** `manifest-parser.ttl` and `manifest-extended.ttl`, read with this
//!   crate's Turtle parser. Tests the group marked `rdft:Rejected` are not run.
//! - **Comparison.** N3 statements hold variables and literals where RDF can't, so both
//!   sides are mapped to RDF (a variable or a literal in subject or predicate position
//!   becomes a reserved IRI; a statement with a blank node as predicate becomes a node with
//!   its three parts) and compared up to blank node names.
//! - **Round trips.** Every document that parses is written with the N3 writer and read
//!   again: the same statements.
//! - **Streaming.** Every document is also read through a reader that gives one byte per
//!   call: the same statements, or the same failure.
//! - **Nesting.** One test nests formulas 1,080 deep; the parser's default limit (128, a
//!   guard for servers) is raised to 2,048 here, and the suite runs on a thread with a
//!   stack to match.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use nrese_rdf::{BlankNode, Dataset, NamedNode, NamedOrBlankNode, Quad, Term};
use nrese_rdf_io::n3::{N3Parser, N3Quad, N3Serializer, N3Term};
use nrese_rdf_io::{RdfFormat, RdfParser};

const BASE: &str = "https://w3c.github.io/N3/tests/N3Tests/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const RDFT: &str = "http://www.w3.org/ns/rdftest#";
const TEST: &str = "https://w3c.github.io/N3/tests/test.n3#";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Positive,
    Negative,
    Eval,
}

struct Test {
    iri: String,
    kind: Kind,
    action: String,
    result: Option<String>,
}

fn suite_root() -> Option<PathBuf> {
    let root = std::env::var_os("NRESE_N3_TESTS").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.cache/n3/tests/N3Tests"),
        PathBuf::from,
    );
    root.join("manifest-parser.ttl").is_file().then_some(root)
}

fn read_manifest(root: &Path, name: &str) -> Vec<Test> {
    let text = std::fs::read(root.join(name)).unwrap();
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(format!("{BASE}{name}"))
        .unwrap()
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    let mut by_subject: HashMap<String, Vec<(String, Term)>> = HashMap::new();
    for q in &quads {
        by_subject
            .entry(q.subject.to_string())
            .or_default()
            .push((q.predicate.as_str().to_owned(), q.object.clone()));
    }
    let get = |subject: &str, predicate: &str| -> Option<Term> {
        by_subject
            .get(subject)?
            .iter()
            .find(|(p, _)| p == predicate)
            .map(|(_, o)| o.clone())
    };
    let iri = |term: Option<Term>| match term {
        Some(Term::NamedNode(n)) => Some(n.into_string()),
        _ => None,
    };
    let mut tests = Vec::new();
    for (subject, properties) in &by_subject {
        let Some(kind) = properties.iter().find_map(|(p, o)| match (p.as_str(), o) {
            (p, Term::NamedNode(t)) if p == format!("{RDF}type") => {
                match t.as_str().strip_prefix(TEST) {
                    Some("TestN3PositiveSyntax") => Some(Kind::Positive),
                    Some("TestN3NegativeSyntax") => Some(Kind::Negative),
                    Some("TestN3Eval") => Some(Kind::Eval),
                    _ => None,
                }
            }
            _ => None,
        }) else {
            continue;
        };
        if iri(get(subject, &format!("{RDFT}approval"))).as_deref()
            == Some(&format!("{RDFT}Rejected"))
        {
            continue;
        }
        let Some(action) = iri(get(subject, &format!("{MF}action"))) else {
            continue;
        };
        tests.push(Test {
            iri: subject.clone(),
            kind,
            action,
            result: iri(get(subject, &format!("{MF}result"))),
        });
    }
    tests.sort_by(|a, b| a.iri.cmp(&b.iri));
    tests
}

fn file(root: &Path, iri: &str) -> PathBuf {
    root.join(
        iri.strip_prefix(BASE)
            .expect("a test file under the suite's base"),
    )
}

/// A reader that gives one byte per call.
struct Trickle<'a>(&'a [u8]);

impl std::io::Read for Trickle<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.0.split_first() {
            Some((&b, rest)) if !out.is_empty() => {
                out[0] = b;
                self.0 = rest;
                Ok(1)
            }
            _ => Ok(0),
        }
    }
}

/// A term as RDF can hold it in subject or predicate position.
fn node(term: &N3Term) -> NamedOrBlankNode {
    match term {
        N3Term::NamedNode(n) => n.clone().into(),
        N3Term::BlankNode(b) => b.clone().into(),
        other => NamedNode::new_unchecked(reserved(other)).into(),
    }
}

fn reserved(term: &N3Term) -> String {
    let text = term.to_string();
    let escaped: String = text
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("urn:nrese:n3:{escaped}")
}

fn object(term: &N3Term) -> Term {
    match term {
        N3Term::NamedNode(n) => n.clone().into(),
        N3Term::BlankNode(b) => b.clone().into(),
        N3Term::Literal(l) => l.clone().into(),
        N3Term::Triple(t) => Term::Triple(t.clone()),
        v @ N3Term::Variable(_) => NamedNode::new_unchecked(reserved(v)).into(),
    }
}

fn canonical(quads: &[N3Quad]) -> Dataset {
    let part = |name: &str| NamedNode::new_unchecked(format!("urn:nrese:n3:statement:{name}"));
    let mut rdf: Vec<Quad> = Vec::new();
    for (i, q) in quads.iter().enumerate() {
        match &q.predicate {
            N3Term::BlankNode(_) => {
                let statement: NamedOrBlankNode =
                    BlankNode::new_unchecked(format!("nrese-statement-{i}")).into();
                for (name, term) in [
                    ("subject", &q.subject),
                    ("predicate", &q.predicate),
                    ("object", &q.object),
                ] {
                    rdf.push(Quad {
                        subject: statement.clone(),
                        predicate: part(name),
                        object: object(term),
                        graph_name: q.graph_name.clone(),
                    });
                }
            }
            predicate => rdf.push(Quad {
                subject: node(&q.subject),
                predicate: match node(predicate) {
                    NamedOrBlankNode::NamedNode(n) => n,
                    NamedOrBlankNode::BlankNode(_) => unreachable!("handled above"),
                },
                object: object(&q.object),
                graph_name: q.graph_name.clone(),
            }),
        }
    }
    let mut dataset: Dataset = rdf.into_iter().collect();
    dataset.canonicalize();
    dataset
}

fn parse(root: &Path, iri: &str) -> Result<Vec<N3Quad>, String> {
    let bytes = std::fs::read(file(root, iri)).map_err(|e| e.to_string())?;
    let parser = N3Parser::new()
        .with_base_iri(iri)
        .map_err(|e| e.to_string())?
        .with_max_nesting(2048);
    let from_slice: Result<Vec<N3Quad>, String> = parser
        .clone()
        .for_slice(&bytes)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    let from_reader: Result<Vec<N3Quad>, String> = parser
        .for_reader(Trickle(&bytes))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    match (&from_slice, &from_reader) {
        (Ok(a), Ok(b)) if canonical(a) != canonical(b) => {
            return Err("a slice and a trickling reader give different statements".to_owned());
        }
        (Ok(_), Err(e)) => return Err(format!("only the trickling reader fails: {e}")),
        (Err(e), Ok(_)) => return Err(format!("only the slice fails: {e}")),
        _ => {}
    }
    from_slice
}

fn round_trip(quads: &[N3Quad]) -> Result<(), String> {
    let mut serializer = N3Serializer::new().with_prefix("ex", "http://example.org/");
    for quad in quads {
        serializer.serialize_quad(quad.clone());
    }
    let text = serializer.finish(Vec::new()).map_err(|e| e.to_string())?;
    let back: Vec<N3Quad> = N3Parser::new()
        .with_max_nesting(2048)
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("reading back: {e}\n{}", String::from_utf8_lossy(&text)))?;
    if canonical(&back) != canonical(quads) {
        return Err(format!(
            "the round trip changes the statements:\n{}",
            String::from_utf8_lossy(&text)
        ));
    }
    Ok(())
}

fn run(root: &Path, test: &Test) -> Result<(), String> {
    let action = parse(root, &test.action);
    match test.kind {
        Kind::Positive => round_trip(&action?),
        Kind::Negative => match action {
            Ok(_) => Err("parsed, but must not".to_owned()),
            Err(_) => Ok(()),
        },
        Kind::Eval => {
            let actual = action?;
            round_trip(&actual)?;
            let result = test
                .result
                .as_deref()
                .ok_or("an evaluation test without result")?;
            // N3 is a superset of N-Triples (some expected results have literal subjects).
            let expected: Vec<N3Quad> = if !result.ends_with(".nq") {
                parse(root, result).map_err(|e| format!("expected result: {e}"))?
            } else {
                let bytes = std::fs::read(file(root, result)).map_err(|e| e.to_string())?;
                RdfParser::from_format(RdfFormat::NQuads)
                    .for_slice(&bytes)
                    .map(|q| q.map(N3Quad::from))
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("expected result: {e}"))?
            };
            let (a, b) = (canonical(&actual), canonical(&expected));
            if a == b {
                Ok(())
            } else {
                Err(format!("different statements:\n{a}\nexpected:\n{b}"))
            }
        }
    }
}

fn expected_failures() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/w3c_n3/expected-failures.txt");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (iri, reason) = l.split_once(' ').unwrap_or((l, ""));
            (
                iri.to_owned(),
                reason.trim_start_matches(['#', ' ']).to_owned(),
            )
        })
        .collect()
}

#[test]
fn w3c_n3_parser_suites() {
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(suites)
        .unwrap()
        .join()
        .unwrap();
}

fn suites() {
    let Some(root) = suite_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|v| v.is_empty()),
            "W3C N3 tests not found; run scripts/fetch-w3c-tests.sh"
        );
        eprintln!("skipped: W3C N3 tests not found (run scripts/fetch-w3c-tests.sh)");
        return;
    };
    let expected = expected_failures();
    let mut unexpected = Vec::new();
    let mut seen = BTreeSet::new();
    println!("W3C N3 parser suites: passed / failed (expected failures)");
    for manifest in ["manifest-parser.ttl", "manifest-extended.ttl"] {
        let tests = read_manifest(&root, manifest);
        assert!(!tests.is_empty(), "no tests in {manifest}");
        let (mut passed, mut failed, mut known) = (0, 0, 0);
        for test in &tests {
            if !seen.insert(test.iri.clone()) {
                continue;
            }
            if std::env::var_os("NRESE_N3_TRACE").is_some() {
                eprintln!("{}", test.action);
            }
            let outcome = std::panic::catch_unwind(|| run(&root, test))
                .unwrap_or_else(|_| Err("panicked".to_owned()));
            match (outcome, expected.contains_key(&test.iri)) {
                (Ok(()), false) => passed += 1,
                (Ok(()), true) => unexpected.push(format!("passes, but is listed: {}", test.iri)),
                (Err(_), true) => {
                    failed += 1;
                    known += 1;
                }
                (Err(why), false) => {
                    failed += 1;
                    unexpected.push(format!("{} ({:?}): {why}", test.iri, test.kind));
                }
            }
        }
        println!("  {manifest}: {passed} / {failed} ({known})");
    }
    assert!(
        unexpected.is_empty(),
        "{} unexpected:\n{}",
        unexpected.len(),
        unexpected.join("\n")
    );
}
