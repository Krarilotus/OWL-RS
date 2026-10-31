//! C1 and C3 gates: the W3C SHACL test suite (Core, and SHACL-SPARQL) against
//! `nrese-shacl`.
//!
//! - **Source.** The pinned `w3c/data-shapes` checkout fetched by
//!   `scripts/fetch-w3c-tests.sh` into `.cache/data-shapes`, or wherever
//!   `NRESE_W3C_SHACL_TESTS` points (the suite's `tests` directory). Without it the test is
//!   skipped, unless `NRESE_W3C_REQUIRED` is set (as in CI).
//! - **Comparison.** A test passes if the report graph is the expected report as the suite
//!   compares them ([`projection`]): isomorphic once only the compared fields are kept, with
//!   a message only where the expected report has it. Every test's report graph is written
//!   to `<target tmp>/w3c-shacl-reports/`.
//! - **Failures.** A test whose result is `sht:Failure` passes if the shapes graph is
//!   refused as ill-formed, or validation reports a failure. A suite file that can't be
//!   read or parsed, or an error of the harness's own, fails it.
//! - **Expected failures.** Every known failure is listed with its reason in
//!   `expected-failures.txt` (Core) and `expected-failures-sparql.txt` (SHACL-SPARQL). The run fails on any failure not in the list, and on any
//!   listed test that now passes. The full report is written to
//!   `<target tmp>/w3c-shacl-report.txt`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use nrese_engine::{Engine, EngineConfig, GraphSelector, TermId};
use nrese_rdf::{
    Graph, GraphName, GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, QuadRef, TermRef,
    Triple,
};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{Selection, compile, validate};

mod projection;

const EXPECTED_FAILURES: &str = include_str!("expected-failures.txt");
const EXPECTED_FAILURES_SPARQL: &str = include_str!("expected-failures-sparql.txt");

/// The IRI the suite's `tests` directory is parsed under.
const BASE: &str = "http://w3c.test/shacl/";

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const SHT: &str = "http://www.w3.org/ns/shacl-test#";
const SH: &str = "http://www.w3.org/ns/shacl#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

fn suite_root() -> Option<PathBuf> {
    let root = std::env::var_os("NRESE_W3C_SHACL_TESTS").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.cache/data-shapes/data-shapes-test-suite/tests")
        },
        PathBuf::from,
    );
    root.join("core/manifest.ttl").is_file().then_some(root)
}

fn iri(namespace: &str, local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{namespace}{local}"))
}

/// The triples of the suite file `file` (an IRI under [`BASE`]).
fn parse(root: &Path, file: NamedNodeRef<'_>) -> Result<Vec<Triple>, String> {
    let relative = file
        .as_str()
        .strip_prefix(BASE)
        .ok_or_else(|| format!("{file} is outside the suite"))?;
    let path = root.join(relative);
    let text = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(file.as_str())
        .map_err(|error| error.to_string())?
        .for_slice(&text)
        .map(|quad| {
            quad.map(|quad| Triple::new(quad.subject, quad.predicate, quad.object))
                .map_err(|error| format!("{}: {error}", path.display()))
        })
        .collect()
}

fn object<'a>(
    graph: &'a Graph,
    subject: NamedOrBlankNodeRef<'a>,
    predicate: &NamedNode,
) -> Option<TermRef<'a>> {
    graph.object_for_subject_predicate(subject, predicate)
}

fn node(term: TermRef<'_>) -> Option<NamedOrBlankNodeRef<'_>> {
    match term {
        TermRef::NamedNode(node) => Some(node.into()),
        TermRef::BlankNode(node) => Some(node.into()),
        TermRef::Literal(_) | TermRef::Triple(_) => None,
    }
}

/// The members of the RDF list at `head`.
fn list<'a>(graph: &'a Graph, head: TermRef<'a>) -> Vec<TermRef<'a>> {
    let (first, rest) = (iri(RDF, "first"), iri(RDF, "rest"));
    let mut members = Vec::new();
    let mut current = head;
    while let Some(cell) = node(current) {
        let Some(member) = object(graph, cell, &first) else {
            break;
        };
        members.push(member);
        match object(graph, cell, &rest) {
            Some(next) => current = next,
            None => break,
        }
    }
    members
}

struct Test {
    id: String,
    /// The file that declares the test: it holds the expected report.
    file: NamedNode,
    data: NamedNode,
    shapes: NamedNode,
}

/// The `sht:Validate` tests of `manifest` and the manifests it includes.
fn tests(root: &Path, manifest: &NamedNode, out: &mut Vec<Test>) -> Result<(), String> {
    let graph: Graph = parse(root, manifest.as_ref())?.into_iter().collect();
    let include = iri(MF, "include");
    for triple in graph.triples_for_predicate(&include) {
        if let TermRef::NamedNode(included) = triple.object {
            tests(root, &included.into_owned(), out)?;
        }
    }
    let (entries, action) = (iri(MF, "entries"), iri(MF, "action"));
    let (data, shapes) = (iri(SHT, "dataGraph"), iri(SHT, "shapesGraph"));
    for triple in graph.triples_for_predicate(&entries) {
        for entry in list(&graph, triple.object) {
            let TermRef::NamedNode(entry) = entry else {
                continue;
            };
            let graphs = object(&graph, entry.into(), &action).and_then(node);
            let file = |predicate| match graphs.and_then(|a| object(&graph, a, predicate)) {
                Some(TermRef::NamedNode(file)) => Ok(file.into_owned()),
                _ => Err(format!("{entry}: no data or shapes graph")),
            };
            out.push(Test {
                id: entry.to_string(),
                file: manifest.clone(),
                data: file(&data)?,
                shapes: file(&shapes)?,
            });
        }
    }
    Ok(())
}

/// Why a run gave no outcome.
enum Refusal {
    /// The processor's own: the shapes graph is ill-formed, or validation failed. What a
    /// `sht:Failure` test expects.
    Processor(String),
    /// Anything else: a suite file, the engine, the report graph. No test expects it.
    Harness(String),
}

impl From<String> for Refusal {
    fn from(why: String) -> Self {
        Self::Harness(why)
    }
}

impl From<Refusal> for String {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Processor(why) | Refusal::Harness(why) => why,
        }
    }
}

