//! The W3C JSON-LD 1.1 test suites (w3c/json-ld-api): `toRdf`, `expand` and `fromRdf`.
//!
//! - **Source.** The pinned checkout `scripts/fetch-w3c-tests.sh` puts in
//!   `.cache/json-ld-api`, or wherever `NRESE_JSONLD_TESTS` points. Without it the test is
//!   skipped, unless `NRESE_W3C_REQUIRED` is set (as in CI).
//! - **Remote documents** are read from the checkout by a document loader that maps the
//!   suite's base IRI to its directory (the parser itself reads none by default).
//! - **toRdf**: the quads must equal the expected N-Quads up to blank node names; a
//!   negative test must fail with the expected error code. Every document is also read
//!   through a reader, which must agree.
//! - **expand**: the expanded document must equal the expected one as JSON-LD compares
//!   them (arrays are sets, but for `@list`).
//! - **fromRdf**: the document the expanded writer makes of the N-Quads must equal the
//!   expected one, compared the same way.
//! - **Round trips.** The quads of every toRdf test are written with both JSON-LD writers
//!   (streaming, compacted with prefixes; and expanded) and read back: the same quads.
//! - Tests for JSON-LD 1.0 processors only, and tests of generalized RDF (blank node
//!   predicates, which RDF can't hold), are not run.
//! - `expected-failures.txt` lists tests that fail on purpose, each with its reason; a new
//!   failure, or a pass of a listed test, fails the run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use nrese_json::Value;
use nrese_rdf::{Dataset, Quad};
use nrese_rdf_io::jsonld::{
    self, FromRdfOptions, JsonLdOptions, JsonLdProcessingMode, RdfDirection, RemoteDocument,
};
use nrese_rdf_io::{RdfFormat, RdfParseError, RdfParser, RdfSerializer};

const BASE: &str = "https://w3c.github.io/json-ld-api/tests/";

fn suite_root() -> Option<PathBuf> {
    let root = std::env::var_os("NRESE_JSONLD_TESTS").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.cache/json-ld-api/tests"),
        PathBuf::from,
    );
    root.join("toRdf-manifest.jsonld").is_file().then_some(root)
}

struct Test {
    id: String,
    types: Vec<String>,
    input: String,
    expect: Option<String>,
    error: Option<String>,
    option: BTreeMap<String, Value<'static>>,
}

impl Test {
    fn is(&self, kind: &str) -> bool {
        self.types.iter().any(|t| t == kind)
    }
}

