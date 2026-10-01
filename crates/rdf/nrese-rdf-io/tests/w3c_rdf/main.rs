//! The W3C RDF 1.1 syntax test suites (w3c/rdf-tests): N-Triples, N-Quads, and the formats
//! this crate reads as they are added.
//!
//! - **Source.** The pinned checkout `scripts/fetch-w3c-tests.sh` puts in `.cache/rdf-tests`,
//!   or wherever `NRESE_W3C_TESTS` points. Without it the test is skipped, unless
//!   `NRESE_W3C_REQUIRED` is set (as in CI).
//! - **Kinds.** Positive syntax: the document parses. Negative syntax: it doesn't.
//!   Evaluation: the document's statements equal the expected N-Triples or N-Quads, up to
//!   blank node names (`nrese-rdf`'s canonicalisation).
//! - **Manifests** are Turtle, read with this crate's own parser.
//! - **Streaming.** Every document is also read through a reader that gives one byte per
//!   call, so that every token is cut by the buffer somewhere: the outcome must be the
//!   same (the same statements up to blank node names, or an error).
//! - **Round trips.** Every document that parses is written back, in its own format, as
//!   N-Quads and as JSON-LD (Turtle, TriG, RDF/XML and JSON-LD with a few prefixes, to
//!   exercise abbreviation), and read again: the statements must be the same.
//! - `expected-failures.txt` lists tests that fail on purpose, each with its reason; a new
//!   failure, or a pass of a listed test, fails the run.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use nrese_rdf::{Dataset, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};

const BASE: &str = "https://w3c.github.io/rdf-tests/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const RDFT: &str = "http://www.w3.org/ns/rdftest#";

/// The suites, by directory under `rdf/rdf11`.
const SUITES: [&str; 5] = [
    "rdf-n-triples",
    "rdf-n-quads",
    "rdf-turtle",
    "rdf-trig",
    "rdf-xml",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    PositiveSyntax,
    NegativeSyntax,
    Eval,
    NegativeEval,
}

struct Test {
    iri: String,
    kind: Kind,
    format: RdfFormat,
    action: String,
    result: Option<String>,
}

fn suite_root() -> Option<PathBuf> {
    let root = std::env::var_os("NRESE_W3C_TESTS").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.cache/rdf-tests"),
        PathBuf::from,
    );
    root.join("rdf/rdf11").is_dir().then_some(root)
}

/// The manifest's tests.
fn read_manifest(root: &Path, suite: &str) -> Vec<Test> {
    let directory = format!("{BASE}rdf/rdf11/{suite}/");
    let text = std::fs::read(root.join("rdf/rdf11").join(suite).join("manifest.ttl")).unwrap();
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(format!("{directory}manifest.ttl"))
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
    let object = |subject: &str, predicate: &str| -> Option<Term> {
        by_subject
            .get(subject)?
            .iter()
            .find(|(p, _)| p == predicate)
            .map(|(_, o)| o.clone())
    };
    // The entries list, in order.
    let mut entries = Vec::new();
    let mut node = object(
        &format!("<{directory}manifest.ttl>"),
        &format!("{MF}entries"),
    );
    while let Some(current) = node {
        let key = current.to_string();
        if key == format!("<{RDF}nil>") {
            break;
        }
        entries.extend(object(&key, &format!("{RDF}first")));
        node = object(&key, &format!("{RDF}rest"));
    }
    let local = |iri: &Term| match iri {
        Term::NamedNode(n) => n.as_str().to_owned(),
        other => other.to_string(),
    };
    entries
        .iter()
        .filter_map(|entry| {
            let key = entry.to_string();
            let kind_iri = local(&object(&key, &format!("{RDF}type"))?);
            let name = kind_iri.strip_prefix(RDFT)?;
            let (format, kind) = match name {
                "TestNTriplesPositiveSyntax" => (RdfFormat::NTriples, Kind::PositiveSyntax),
                "TestNTriplesNegativeSyntax" => (RdfFormat::NTriples, Kind::NegativeSyntax),
                "TestNQuadsPositiveSyntax" => (RdfFormat::NQuads, Kind::PositiveSyntax),
                "TestNQuadsNegativeSyntax" => (RdfFormat::NQuads, Kind::NegativeSyntax),
                "TestTurtlePositiveSyntax" => (RdfFormat::Turtle, Kind::PositiveSyntax),
                "TestTurtleNegativeSyntax" => (RdfFormat::Turtle, Kind::NegativeSyntax),
                "TestTurtleEval" => (RdfFormat::Turtle, Kind::Eval),
                "TestTurtleNegativeEval" => (RdfFormat::Turtle, Kind::NegativeEval),
                "TestTrigPositiveSyntax" => (RdfFormat::TriG, Kind::PositiveSyntax),
                "TestTrigNegativeSyntax" => (RdfFormat::TriG, Kind::NegativeSyntax),
                "TestTrigEval" => (RdfFormat::TriG, Kind::Eval),
                "TestTrigNegativeEval" => (RdfFormat::TriG, Kind::NegativeEval),
                "TestXMLEval" => (RdfFormat::RdfXml, Kind::Eval),
                "TestXMLNegativeSyntax" => (RdfFormat::RdfXml, Kind::NegativeSyntax),
                _ => return None,
            };
            Some(Test {
                iri: local(entry),
                kind,
                format,
                action: local(&object(&key, &format!("{MF}action"))?),
                result: object(&key, &format!("{MF}result")).map(|r| local(&r)),
            })
        })
        .collect()
}