/// The report graph of validating the test's data graph against its shapes graph.
fn actual(root: &Path, test: &Test, file: &[Triple]) -> Result<Graph, Refusal> {
    let triples = |graph: &NamedNode| -> Result<Vec<Triple>, String> {
        if *graph == test.file {
            Ok(file.to_vec())
        } else {
            parse(root, graph.as_ref())
        }
    };
    let engine = Engine::new(EngineConfig::default()).map_err(|error| error.to_string())?;
    // The shapes go to a graph named after their file, as in a repository: SHACL-SPARQL
    // reads it as `$shapesGraph`. A shapes graph that is the data graph is in the default
    // graph as data too.
    let shapes_graph = test.shapes.as_ref();
    let mut tx = engine.transaction();
    for triple in &triples(&test.data)? {
        tx.insert(QuadRef::new(
            &triple.subject,
            &triple.predicate,
            &triple.object,
            GraphNameRef::DefaultGraph,
        ));
    }
    for triple in &triples(&test.shapes)? {
        tx.insert(QuadRef::new(
            &triple.subject,
            &triple.predicate,
            &triple.object,
            shapes_graph,
        ));
    }
    tx.commit().map_err(|error| error.to_string())?;
    let snapshot = engine.snapshot();
    let shapes_selector = match snapshot.lookup(shapes_graph.into()) {
        Some(graph) => GraphSelector::Exact(graph),
        None => return Err("empty shapes graph".to_owned().into()),
    };
    let shapes = compile(&snapshot, Selection::asserted(shapes_selector)).map_err(|errors| {
        let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
        Refusal::Processor(format!("ill-formed shapes: {}", errors.join("; ")))
    })?;
    let report = validate(
        &snapshot,
        &shapes,
        Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
    );
    if !report.failures.is_empty() {
        return Err(Refusal::Processor(format!(
            "validation failed: {}",
            report.failures.join("; ")
        )));
    }
    Ok(report.to_triples().into_iter().collect())
}

/// `Ok` if the test passes, else what differs. The report graph, if any, is written to
/// `reports`.
fn run(root: &Path, test: &Test, reports: &Path) -> Result<(), String> {
    let file = parse(root, test.file.as_ref())?;
    let graph: Graph = file.iter().cloned().collect();
    let entry = NamedNode::new_unchecked(test.id.trim_matches(['<', '>']));
    let expected =
        object(&graph, entry.as_ref().into(), &iri(MF, "result")).ok_or("no mf:result")?;
    let actual = actual(root, test, &file);
    if let Ok(report) = &actual {
        let name = test.id.trim_matches(['<', '>']);
        let name = name
            .strip_prefix(BASE)
            .unwrap_or(name)
            .replace(['/', ':', '#'], "_");
        let lines: String = report
            .iter()
            .map(|triple| format!("{triple} .\n"))
            .collect();
        std::fs::write(reports.join(format!("{name}.nt")), lines)
            .map_err(|error| format!("the report graph not kept: {error}"))?;
    }
    if expected == TermRef::NamedNode(iri(SHT, "Failure").as_ref()) {
        return match actual {
            Err(Refusal::Processor(_)) => Ok(()),
            Err(Refusal::Harness(why)) => Err(format!("expected a failure, but {why}")),
            Ok(_) => Err("expected a failure (ill-formed shapes or a validation failure)".into()),
        };
    }
    let expected = node(expected).ok_or("no mf:result")?;
    projection::compare(&graph, expected, &actual?)
}

