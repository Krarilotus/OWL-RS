//! Q1 gate: the W3C SPARQL 1.1 test suites (query evaluation, update evaluation, query and
//! update syntax, CSV/TSV/JSON results) against `nrese-sparql` on engine v2.
//!
//! - **Source.** The suite is the pinned `w3c/rdf-tests` checkout fetched by
//!   `scripts/fetch-w3c-tests.sh` into `.cache/rdf-tests`, or wherever `NRESE_W3C_TESTS`
//!   points. Without it the test is skipped, unless `NRESE_W3C_REQUIRED` is set (as in CI).
//! - **Reference.** Every test also runs on the reference evaluator
//!   (`nrese-sparql-reference`: the specification's algebra, evaluated plainly), which
//!   shares the executor's function semantics. A failure on both is a function's or the
//!   test format's; a failure only on ours is the executor's.
//! - **Not covered here.** Protocol, Graph Store Protocol, service description and
//!   federation tests are HTTP-level and belong to the server's suites.
//! - **Expected failures.** Every known failure of ours is listed with its reason in
//!   `expected-failures.txt`. The run fails on any failure not in the list, and on any listed
//!   test that now passes, so the list never goes stale. The full report is written to
//!   `<target tmp>/w3c-sparql11-report.txt`.

mod manifest;
mod results;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

use manifest::{GraphFile, Kind, Suite, Test};
use nrese_engine::{Engine, EngineConfig, QuadPattern};
use nrese_sparql::{QueryOptions, QueryResults, UpdateOptions, apply_update, evaluate_query};
use oxrdf::graph::CanonicalizationAlgorithm;
use oxrdf::{BlankNode, Dataset, GraphName, NamedNode, NamedOrBlankNode, Quad, Term, Variable};
use results::{Results, canonical, parse_expected};
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use spargebra::{Query, SparqlParser, Update};
use std::cell::RefCell;

const EXPECTED_FAILURES: &str = include_str!("expected-failures.txt");

#[derive(Debug)]
enum Outcome {
    Passed,
    Failed(String),
    Skipped(&'static str),
}

impl Outcome {
    fn failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

struct Row {
    kind: Kind,
    ours: Outcome,
    oracle: Outcome,
}

#[test]
fn w3c_sparql11() {
    let Some(root) = manifest::suite_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|value| value.is_empty()),
            "W3C rdf-tests not found; run scripts/fetch-w3c-tests.sh"
        );
        eprintln!("skipped: W3C rdf-tests not found (run scripts/fetch-w3c-tests.sh)");
        return;
    };
    let suite = Suite::new(root);
    let mut tests = Vec::new();
    for manifest in [
        "manifest-sparql11-query.ttl",
        "manifest-sparql11-update.ttl",
        "manifest-sparql11-results.ttl",
    ] {
        tests.extend(suite.tests(manifest).expect("manifest"));
    }
    let rows: BTreeMap<String, Row> = tests
        .iter()
        .map(|test| {
            let row = Row {
                kind: test.kind,
                ours: outcome(&suite, test, Backend::ours),
                oracle: outcome(&suite, test, Backend::oracle),
            };
            (test.id.clone(), row)
        })
        .collect();

    // Entries are `<iri> # reason`; the IRIs contain `#` themselves.
    let expected: BTreeSet<&str> = EXPECTED_FAILURES
        .lines()
        .filter(|line| line.starts_with('<'))
        .filter_map(|line| line.find('>').map(|end| &line[..=end]))
        .collect();
    let failed: BTreeSet<&str> = rows
        .iter()
        .filter(|(_, row)| row.ours.failed())
        .map(|(id, _)| id.as_str())
        .collect();
    let report = report(&rows);
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("w3c-sparql11-report.txt");
    std::fs::write(&path, &report).expect("report");
    eprintln!("{}", report.lines().take(12).collect::<Vec<_>>().join("\n"));
    eprintln!("full report: {}", path.display());

    let new_failures: Vec<_> = failed.difference(&expected).collect();
    let fixed: Vec<_> = expected.difference(&failed).collect();
    assert!(
        new_failures.is_empty() && fixed.is_empty(),
        "new failures (fix, or list them with a reason): {new_failures:#?}\n\
         listed but now passing (remove from expected-failures.txt): {fixed:#?}"
    );
}