/// The file behind a test IRI.
fn file(root: &Path, iri: &str) -> PathBuf {
    root.join(
        iri.strip_prefix(BASE)
            .expect("a test file IRI under the suite's base"),
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

fn canonical(quads: BTreeSet<Quad>) -> Dataset {
    let mut dataset: Dataset = quads.into_iter().collect();
    dataset.canonicalize();
    dataset
}

/// The statements of a document, or the parse error; read from a slice and through a
/// trickling reader, which must agree.
fn parse(root: &Path, format: RdfFormat, iri: &str) -> Result<BTreeSet<Quad>, String> {
    let bytes = std::fs::read(file(root, iri)).map_err(|e| e.to_string())?;
    let parser = RdfParser::from_format(format)
        .with_base_iri(iri)
        .map_err(|e| e.to_string())?;
    let from_slice: Result<BTreeSet<Quad>, String> = parser
        .clone()
        .for_slice(&bytes)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    let from_reader: Result<BTreeSet<Quad>, String> = parser
        .for_reader(Trickle(&bytes))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    match (&from_slice, from_reader) {
        (Ok(a), Ok(b)) if canonical(a.clone()) != canonical(b.clone()) => {
            return Err("a slice and a trickling reader give different statements".to_owned());
        }
        (Ok(_), Err(e)) => return Err(format!("only the trickling reader fails: {e}")),
        (Err(e), Ok(_)) => return Err(format!("only the slice fails: {e}")),
        _ => {}
    }
    from_slice
}

/// The statements written in `format` and read back must be the same.
fn round_trip(format: RdfFormat, quads: &BTreeSet<Quad>) -> Result<(), String> {
    for format in [format, RdfFormat::NQuads, RdfFormat::JsonLd] {
        let mut serializer = RdfSerializer::from_format(format);
        if matches!(
            format,
            RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::RdfXml | RdfFormat::JsonLd
        ) {
            for (name, iri) in [
                ("ex", "http://example/"),
                ("", "http://www.w3.org/2013/TurtleTests/"),
                ("xsd", "http://www.w3.org/2001/XMLSchema#"),
                ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            ] {
                serializer = serializer
                    .with_prefix(name, iri)
                    .map_err(|e| e.to_string())?;
            }
        }
        let mut writer = serializer.for_writer(Vec::new());
        for quad in quads {
            writer
                .serialize_quad(quad)
                .map_err(|e| format!("writing {format}: {e}"))?;
        }
        let text = writer.finish().map_err(|e| e.to_string())?;
        let read: BTreeSet<Quad> = RdfParser::from_format(format)
            .for_slice(&text)
            .collect::<Result<_, _>>()
            .map_err(|e| {
                format!(
                    "reading back {format}: {e}\n{}",
                    String::from_utf8_lossy(&text)
                )
            })?;
        if canonical(read) != canonical(quads.clone()) {
            return Err(format!(
                "{format} round trip changes the statements:\n{}",
                String::from_utf8_lossy(&text)
            ));
        }
    }
    Ok(())
}

/// Whether a test passes, and why not.
fn run(root: &Path, test: &Test) -> Result<(), String> {
    let action = parse(root, test.format, &test.action);
    match test.kind {
        Kind::PositiveSyntax => round_trip(test.format, &action?),
        Kind::NegativeSyntax | Kind::NegativeEval => match action {
            Ok(_) => Err("parsed, but must not".to_owned()),
            Err(_) => Ok(()),
        },
        Kind::Eval => {
            let actual = action?;
            round_trip(test.format, &actual)?;
            let result = test
                .result
                .as_deref()
                .ok_or("an evaluation test without result")?;
            let format = if result.ends_with(".nq") {
                RdfFormat::NQuads
            } else {
                RdfFormat::NTriples
            };
            let expected =
                parse(root, format, result).map_err(|e| format!("expected result: {e}"))?;
            let (a, b) = (canonical(actual), canonical(expected));
            if a == b {
                Ok(())
            } else {
                Err(format!("different statements:\n{a}\nexpected:\n{b}"))
            }
        }
    }
}

fn expected_failures() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/w3c_rdf/expected-failures.txt");
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
fn w3c_rdf_syntax_suites() {
    let Some(root) = suite_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|v| v.is_empty()),
            "W3C rdf-tests not found; run scripts/fetch-w3c-tests.sh"
        );
        eprintln!("skipped: W3C rdf-tests not found (run scripts/fetch-w3c-tests.sh)");
        return;
    };
    let expected = expected_failures();
    let mut unexpected = Vec::new();
    println!("W3C RDF 1.1 syntax suites: passed / failed (expected failures)");
    for suite in SUITES {
        let tests = read_manifest(&root, suite);
        assert!(!tests.is_empty(), "no tests in {suite}");
        let (mut passed, mut failed, mut known) = (0, 0, 0);
        for test in &tests {
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
        println!("  {suite}: {passed} / {failed} ({known})");
    }
    assert!(unexpected.is_empty(), "{}", unexpected.join("\n"));
}
