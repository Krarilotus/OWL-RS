//! C1 and C3 gates: the W3C SHACL test suite (Core, and SHACL-SPARQL) against
//! `nrese-shacl`.
//!
//! - **Source.** The pinned `w3c/data-shapes` checkout fetched by
//!   `scripts/fetch-w3c-tests.sh` into `.cache/data-shapes`, or wherever
//!   `NRESE_W3C_SHACL_TESTS` points (the suite's `tests` directory). Without it the test is
//!   skipped, unless `NRESE_W3C_REQUIRED` is set (as in CI).
//! - **Comparison.** A test passes if the report's `sh:conforms` and its results equal the
//!   expected ones as multisets of (focus node, path, value, source shape, component,
//!   severity). Blank nodes compare equal to each other; messages aren't compared (the
//!   specification leaves them free).
//! - **Failures.** A test whose result is `sht:Failure` passes if the shapes graph is
//!   refused as ill-formed, or validation reports a failure.
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
    Graph, GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, QuadRef, Term, TermRef,
    Triple,
};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{PropertyPath, Selection, compile, validate};

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

/// A term as results are compared: blank nodes are all alike.
fn key(term: TermRef<'_>) -> String {
    match term {
        TermRef::BlankNode(_) => "_:".to_owned(),
        other => other.to_string(),
    }
}

/// The path an expected report gives as `sh:resultPath`.
fn expected_path(graph: &Graph, term: TermRef<'_>) -> Result<PropertyPath, String> {
    let blank = match term {
        TermRef::NamedNode(predicate) => {
            return Ok(PropertyPath::Predicate(predicate.into_owned()));
        }
        TermRef::BlankNode(blank) => NamedOrBlankNodeRef::from(blank),
        TermRef::Literal(_) | TermRef::Triple(_) => {
            return Err("a literal or triple term as path".to_owned());
        }
    };
    let inner = |local: &str| {
        object(graph, blank, &iri(SH, local))
            .map(|inner| expected_path(graph, inner).map(Box::new))
            .transpose()
    };
    let members = |head: TermRef<'_>| -> Result<Vec<PropertyPath>, String> {
        list(graph, head)
            .into_iter()
            .map(|member| expected_path(graph, member))
            .collect()
    };
    if let Some(path) = inner("inversePath")? {
        Ok(PropertyPath::Inverse(path))
    } else if let Some(path) = inner("zeroOrMorePath")? {
        Ok(PropertyPath::ZeroOrMore(path))
    } else if let Some(path) = inner("oneOrMorePath")? {
        Ok(PropertyPath::OneOrMore(path))
    } else if let Some(path) = inner("zeroOrOnePath")? {
        Ok(PropertyPath::ZeroOrOne(path))
    } else if let Some(head) = object(graph, blank, &iri(SH, "alternativePath")) {
        Ok(PropertyPath::Alternative(members(head)?))
    } else {
        Ok(PropertyPath::Sequence(members(term)?))
    }
}

/// `sh:conforms` and the sorted result keys.
type Outcome = (bool, Vec<String>);

/// `None` if the test expects a failure (`sht:Failure`).
fn expected(graph: &Graph, test: &Test) -> Result<Option<Outcome>, String> {
    let entry = NamedNode::new_unchecked(test.id.trim_matches(['<', '>']));
    let result = object(graph, entry.as_ref().into(), &iri(MF, "result")).ok_or("no mf:result")?;
    if result == TermRef::NamedNode(iri(SHT, "Failure").as_ref()) {
        return Ok(None);
    }
    let report = node(result).ok_or("no mf:result")?;
    let conforms = match object(graph, report, &iri(SH, "conforms")) {
        Some(TermRef::Literal(literal)) => literal.value() == "true",
        _ => return Err("no sh:conforms".to_owned()),
    };
    let mut results = Vec::new();
    for result in graph.objects_for_subject_predicate(report, &iri(SH, "result")) {
        let result = node(result).ok_or("a literal as result")?;
        let field = |local: &str| object(graph, result, &iri(SH, local));
        let path = field("resultPath")
            .map(|path| expected_path(graph, path))
            .transpose()?;
        results.push(result_key(
            field("focusNode").map(key),
            path.as_ref(),
            field("value").map(key),
            field("sourceShape").map(key),
            field("sourceConstraintComponent").map(key),
            field("resultSeverity").map(key),
        ));
    }
    results.sort();
    Ok(Some((conforms, results)))
}