fn outcome(suite: &Suite, test: &Test, backend: fn() -> Backend) -> Outcome {
    if test.needs_service {
        return Outcome::Skipped("needs a remote SERVICE endpoint");
    }
    match catch_unwind(AssertUnwindSafe(|| run(suite, test, backend()))) {
        Ok(Ok(())) => Outcome::Passed,
        Ok(Err(message)) => Outcome::Failed(message),
        Err(_) => Outcome::Failed("panicked".to_owned()),
    }
}

fn report(rows: &BTreeMap<String, Row>) -> String {
    // passed, failed, skipped for ours; then failures shared with the oracle.
    let mut by_kind: BTreeMap<Kind, [usize; 4]> = BTreeMap::new();
    let mut oracle_passed = 0;
    for row in rows.values() {
        let counts = by_kind.entry(row.kind).or_default();
        match row.ours {
            Outcome::Passed => counts[0] += 1,
            Outcome::Failed(_) => counts[1] += 1,
            Outcome::Skipped(_) => counts[2] += 1,
        }
        if row.ours.failed() && row.oracle.failed() {
            counts[3] += 1;
        }
        oracle_passed += usize::from(matches!(row.oracle, Outcome::Passed));
    }
    let mut out = String::from(
        "W3C SPARQL 1.1 on engine v2: passed / failed / skipped (failures shared with the reference)\n",
    );
    let mut total = [0; 4];
    for (kind, counts) in &by_kind {
        let _ = writeln!(
            out,
            "  {kind:?}: {} / {} / {} ({})",
            counts[0], counts[1], counts[2], counts[3]
        );
        for (sum, count) in total.iter_mut().zip(counts) {
            *sum += count;
        }
    }
    let _ = writeln!(
        out,
        "  total: {} / {} / {} ({})\n  reference evaluator passed: {oracle_passed}\n",
        total[0], total[1], total[2], total[3]
    );
    for (id, row) in rows {
        match &row.ours {
            Outcome::Failed(message) => {
                let scope = match row.oracle.failed() {
                    true => "shared with the reference",
                    false => "ours only",
                };
                let message: String = message.chars().take(3_000).collect();
                let _ = writeln!(out, "FAIL ({scope}) {id}\n  {message}\n");
            }
            Outcome::Skipped(reason) => {
                let _ = writeln!(out, "SKIP {id}: {reason}");
            }
            Outcome::Passed if row.oracle.failed() => {
                let _ = writeln!(out, "PASS (reference fails) {id}");
            }
            Outcome::Passed => {}
        }
    }
    out
}

/// Where a test runs.
enum Backend {
    Ours(Engine),
    Oracle(RefCell<nrese_sparql_reference::Dataset>),
}

impl Backend {
    fn ours() -> Self {
        Self::Ours(
            Engine::new(EngineConfig {
                background_maintenance: false,
                ..EngineConfig::default()
            })
            .expect("engine"),
        )
    }

    fn oracle() -> Self {
        Self::Oracle(RefCell::default())
    }

    fn insert(&self, quads: &[Quad]) -> Result<(), String> {
        match self {
            Self::Ours(engine) => {
                let mut tx = engine.transaction();
                for quad in quads {
                    tx.insert(quad.as_ref());
                }
                tx.commit().map(drop).map_err(|error| error.to_string())
            }
            Self::Oracle(dataset) => {
                let mut dataset = dataset.borrow_mut();
                for quad in quads {
                    dataset.insert(quad.clone());
                }
                Ok(())
            }
        }
    }

