//! The W3C RDF Dataset Canonicalization test suite (w3c/rdf-canon, RDFC-1.0) on
//! `nrese_rdf::rdfc`.
//!
//! - **Source.** The pinned checkout `scripts/fetch-w3c-tests.sh` puts in
//!   `.cache/rdf-canon`. Without it the test is skipped, unless `NRESE_W3C_REQUIRED` is set.
//! - **Kinds.** Evaluation: the canonical N-Quads document, byte for byte. Map: the label
//!   each input blank node gets. Negative: a poison graph, which must stop at the work
//!   limit instead of running on.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use nrese_json::Value;
use nrese_rdf::rdfc::{HashAlgorithm, Rdfc10};
use nrese_rdf::{Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser};

const BASE: &str = "https://w3c.github.io/rdf-canon/tests/";
const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFC: &str = "https://w3c.github.io/rdf-canon/tests/vocab#";

fn suite_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.cache/rdf-canon/tests");
    root.join("manifest.ttl").is_file().then_some(root)
}

struct Test {
    iri: String,
    kind: String,
    action: String,
    result: Option<String>,
    hash: HashAlgorithm,
}

fn tests(root: &Path) -> Vec<Test> {
    let text = std::fs::read(root.join("manifest.ttl")).unwrap();
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(format!("{BASE}manifest.ttl"))
        .unwrap()
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    let mut by_subject: BTreeMap<String, HashMap<String, Term>> = BTreeMap::new();
    for q in quads {
        by_subject
            .entry(q.subject.to_string())
            .or_default()
            .insert(q.predicate.as_str().to_owned(), q.object);
    }
    let iri = |t: &Term| match t {
        Term::NamedNode(n) => n.as_str().to_owned(),
        other => other.to_string(),
    };
    by_subject
        .into_iter()
        .filter_map(|(subject, properties)| {
            let kind = iri(properties.get(RDF_TYPE)?)
                .strip_prefix(RDFC)?
                .to_owned();
            let hash = match properties.get(&format!("{RDFC}hashAlgorithm")) {
                Some(Term::Literal(l)) if l.value() == "SHA384" => HashAlgorithm::Sha384,
                _ => HashAlgorithm::Sha256,
            };
            Some(Test {
                iri: subject,
                kind,
                action: iri(properties.get(&format!("{MF}action"))?),
                result: properties.get(&format!("{MF}result")).map(iri),
                hash,
            })
        })
        .collect()
}

fn file(root: &Path, iri: &str) -> PathBuf {
    root.join(
        iri.strip_prefix(BASE)
            .expect("a test file under the suite's base"),
    )
}

fn run(root: &Path, test: &Test) -> Result<(), String> {
    let input = std::fs::read(file(root, &test.action)).map_err(|e| e.to_string())?;
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&input)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let canonical = Rdfc10::new()
        .with_hash(test.hash)
        .canonicalize(quads.iter().map(Quad::as_ref));
    let result = || -> Result<String, String> {
        let result = test.result.as_deref().ok_or("a test without result")?;
        std::fs::read_to_string(file(root, result)).map_err(|e| e.to_string())
    };
    match test.kind.as_str() {
        "RDFC10NegativeEvalTest" => match canonical {
            Ok(_) => Err("canonicalised a poison graph".to_owned()),
            Err(_) => Ok(()),
        },
        "RDFC10EvalTest" => {
            let written = canonical.map_err(|e| e.to_string())?.to_nquads();
            let expected = result()?;
            if written == expected {
                Ok(())
            } else {
                Err(format!("written:\n{written}expected:\n{expected}"))
            }
        }
        "RDFC10MapTest" => {
            let canonical = canonical.map_err(|e| e.to_string())?;
            let actual: BTreeMap<String, String> = canonical
                .issued
                .iter()
                .map(|(input, label)| (input.as_str().to_owned(), label.as_str().to_owned()))
                .collect();
            let expected = result()?;
            let Value::Object(map) = Value::parse(&expected).map_err(|e| e.to_string())? else {
                return Err("the expected map isn't an object".to_owned());
            };
            let expected: BTreeMap<String, String> = map
                .iter()
                .filter_map(|(k, v)| match v {
                    Value::String(s) => Some((k.to_string(), s.to_string())),
                    _ => None,
                })
                .collect();
            if actual == expected {
                Ok(())
            } else {
                Err(format!("issued {actual:?}, expected {expected:?}"))
            }
        }
        other => Err(format!("an unknown kind of test: {other}")),
    }
}

#[test]
fn w3c_rdfc10_suite() {
    let Some(root) = suite_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|v| v.is_empty()),
            "W3C rdf-canon tests not found; run scripts/fetch-w3c-tests.sh"
        );
        eprintln!("skipped: W3C rdf-canon tests not found (run scripts/fetch-w3c-tests.sh)");
        return;
    };
    let tests = tests(&root);
    assert!(tests.len() > 80, "only {} tests", tests.len());
    let failures: Vec<String> = tests
        .iter()
        .filter_map(|test| {
            run(&root, test)
                .err()
                .map(|why| format!("{} ({}): {why}", test.iri, test.kind))
        })
        .collect();
    println!(
        "W3C RDFC-1.0: {} of {} passed",
        tests.len() - failures.len(),
        tests.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
