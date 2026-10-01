//! The W3C SPARQL 1.0, 1.1 and 1.2 test suites (w3c/rdf-tests) on the parser.
//!
//! - **Source.** The pinned checkout `scripts/fetch-w3c-tests.sh` puts in
//!   `.cache/rdf-tests/sparql`. Without it the test is skipped, unless `NRESE_W3C_REQUIRED`
//!   is set.
//! - **Kinds.** Positive and negative syntax tests of queries and updates; the query and
//!   update of every evaluation test count as positive syntax tests.
//! - **Round trip.** Every query and update that parses is written back and parsed again,
//!   and must give the same algebra (generated names compared by position).
//! - **Expected failures** are listed with their reasons in `expected-failures.txt`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use nrese_rdf::{Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql_syntax::SparqlParser;

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const QT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-query#";
const UT: &str = "http://www.w3.org/2009/sparql/tests/test-update#";
const DAWGT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-dawg#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    PositiveQuery,
    NegativeQuery,
    PositiveUpdate,
    NegativeUpdate,
}

struct Test {
    iri: String,
    kind: Kind,
    file: String,
}

fn sparql_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.cache/rdf-tests/sparql");
    root.join("sparql11/manifest-all.ttl")
        .is_file()
        .then_some(root)
}

fn file_iri(path: &Path) -> String {
    let path = path
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    let path = path.trim_start_matches("//?/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn iri_path(iri: &str) -> PathBuf {
    let path = iri.strip_prefix("file://").unwrap();
    let path = if path.as_bytes().get(2) == Some(&b':') {
        &path[1..] // file:///C:/…
    } else {
        path
    };
    PathBuf::from(path.replace("%20", " "))
}

/// The statements of a manifest, by subject.
struct Graph {
    by_subject: HashMap<String, Vec<(String, Term)>>,
}

impl Graph {
    fn read(path: &Path) -> Self {
        let text = std::fs::read(path).unwrap();
        let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
            .with_base_iri(file_iri(path))
            .unwrap()
            .for_slice(&text)
            .collect::<Result<_, _>>()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut by_subject: HashMap<String, Vec<(String, Term)>> = HashMap::new();
        for q in quads {
            by_subject
                .entry(key(&q.subject.into()))
                .or_default()
                .push((q.predicate.as_str().to_owned(), q.object));
        }
        Self { by_subject }
    }

    fn objects<'g>(&'g self, subject: &str, predicate: &str) -> impl Iterator<Item = &'g Term> {
        self.by_subject
            .get(subject)
            .into_iter()
            .flatten()
            .filter(move |(p, _)| p == predicate)
            .map(|(_, o)| o)
    }

    fn object(&self, subject: &str, predicate: &str) -> Option<&Term> {
        self.objects(subject, predicate).next()
    }

    fn list(&self, head: &Term) -> Vec<Term> {
        let mut out = Vec::new();
        let mut node = head.clone();
        while key(&node) != format!("<{RDF}nil>") {
            let k = key(&node);
            let Some(first) = self.object(&k, &format!("{RDF}first")) else {
                break;
            };
            out.push(first.clone());
            node = self.object(&k, &format!("{RDF}rest")).unwrap().clone();
        }
        out
    }
}

fn key(term: &Term) -> String {
    term.to_string()
}

fn iri(term: &Term) -> Option<&str> {
    match term {
        Term::NamedNode(n) => Some(n.as_str()),
        _ => None,
    }
}

fn collect(manifest: &Path, out: &mut Vec<Test>, seen: &mut HashSet<PathBuf>) {
    if !seen.insert(manifest.to_path_buf()) {
        return;
    }
    let graph = Graph::read(manifest);
    for (subject, properties) in &graph.by_subject {
        for (predicate, object) in properties {
            if predicate == &format!("{MF}include") {
                for included in graph.list(object) {
                    collect(&iri_path(iri(&included).unwrap()), out, seen);
                }
            }
            if predicate == &format!("{MF}entries") {
                for entry in graph.list(object) {
                    if let Some(test) = test(&graph, &key(&entry)) {
                        out.push(test);
                    }
                }
            }
        }
        let _ = subject;
    }
}

fn test(graph: &Graph, subject: &str) -> Option<Test> {
    let kind_iri = iri(graph.object(subject, &format!("{RDF}type"))?)?;
    let local = kind_iri.strip_prefix(MF)?;
    let approval = graph
        .object(subject, &format!("{DAWGT}approval"))
        .and_then(iri)
        .unwrap_or_default();
    if approval.ends_with("Withdrawn") || approval.ends_with("Rejected") {
        return None;
    }
    let action = graph.object(subject, &format!("{MF}action"))?;
    let (kind, file) = match local {
        "PositiveSyntaxTest" | "PositiveSyntaxTest11" => (Kind::PositiveQuery, iri(action)?),
        "NegativeSyntaxTest" | "NegativeSyntaxTest11" => (Kind::NegativeQuery, iri(action)?),
        "PositiveUpdateSyntaxTest" | "PositiveUpdateSyntaxTest11" => {
            (Kind::PositiveUpdate, iri(action)?)
        }
        "NegativeUpdateSyntaxTest" | "NegativeUpdateSyntaxTest11" => {
            (Kind::NegativeUpdate, iri(action)?)
        }
        "QueryEvaluationTest" => (
            Kind::PositiveQuery,
            iri(graph.object(&key(action), &format!("{QT}query"))?)?,
        ),
        "UpdateEvaluationTest" => (
            Kind::PositiveUpdate,
            iri(graph.object(&key(action), &format!("{UT}request"))?)?,
        ),
        _ => return None,
    };
    Some(Test {
        iri: subject.trim_matches(['<', '>']).to_owned(),
        kind,
        file: file.to_owned(),
    })
}