fn read_manifest(root: &Path, name: &str) -> Vec<Test> {
    let text = std::fs::read_to_string(root.join(name)).unwrap();
    let manifest = Value::parse(&text).unwrap().into_owned();
    let sequence = manifest
        .as_object()
        .and_then(|m| m.get("sequence"))
        .unwrap();
    let string = |o: &nrese_json::Object<'_>, key: &str| {
        o.get(key).and_then(Value::as_str).map(str::to_owned)
    };
    sequence
        .as_items()
        .iter()
        .map(|test| {
            let test = test.as_object().unwrap();
            Test {
                id: format!("{name}{}", string(test, "@id").unwrap()),
                types: test
                    .get("@type")
                    .unwrap()
                    .as_items()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                input: string(test, "input").unwrap(),
                expect: string(test, "expect"),
                error: string(test, "expectErrorCode"),
                option: test
                    .get("option")
                    .and_then(Value::as_object)
                    .map(|o| o.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// Whether the suite asks for something this processor doesn't do.
fn skipped(test: &Test) -> bool {
    test.option.get("specVersion").and_then(Value::as_str) == Some("json-ld-1.0")
        || test
            .option
            .get("produceGeneralizedRdf")
            .and_then(Value::as_bool)
            == Some(true)
}

/// The options and the base IRI of a test.
fn options(root: &Path, test: &Test) -> (JsonLdOptions, String) {
    let root = root.to_path_buf();
    let mut options = JsonLdOptions::new().with_document_loader(move |url| {
        let path = url
            .strip_prefix(BASE)
            .ok_or_else(|| format!("{url} is outside the test suite"))?;
        let document = std::fs::read_to_string(root.join(path)).map_err(|e| e.to_string())?;
        Ok(RemoteDocument {
            document_url: url.to_owned(),
            document,
        })
    });
    if test.option.get("processingMode").and_then(Value::as_str) == Some("json-ld-1.0") {
        options = options.with_processing_mode(JsonLdProcessingMode::JsonLd10);
    }
    match test.option.get("rdfDirection").and_then(Value::as_str) {
        Some("i18n-datatype") => options = options.with_rdf_direction(RdfDirection::I18nDatatype),
        Some("compound-literal") => {
            options = options.with_rdf_direction(RdfDirection::CompoundLiteral)
        }
        _ => {}
    }
    if let Some(context) = test.option.get("expandContext").and_then(Value::as_str) {
        let text = std::fs::read_to_string(suite_root().unwrap().join(context)).unwrap();
        options = options.with_expand_context(&text).unwrap();
    }
    let base = test
        .option
        .get("base")
        .and_then(Value::as_str)
        .map_or_else(|| format!("{BASE}{}", test.input), str::to_owned);
    (options, base)
}

fn canonical(quads: BTreeSet<Quad>) -> Dataset {
    let mut dataset: Dataset = quads.into_iter().collect();
    dataset.canonicalize();
    dataset
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

fn message(error: &RdfParseError) -> String {
    match error {
        RdfParseError::Syntax(e) => e.message().to_owned(),
        RdfParseError::Io(e) => e.to_string(),
    }
}

fn to_rdf(root: &Path, test: &Test) -> Result<(), String> {
    let (options, base) = options(root, test);
    let bytes = std::fs::read(root.join(&test.input)).map_err(|e| e.to_string())?;
    let parser = RdfParser::from_format(RdfFormat::JsonLd)
        .with_base_iri(base)
        .map_err(|e| e.to_string())?
        .with_json_ld_options(options);
    let from_slice: Result<BTreeSet<Quad>, String> = parser
        .clone()
        .for_slice(&bytes)
        .collect::<Result<_, _>>()
        .map_err(|e| message(&e));
    let from_reader: Result<BTreeSet<Quad>, String> = parser
        .for_reader(Trickle(&bytes))
        .collect::<Result<_, _>>()
        .map_err(|e| message(&e));
    match (&from_slice, &from_reader) {
        (Ok(a), Ok(b)) if canonical(a.clone()) != canonical(b.clone()) => {
            return Err("a slice and a reader give different quads".to_owned());
        }
        (Ok(_), Err(e)) => return Err(format!("only the reader fails: {e}")),
        (Err(e), Ok(_)) => return Err(format!("only the slice fails: {e}")),
        _ => {}
    }
    if let Some(code) = &test.error {
        return match from_slice {
            Ok(_) => Err(format!("parsed, but must fail with {code:?}")),
            Err(e) if e.starts_with(code.as_str()) => Ok(()),
            Err(e) => Err(format!("fails with {e:?}, not {code:?}")),
        };
    }
    let actual = from_slice?;
    round_trip(&actual)?;
    let Some(expect) = &test.expect else {
        return Ok(());
    };
    let expected: BTreeSet<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&std::fs::read(root.join(expect)).map_err(|e| e.to_string())?)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("expected result: {e}"))?;
    let (a, b) = (canonical(actual), canonical(expected));
    if a == b {
        Ok(())
    } else {
        Err(format!("different quads:\n{a}\nexpected:\n{b}"))
    }
}

/// The quads written with each JSON-LD writer and read back must be the same.
fn round_trip(quads: &BTreeSet<Quad>) -> Result<(), String> {
    for expanded in [false, true] {
        let mut serializer = RdfSerializer::from_format(RdfFormat::JsonLd);
        if expanded {
            serializer = serializer.with_json_ld_expanded(FromRdfOptions::default());
        } else {
            for (name, iri) in [
                ("ex", "http://example.org/"),
                ("xsd", "http://www.w3.org/2001/XMLSchema#"),
                ("v", "http://example.org/vocab#"),
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
                .map_err(|e| format!("writing: {e}"))?;
        }
        let text = writer.finish().map_err(|e| e.to_string())?;
        let back: BTreeSet<Quad> = RdfParser::from_format(RdfFormat::JsonLd)
            .for_slice(&text)
            .collect::<Result<_, _>>()
            .map_err(|e| format!("reading back: {e}\n{}", String::from_utf8_lossy(&text)))?;
        if canonical(back) != canonical(quads.clone()) {
            let form = if expanded { "expanded" } else { "streaming" };
            return Err(format!(
                "the {form} round trip changes the quads:\n{}",
                String::from_utf8_lossy(&text)
            ));
        }
    }
    Ok(())
}

fn from_rdf(root: &Path, test: &Test) -> Result<(), String> {
    let bytes = std::fs::read(root.join(&test.input)).map_err(|e| e.to_string())?;
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&bytes)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let flag = |key: &str| test.option.get(key).and_then(Value::as_bool) == Some(true);
    let options = FromRdfOptions {
        use_native_types: flag("useNativeTypes"),
        use_rdf_type: flag("useRdfType"),
        rdf_direction: match test.option.get("rdfDirection").and_then(Value::as_str) {
            Some("i18n-datatype") => Some(RdfDirection::I18nDatatype),
            Some("compound-literal") => Some(RdfDirection::CompoundLiteral),
            _ => None,
        },
        processing_mode: if test.option.get("processingMode").and_then(Value::as_str)
            == Some("json-ld-1.0")
        {
            JsonLdProcessingMode::JsonLd10
        } else {
            JsonLdProcessingMode::JsonLd11
        },
    };
    let result = jsonld::from_rdf(quads.iter().map(Quad::as_ref), &options);
    if let Some(code) = &test.error {
        return match result {
            Ok(_) => Err(format!("serialised, but must fail with {code:?}")),
            Err(e) if e.code().as_str() == code => Ok(()),
            Err(e) => Err(format!("fails with {e:?}, not {code:?}")),
        };
    }
    let actual = result.map_err(|e| e.to_string())?;
    let expect = test.expect.as_deref().ok_or("no expected result")?;
    let expected_text = std::fs::read_to_string(root.join(expect)).map_err(|e| e.to_string())?;
    let expected = Value::parse(&expected_text).map_err(|e| e.to_string())?;
    if same(&actual, &expected, false) {
        Ok(())
    } else {
        Err(format!("serialised:\n{actual}\nexpected:\n{expected}"))
    }
}

/// JSON-LD equality of expanded documents: arrays are compared as multisets except the
/// value of `@list`; numbers by value.
fn same(a: &Value<'_>, b: &Value<'_>, ordered: bool) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return false;
            }
            if ordered {
                return x.iter().zip(y).all(|(p, q)| same(p, q, false));
            }
            let mut used = vec![false; y.len()];
            x.iter().all(|p| {
                let found = y
                    .iter()
                    .enumerate()
                    .position(|(i, q)| !used[i] && same(p, q, false));
                found.map(|i| used[i] = true).is_some()
            })
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same(v, w, k == "@list")))
        }
        (Value::Number(_), Value::Number(_)) => a.as_f64() == b.as_f64(),
        _ => a == b,
    }
}