/// The cases every reader of the suite's report comparison passes (`projection-cases.trig`):
/// each an expected and an actual report graph, and whether the suite counts them the same.
#[test]
fn report_projection_cases() {
    const CASES: &str = "http://w3c.test/shacl/projection-cases#";
    let quads: Vec<_> = RdfParser::from_format(RdfFormat::TriG)
        .for_slice(include_bytes!("projection-cases.trig"))
        .collect::<Result<_, _>>()
        .expect("projection-cases.trig");
    let graph = |name: TermRef<'_>| -> Graph {
        let TermRef::NamedNode(name) = name else {
            panic!("{name} names no graph");
        };
        quads
            .iter()
            .filter(|quad| quad.graph_name == GraphName::NamedNode(name.into_owned()))
            .map(|quad| {
                Triple::new(
                    quad.subject.clone(),
                    quad.predicate.clone(),
                    quad.object.clone(),
                )
            })
            .collect()
    };
    let cases: Graph = quads
        .iter()
        .filter(|quad| quad.graph_name == GraphName::DefaultGraph)
        .map(|quad| {
            Triple::new(
                quad.subject.clone(),
                quad.predicate.clone(),
                quad.object.clone(),
            )
        })
        .collect();
    let mut wrong = Vec::new();
    let mut count = 0;
    for case in cases.triples_for_predicate(&iri(CASES, "same")) {
        count += 1;
        let named = |local: &str| {
            let name = object(&cases, case.subject, &iri(CASES, local));
            graph(name.unwrap_or_else(|| panic!("{}: no {local}", case.subject)))
        };
        let (expected, actual) = (named("expected"), named("actual"));
        let root = projection::report_node(&expected).expect("the expected report's node");
        let verdict = projection::compare(&expected, root, &actual);
        let same = matches!(case.object, TermRef::Literal(same) if same.value() == "true");
        if verdict.is_ok() != same {
            wrong.push(format!("{}: {verdict:?}", case.subject));
        }
    }
    assert!(count >= 25, "only {count} cases");
    assert!(
        wrong.is_empty(),
        "cases the comparison gets wrong:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn w3c_shacl_core() {
    suite(
        "core/manifest.ttl",
        "W3C SHACL Core",
        98,
        EXPECTED_FAILURES,
        "w3c-shacl-report.txt",
    );
}

#[test]
fn w3c_shacl_sparql() {
    suite(
        "sparql/manifest.ttl",
        "W3C SHACL-SPARQL",
        20,
        EXPECTED_FAILURES_SPARQL,
        "w3c-shacl-sparql-report.txt",
    );
}

/// Runs the tests of `manifest` (under the suite's `tests` directory) and checks the
/// failures against `expected_failures`.
fn suite(manifest: &str, name: &str, at_least: usize, expected_failures: &str, report_file: &str) {
    let Some(root) = suite_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|value| value.is_empty()),
            "W3C data-shapes tests not found; run scripts/fetch-w3c-tests.sh"
        );
        eprintln!(
            "not qualified: W3C data-shapes tests not found (run scripts/fetch-w3c-tests.sh)"
        );
        return;
    };
    let mut all = Vec::new();
    tests(&root, &iri(BASE, manifest), &mut all).expect("manifests");
    assert!(all.len() >= at_least, "only {} tests found", all.len());
    let reports = Path::new(env!("CARGO_TARGET_TMPDIR")).join("w3c-shacl-reports");
    std::fs::create_dir_all(&reports).expect("the reports' directory");

    let outcomes: Vec<(&Test, Result<(), String>)> = all
        .iter()
        .map(|test| {
            let outcome = catch_unwind(AssertUnwindSafe(|| run(&root, test, &reports)))
                .unwrap_or_else(|_| Err("panicked".to_owned()));
            (test, outcome)
        })
        .collect();

    let failed: BTreeSet<&str> = outcomes
        .iter()
        .filter(|(_, outcome)| outcome.is_err())
        .map(|(test, _)| test.id.as_str())
        .collect();
    let mut report = format!(
        "{name}: {} of {} pass\n",
        outcomes.len() - failed.len(),
        outcomes.len()
    );
    for (test, outcome) in &outcomes {
        if let Err(difference) = outcome {
            writeln!(report, "FAIL {}\n    {difference}", test.id).unwrap();
        }
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(report_file);
    std::fs::write(&path, &report).expect("report");
    eprintln!("{}", report.lines().next().unwrap_or_default());
    eprintln!("full report: {}", path.display());

    // Entries are `<iri> # reason`.
    let listed: BTreeSet<&str> = expected_failures
        .lines()
        .filter(|line| line.starts_with('<'))
        .filter_map(|line| line.find('>').map(|end| &line[..=end]))
        .collect();
    let new_failures: Vec<_> = failed.difference(&listed).collect();
    let fixed: Vec<_> = listed.difference(&failed).collect();
    assert!(
        new_failures.is_empty() && fixed.is_empty(),
        "new failures (fix, or list them with a reason): {new_failures:#?}\n\
         listed but now passing (remove them from the expected failures): {fixed:#?}"
    );
}