    /// Evaluates `query` and hands the results to `consume` while the view is alive.
    fn query<T>(
        &self,
        query: &Query,
        consume: impl FnOnce(QueryResults<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let failed =
            |error: nrese_sparql::QueryEvaluationError| format!("evaluation failed: {error}");
        match self {
            Self::Ours(engine) => {
                let snapshot = engine.snapshot();
                consume(evaluate_query(&snapshot, query, &QueryOptions::default()).map_err(failed)?)
            }
            Self::Oracle(dataset) => consume(
                dataset
                    .borrow()
                    .query(query, &QueryOptions::default())
                    .map_err(failed)?,
            ),
        }
    }

    fn update(&self, update: &Update) -> Result<(), String> {
        match self {
            Self::Ours(engine) => {
                let mut tx = engine.transaction();
                apply_update(&mut tx, update, &UpdateOptions::default())
                    .map_err(|error| format!("update failed: {error}"))?;
                tx.commit().map(drop).map_err(|error| error.to_string())
            }
            Self::Oracle(dataset) => dataset
                .borrow_mut()
                .update(update, &QueryOptions::default())
                .map_err(|error| format!("update failed: {error}")),
        }
    }

    fn dataset(&self) -> Dataset {
        let mut dataset: Dataset = match self {
            Self::Ours(engine) => {
                let snapshot = engine.snapshot();
                snapshot
                    .quads_for_pattern(&QuadPattern::all())
                    .map(|quad| snapshot.decode_quad(quad).expect("decodable"))
                    .collect()
            }
            Self::Oracle(dataset) => dataset.borrow().quads().cloned().collect(),
        };
        dataset.canonicalize(CanonicalizationAlgorithm::Unstable);
        dataset
    }
}

fn run(suite: &Suite, test: &Test, backend: Backend) -> Result<(), String> {
    let action = test.action.as_ref().ok_or("test has no action")?;
    let text = suite.read(action.as_ref())?;
    let parser = || {
        SparqlParser::new()
            .with_base_iri(action.as_str())
            .map_err(|error| error.to_string())
    };
    match test.kind {
        Kind::PositiveSyntax => parser()?
            .parse_query(&text)
            .map(drop)
            .map_err(|error| format!("rejected a valid query: {error}")),
        Kind::NegativeSyntax => match parser()?.parse_query(&text) {
            Ok(_) => Err("accepted an invalid query".to_owned()),
            Err(_) => Ok(()),
        },
        Kind::PositiveUpdateSyntax => parser()?
            .parse_update(&text)
            .map(drop)
            .map_err(|error| format!("rejected a valid update: {error}")),
        Kind::NegativeUpdateSyntax => match parser()?.parse_update(&text) {
            Ok(_) => Err("accepted an invalid update".to_owned()),
            Err(_) => Ok(()),
        },
        Kind::QueryEvaluation | Kind::CsvResultFormat => {
            let query = parser()?
                .parse_query(&text)
                .map_err(|error| format!("parse: {error}"))?;
            run_query(suite, test, &query, &backend)
        }
        Kind::UpdateEvaluation => {
            let update = parser()?
                .parse_update(&text)
                .map_err(|error| format!("parse: {error}"))?;
            backend.insert(&quads(suite, &test.data)?)?;
            backend.update(&update)?;
            let actual = backend.dataset();
            let mut expected: Dataset = quads(suite, &test.expected_data)?.into_iter().collect();
            expected.canonicalize(CanonicalizationAlgorithm::Unstable);
            match actual == expected {
                true => Ok(()),
                false => Err(format!("expected dataset\n{expected}\ngot\n{actual}")),
            }
        }
    }
}

/// The quads of `files`. Each file is its own document, so its blank nodes are its own.
fn quads(suite: &Suite, files: &[GraphFile]) -> Result<Vec<Quad>, String> {
    let mut quads = Vec::new();
    for (document, file) in files.iter().enumerate() {
        let graph_name = file
            .name
            .clone()
            .map_or(GraphName::DefaultGraph, GraphName::from);
        let scoped =
            |node: &BlankNode| BlankNode::new_unchecked(format!("d{document}x{}", node.as_str()));
        for triple in &suite.parse_graph(file.file.as_ref())? {
            let mut quad = triple.into_owned().in_graph(graph_name.clone());
            if let NamedOrBlankNode::BlankNode(node) = &quad.subject {
                quad.subject = scoped(node).into();
            }
            if let Term::BlankNode(node) = &quad.object {
                quad.object = scoped(node).into();
            }
            quads.push(quad);
        }
    }
    Ok(quads)
}

fn run_query(suite: &Suite, test: &Test, query: &Query, backend: &Backend) -> Result<(), String> {
    let mut files = test.data.clone();
    // Graphs named in FROM / FROM NAMED are suite files; load them under their IRIs.
    for iri in dataset_iris(query) {
        let loaded = files.iter().any(|file| file.name.as_ref() == Some(&iri));
        if !loaded
            && suite
                .local_path(iri.as_ref())
                .is_some_and(|path| path.is_file())
        {
            files.push(GraphFile {
                name: Some(iri.clone()),
                file: iri,
            });
        }
    }
    backend.insert(&quads(suite, &files)?)?;
    let result_iri = test.result.as_ref().ok_or("no expected result")?;
    let extension = result_iri.as_str().rsplit('.').next().unwrap_or("");
    let expected_text = suite.read(result_iri.as_ref())?;

    if test.kind == Kind::CsvResultFormat || extension == "csv" {
        return backend.query(query, |results| compare_csv(results, &expected_text));
    }
    let expected = parse_expected(extension, &expected_text, || {
        suite.parse_graph(result_iri.as_ref())
    })?;
    let ordered = results::is_ordered(&expected);
    let actual = backend.query(query, |results| comparable(results, ordered))?;
    match actual == expected {
        true => Ok(()),
        false => Err(format!("expected {expected:?}\ngot {actual:?}")),
    }
}

fn comparable(results: QueryResults<'_>, ordered: bool) -> Result<Results, String> {
    let failed = |error: nrese_sparql::QueryEvaluationError| format!("evaluation failed: {error}");
    Ok(match results {
        QueryResults::Boolean(value) => Results::Boolean(value),
        QueryResults::Solutions(solutions) => {
            let rows = solutions
                .map(|solution| {
                    solution.map(|solution| {
                        solution
                            .iter()
                            .map(|(variable, term)| (variable.clone(), term.clone()))
                            .collect::<Vec<(Variable, Term)>>()
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(failed)?;
            results::solutions(rows, ordered)
        }
        QueryResults::Graph(triples) => Results::Graph(canonical(
            triples.collect::<Result<_, _>>().map_err(failed)?,
        )),
    })
}

/// CSV loses datatypes, so the comparison is on the text of each cell: rows as a multiset,
/// columns matched by name (`SELECT *` leaves the column order to the implementation),
/// blank-node labels ignored (they are only unique per document).
fn compare_csv(results: QueryResults<'_>, expected: &str) -> Result<(), String> {
    let QueryResults::Solutions(solutions) = results else {
        return Err("CSV test without solutions".to_owned());
    };
    let mut writer = QueryResultsSerializer::from_format(QueryResultsFormat::Csv)
        .serialize_solutions_to_writer(Vec::new(), solutions.variables().to_vec())
        .map_err(|error| error.to_string())?;
    for solution in solutions {
        let solution = solution.map_err(|error| error.to_string())?;
        writer
            .serialize(&solution)
            .map_err(|error| error.to_string())?;
    }
    let actual = String::from_utf8(writer.finish().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    match csv_table(&actual) == csv_table(expected) {
        true => Ok(()),
        false => Err(format!("expected CSV\n{expected}\ngot\n{actual}")),
    }
}

/// Sorted rows of `(column, cell)` pairs, each row sorted by column name.
fn csv_table(text: &str) -> Vec<Vec<(String, String)>> {
    let mut records = csv_records(text).into_iter();
    let header = records.next().unwrap_or_default();
    let mut rows: Vec<Vec<(String, String)>> = records
        .filter(|record| record.iter().any(|cell| !cell.is_empty()))
        .map(|record| {
            let mut row: Vec<(String, String)> = header
                .iter()
                .cloned()
                .zip(record.into_iter().map(|cell| match cell.starts_with("_:") {
                    true => "_:".to_owned(),
                    false => cell,
                }))
                .collect();
            row.sort();
            row
        })
        .collect();
    rows.sort();
    rows
}

/// RFC 4180 records: quoted cells may contain commas, quotes (doubled) and line breaks.
fn csv_records(text: &str) -> Vec<Vec<String>> {
    let (mut records, mut record, mut cell) = (Vec::new(), Vec::new(), String::new());
    let mut chars = text.chars().filter(|c| u32::from(*c) != 13).peekable(); // drop CR of CRLF
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                cell.push('"');
            }
            (true, '"') => quoted = false,
            (true, c) => cell.push(c),
            (false, '"') => quoted = true,
            (false, ',') => record.push(std::mem::take(&mut cell)),
            (false, '\n') => {
                record.push(std::mem::take(&mut cell));
                records.push(std::mem::take(&mut record));
            }
            (false, c) => cell.push(c),
        }
    }
    if !cell.is_empty() || !record.is_empty() {
        record.push(cell);
        records.push(record);
    }
    records
}

fn dataset_iris(query: &Query) -> Vec<NamedNode> {
    let dataset = match query {
        Query::Select { dataset, .. }
        | Query::Construct { dataset, .. }
        | Query::Describe { dataset, .. }
        | Query::Ask { dataset, .. } => dataset.as_ref(),
    };
    dataset
        .map(|dataset| {
            dataset
                .default
                .iter()
                .chain(dataset.named.iter().flatten())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}