fn expand(root: &Path, test: &Test) -> Result<(), String> {
    let (options, base) = options(root, test);
    let text = std::fs::read_to_string(root.join(&test.input)).map_err(|e| e.to_string())?;
    let result = jsonld::expand(&text, Some(&base), &options);
    if let Some(code) = &test.error {
        return match result {
            Ok(_) => Err(format!("expanded, but must fail with {code:?}")),
            Err(e) if e.code().as_str() == code => Ok(()),
            Err(e) => Err(format!("fails with {e:?}, not {code:?}")),
        };
    }
    let actual = result.map_err(|e| e.to_string())?;
    let Some(expect) = &test.expect else {
        return Ok(());
    };
    let expected_text = std::fs::read_to_string(root.join(expect)).map_err(|e| e.to_string())?;
    let expected = Value::parse(&expected_text).map_err(|e| e.to_string())?;
    if same(&actual, &expected, false) {
        Ok(())
    } else {
        Err(format!("expanded:\n{actual}\nexpected:\n{expected}"))
    }
}

fn expected_failures() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/w3c_jsonld/expected-failures.txt");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (id, reason) = l.split_once(' ').unwrap_or((l, ""));
            (
                id.to_owned(),
                reason.trim_start_matches(['#', ' ']).to_owned(),
            )
        })
        .collect()
}

#[test]
fn w3c_json_ld_suites() {
    let Some(root) = suite_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|v| v.is_empty()),
            "W3C json-ld-api tests not found; run scripts/fetch-w3c-tests.sh"
        );
        eprintln!("skipped: W3C json-ld-api tests not found (run scripts/fetch-w3c-tests.sh)");
        return;
    };
    let expected = expected_failures();
    let mut unexpected = Vec::new();
    println!("W3C JSON-LD 1.1 suites: passed / failed (expected failures) / not run");
    type Runner = fn(&Path, &Test) -> Result<(), String>;
    let suites: [(&str, Runner); 3] = [
        ("toRdf-manifest.jsonld", to_rdf),
        ("expand-manifest.jsonld", expand),
        ("fromRdf-manifest.jsonld", from_rdf),
    ];
    for (manifest, runner) in suites {
        let tests = read_manifest(&root, manifest);
        assert!(!tests.is_empty(), "no tests in {manifest}");
        let (mut passed, mut failed, mut known, mut not_run) = (0, 0, 0, 0);
        for test in &tests {
            if skipped(test) {
                not_run += 1;
                continue;
            }
            assert!(
                test.is("jld:PositiveEvaluationTest")
                    || test.is("jld:NegativeEvaluationTest")
                    || test.is("jld:PositiveSyntaxTest"),
                "{}: unknown kind {:?}",
                test.id,
                test.types
            );
            let outcome = std::panic::catch_unwind(|| runner(&root, test))
                .unwrap_or_else(|_| Err("panicked".to_owned()));
            match (outcome, expected.contains_key(&test.id)) {
                (Ok(()), false) => passed += 1,
                (Ok(()), true) => unexpected.push(format!("passes, but is listed: {}", test.id)),
                (Err(_), true) => {
                    failed += 1;
                    known += 1;
                }
                (Err(why), false) => {
                    failed += 1;
                    unexpected.push(format!("{}: {why}", test.id));
                }
            }
        }
        println!("  {manifest}: {passed} / {failed} ({known}) / {not_run}");
    }
    assert!(
        unexpected.is_empty(),
        "{} unexpected:\n{}",
        unexpected.len(),
        unexpected.join("\n")
    );
}