/// A `Debug` rendering with generated names (`__agg0`, `___b3`, …) replaced by their
/// order of first appearance.
fn normalised(debug: &str) -> String {
    let mut names: HashMap<String, usize> = HashMap::new();
    let mut out = String::with_capacity(debug.len());
    let mut rest = debug;
    while let Some(i) = rest.find("\"__") {
        out.push_str(&rest[..=i]);
        let after = &rest[i + 1..];
        let end = after.find('"').unwrap_or(after.len());
        let name = &after[..end];
        let core = name.trim_start_matches('_');
        let generated = name.len() - core.len() >= 2
            && core
                .trim_end_matches(|c: char| c.is_ascii_digit())
                .chars()
                .all(|c| c.is_ascii_lowercase());
        if generated {
            let n = names.len();
            let id = *names.entry(name.to_owned()).or_insert(n);
            out.push_str(&format!("#gen{id}"));
        } else {
            out.push_str(name);
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn expected_failures() -> HashSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/w3c_syntax/expected-failures.txt");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split('#').next())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn w3c_sparql_syntax() {
    let Some(root) = sparql_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none(),
            "the W3C suites are required: run scripts/fetch-w3c-tests.sh"
        );
        eprintln!("skipped: no W3C suites (scripts/fetch-w3c-tests.sh)");
        return;
    };
    let mut tests = Vec::new();
    let mut seen = HashSet::new();
    for manifest in [
        "sparql10/manifest.ttl",
        "sparql11/manifest-all.ttl",
        "sparql12/manifest.ttl",
    ] {
        collect(&root.join(manifest), &mut tests, &mut seen);
    }
    // One test per file and kind (evaluation tests share query files).
    let mut unique: BTreeMap<(String, String), Test> = BTreeMap::new();
    for t in tests {
        unique
            .entry((t.file.clone(), format!("{:?}", t.kind)))
            .or_insert(t);
    }
    let expected = expected_failures();
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut fixed = Vec::new();
    for test in unique.values() {
        let text = match std::fs::read_to_string(iri_path(&test.file)) {
            Ok(text) => text,
            Err(e) => {
                failures.push(format!("{}: unreadable {}: {e}", test.iri, test.file));
                continue;
            }
        };
        let parser = SparqlParser::new()
            .with_base_iri(test.file.as_str())
            .unwrap();
        let outcome: Result<(), String> = match test.kind {
            Kind::PositiveQuery => match parser.parse_query(&text) {
                Ok(query) => {
                    let written = query.to_string();
                    match parser.parse_query(&written) {
                        Ok(again)
                            if normalised(&format!("{again:?}"))
                                == normalised(&format!("{query:?}")) =>
                        {
                            Ok(())
                        }
                        Ok(_) => Err(format!("written back differently:\n{written}")),
                        Err(e) => Err(format!("written back unparseable ({e}):\n{written}")),
                    }
                }
                Err(e) => Err(format!("rejected: {e}")),
            },
            Kind::PositiveUpdate => match parser.parse_update(&text) {
                Ok(update) => {
                    let written = update.to_string();
                    match parser.parse_update(&written) {
                        Ok(again)
                            if normalised(&format!("{again:?}"))
                                == normalised(&format!("{update:?}")) =>
                        {
                            Ok(())
                        }
                        Ok(_) => Err(format!("written back differently:\n{written}")),
                        Err(e) => Err(format!("written back unparseable ({e}):\n{written}")),
                    }
                }
                Err(e) => Err(format!("rejected: {e}")),
            },
            Kind::NegativeQuery => match parser.parse_query(&text) {
                Ok(_) => Err("accepted".into()),
                Err(_) => Ok(()),
            },
            Kind::NegativeUpdate => match parser.parse_update(&text) {
                Ok(_) => Err("accepted".into()),
                Err(_) => Ok(()),
            },
        };
        let suite = test
            .file
            .split("/sparql/")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .unwrap_or("?")
            .to_owned();
        let entry = counts
            .entry(format!("{suite} {:?}", test.kind))
            .or_default();
        entry.1 += 1;
        match outcome {
            Ok(()) => {
                entry.0 += 1;
                if expected.contains(&test.iri) {
                    fixed.push(test.iri.clone());
                }
            }
            Err(why) if expected.contains(&test.iri) => {
                let _ = why;
            }
            Err(why) => failures.push(format!("{} ({}): {why}", test.iri, test.file)),
        }
    }
    for (suite, (passed, total)) in &counts {
        eprintln!("{suite}: {passed}/{total}");
    }
    assert!(
        failures.is_empty() && fixed.is_empty(),
        "{} unexpected failures:\n{}\n\npassing now, remove from expected-failures.txt:\n{}",
        failures.len(),
        failures.join("\n\n"),
        fixed.join("\n")
    );
}