fn result_key(
    focus: Option<String>,
    path: Option<&PropertyPath>,
    value: Option<String>,
    shape: Option<String>,
    component: Option<String>,
    severity: Option<String>,
) -> String {
    let text = |part: Option<String>| part.unwrap_or_else(|| "-".to_owned());
    format!(
        "focus {} | path {} | value {} | shape {} | {} | {}",
        text(focus),
        text(path.map(ToString::to_string)),
        text(value),
        text(shape),
        text(component),
        text(severity)
    )
}

fn actual(root: &Path, test: &Test, file: &[Triple]) -> Result<Outcome, String> {
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
        None => return Err("empty shapes graph".to_owned()),
    };
    let shapes = compile(&snapshot, Selection::asserted(shapes_selector)).map_err(|errors| {
        let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
        format!("ill-formed shapes: {}", errors.join("; "))
    })?;
    let report = validate(
        &snapshot,
        &shapes,
        Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
    );
    if !report.failures.is_empty() {
        return Err(format!("validation failed: {}", report.failures.join("; ")));
    }
    let mut results: Vec<String> = report
        .results
        .iter()
        .map(|result| {
            let term = |term: &Term| key(term.as_ref());
            result_key(
                Some(term(&result.focus_node)),
                result.path.as_ref(),
                result.value.as_ref().map(term),
                Some(term(&result.source_shape)),
                Some(format!("<{}>", result.component.as_str())),
                Some(result.severity.to_string()),
            )
        })
        .collect();
    results.sort();
    // The report's RDF form must state the same.
    let graph: Graph = report.to_triples().into_iter().collect();
    let count = graph.triples_for_predicate(&iri(SH, "result")).count();
    if count != report.results.len() {
        return Err(format!("the report graph has {count} results"));
    }
    Ok((report.conforms(), results))
}

/// `Ok` if the test passes, else what differs.
fn run(root: &Path, test: &Test) -> Result<(), String> {
    let file = parse(root, test.file.as_ref())?;
    let graph: Graph = file.iter().cloned().collect();
    let Some(expected) = expected(&graph, test)? else {
        return match actual(root, test, &file) {
            Err(_) => Ok(()),
            Ok(_) => Err("expected a failure (ill-formed shapes or a validation failure)".into()),
        };
    };
    let actual = actual(root, test, &file)?;
    if expected == actual {
        return Ok(());
    }
    let mut difference = format!("conforms: expected {}, got {}", expected.0, actual.0);
    for line in &expected.1 {
        if !actual.1.contains(line) {
            write!(difference, "\n    missing   {line}").unwrap();
        }
    }
    for line in &actual.1 {
        if !expected.1.contains(line) {
            write!(difference, "\n    unexpected {line}").unwrap();
        }
    }
    if expected.1.len() != actual.1.len() {
        write!(
            difference,
            "\n    expected {} results, got {}",
            expected.1.len(),
            actual.1.len()
        )
        .unwrap();
    }
    Err(difference)
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
        eprintln!("skipped: W3C data-shapes tests not found (run scripts/fetch-w3c-tests.sh)");
        return;
    };
    let mut all = Vec::new();
    tests(&root, &iri(BASE, manifest), &mut all).expect("manifests");
    assert!(all.len() >= at_least, "only {} tests found", all.len());

    let outcomes: Vec<(&Test, Result<(), String>)> = all
        .iter()
        .map(|test| {
            let outcome = catch_unwind(AssertUnwindSafe(|| run(&root, test)))
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
