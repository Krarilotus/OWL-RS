//! The GeoSPARQL compliance benchmark (OpenLink Software, the suite's `geosparql`
//! workload): its dataset, 212 queries over the GeoSPARQL requirements, and their expected
//! answers with accepted alternatives.
//!
//! - The benchmark: `.cache/geosparql-benchmark` (scripts/fetch-w3c-tests.sh; GPL-2.0, so
//!   fetched, never copied here), or the directory `NRESE_GEOSPARQL_BENCHMARK` names.
//!   Without it the test is skipped, unless `NRESE_W3C_REQUIRED` is set (as in CI).
//! - The queries marked `# INFERENCE` run over the dataset with its RDFS closure (they ask
//!   for memberships its class hierarchy entails), the others over the dataset as it is
//!   (their answers leave the entailed statements out).
//! - A query passes when its answer equals the expected one or an alternative: rows as
//!   multisets (in order under ORDER BY); geometry literals (WKT, GeoJSON, GML) by their
//!   geometry: the same reference system, topologically equal to a millionth of a unit
//!   (the benchmark's own module compares WKT text without whitespace, so a polygon
//!   written from another start vertex fails there); XSD numbers and booleans by value (numbers within a
//!   relative 1e-9: geodesic computations differ in the last digits); blank nodes as one
//!   placeholder.
//! - Known failures are listed in `expected-failures.txt`; the run fails on any failure not
//!   in the list and on any listed query that passes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput,
};
use nrese_sparql_syntax::SparqlParser;

const EXPECTED_FAILURES: &str = include_str!("expected-failures.txt");
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const GEO: &str = "http://www.opengis.net/ont/geosparql#";

fn benchmark_dir() -> PathBuf {
    std::env::var_os("NRESE_GEOSPARQL_BENCHMARK").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/geosparql-benchmark"),
        PathBuf::from,
    )
}

/// A term as the comparison sees it.
#[derive(Debug, Clone)]
enum Value {
    Number(f64),
    Boolean(bool),
    Geometry(String, geo::Geometry<f64>),
    Text(String),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => {
                a == b || (a - b).abs() <= 1e-9 * a.abs().max(b.abs()) || (a.is_nan() && b.is_nan())
            }
            (Value::Boolean(a), Value::Boolean(b)) => a == b,
            (Value::Geometry(c, a), Value::Geometry(d, b)) => {
                use geo::{HasDimensions, MapCoords, Relate};
                // To a millionth of a unit: the clipping arithmetic of systems differs.
                let snap = |g: &geo::Geometry<f64>| {
                    g.map_coords(|c| geo::coord! { x: (c.x * 1e6).round() / 1e6, y: (c.y * 1e6).round() / 1e6 })
                };
                c == d
                    && ((a.is_empty() && b.is_empty()) || snap(a).relate(&snap(b)).is_equal_topo())
            }
            (Value::Text(a), Value::Text(b)) => a == b,
            _ => false,
        }
    }
}

fn value(term: &Term) -> Value {
    match term {
        Term::BlankNode(_) => Value::Text("_:b".to_owned()),
        Term::Triple(t) => Value::Text(t.to_string()),
        Term::NamedNode(n) => Value::Text(format!("<{}>", n.as_str())),
        Term::Literal(l) => {
            let datatype = l.datatype().as_str();
            let text = l.value();
            if let Some(local) = datatype.strip_prefix(XSD) {
                match local {
                    "boolean" => match text.trim() {
                        "true" | "1" => return Value::Boolean(true),
                        "false" | "0" => return Value::Boolean(false),
                        _ => {}
                    },
                    "double" | "float" | "decimal" | "integer" | "int" | "long" | "short"
                    | "byte" | "nonNegativeInteger" | "positiveInteger" | "negativeInteger"
                    | "nonPositiveInteger" | "unsignedInt" | "unsignedLong" => {
                        if let Ok(n) = text.trim().replace("INF", "inf").parse::<f64>() {
                            return Value::Number(n);
                        }
                    }
                    _ => {}
                }
            }
            // Geometry literals (WKT, GeoJSON, GML) by their geometry; the text of those
            // that don't parse, without whitespace and in lower case.
            if datatype.starts_with(GEO) {
                if let Some((crs, geometry)) = nrese_sparql::geometry_literal(term) {
                    return Value::Geometry(crs, geometry);
                }
                let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                return Value::Text(format!("{datatype}:{}", text.to_lowercase()));
            }
            Value::Text(l.to_string())
        }
    }
}

/// An answer: a boolean, or the rows (each variable's value, by name) and whether their
/// order counts.
#[derive(Debug)]
enum Answer {
    Boolean(bool),
    Rows(Vec<BTreeMap<String, Value>>),
}

fn same(actual: &Answer, expected: &Answer, ordered: bool) -> bool {
    match (actual, expected) {
        (Answer::Boolean(a), Answer::Boolean(b)) => a == b,
        (Answer::Rows(a), Answer::Rows(b)) => {
            if a.len() != b.len() {
                return false;
            }
            if ordered {
                return a == b;
            }
            let mut left: Vec<&BTreeMap<String, Value>> = b.iter().collect();
            a.iter()
                .all(|row| match left.iter().position(|r| *r == row) {
                    Some(at) => {
                        left.swap_remove(at);
                        true
                    }
                    None => false,
                })
        }
        _ => false,
    }
}

/// XML with its CDATA sections written as escaped text (the results parser takes no CDATA).
fn without_cdata(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<![CDATA[") {
        out.push_str(&rest[..at]);
        let body = &rest[at + "<![CDATA[".len()..];
        let end = body.find("]]>").unwrap_or(body.len());
        out.push_str(
            &body[..end]
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;"),
        );
        rest = body.get(end + 3..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

fn expected(path: &Path) -> Result<Answer, String> {
    let text = without_cdata(&std::fs::read_to_string(path).map_err(|e| e.to_string())?);
    match QueryResultsParser::from_format(QueryResultsFormat::Xml)
        .for_reader(text.as_bytes())
        .map_err(|e| e.to_string())?
    {
        ReaderQueryResultsParserOutput::Boolean(b) => Ok(Answer::Boolean(b)),
        ReaderQueryResultsParserOutput::Solutions(solutions) => {
            let mut rows = Vec::new();
            for solution in solutions {
                let solution = solution.map_err(|e| e.to_string())?;
                rows.push(
                    solution
                        .iter()
                        .map(|(v, t)| (v.as_str().to_owned(), value(t)))
                        .collect(),
                );
            }
            Ok(Answer::Rows(rows))
        }
    }
}

fn actual(engine: &Engine, text: &str) -> Result<Answer, String> {
    let query = SparqlParser::new()
        .parse_query(text)
        .map_err(|e| e.to_string())?;
    let snapshot = engine.snapshot();
    match evaluate_query(&snapshot, &query, &QueryOptions::default()).map_err(|e| e.to_string())? {
        QueryResults::Boolean(b) => Ok(Answer::Boolean(b)),
        QueryResults::Solutions(solutions) => {
            let mut rows = Vec::new();
            for solution in solutions {
                let solution = solution.map_err(|e| e.to_string())?;
                rows.push(
                    solution
                        .iter()
                        .map(|(v, t)| (v.as_str().to_owned(), value(t)))
                        .collect(),
                );
            }
            Ok(Answer::Rows(rows))
        }
        QueryResults::Graph(_) => Err("a graph result".to_owned()),
    }
}

/// The statements RDFS derives from `quads` (reasoner v2; statements with a literal
/// subject left out).
fn rdfs_closure(quads: &[Quad]) -> Vec<Quad> {
    use nrese_reasoner::v2::batch::{self, Schema};
    use nrese_reasoner::v2::rulesets::Ruleset;
    use nrese_reasoner::v2::vocabulary::LocalVocabulary;
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Rdfs.rules(&mut vocabulary).unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let facts: Vec<[u64; 3]> = quads
        .iter()
        .map(|q| {
            [
                vocabulary.term(&q.subject.to_string()),
                vocabulary.term(&q.predicate.to_string()),
                vocabulary.term(&q.object.to_string()),
            ]
        })
        .collect();
    let result = batch::materialise(&facts, &rules, None, &schema);
    let text: String = result
        .derived
        .iter()
        .map(|[s, p, o]| {
            format!(
                "{} {} {} .\n",
                vocabulary.text(*s),
                vocabulary.text(*p),
                vocabulary.text(*o)
            )
        })
        .collect();
    RdfParser::from_format(RdfFormat::NTriples)
        .for_reader(text.as_bytes())
        .filter_map(Result::ok)
        .map(|q| Quad::new(q.subject, q.predicate, q.object, GraphName::DefaultGraph))
        .collect()
}

#[test]
fn geosparql_compliance_benchmark() {
    let dir = benchmark_dir().join("src/main/resources");
    if !dir.exists() {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|value| value.is_empty()),
            "NRESE_W3C_REQUIRED is set, but the GeoSPARQL benchmark is missing: {}",
            dir.display()
        );
        eprintln!("skipped: GeoSPARQL benchmark not found (run scripts/fetch-w3c-tests.sh)");
        return;
    }
    let data = std::fs::read(dir.join("gsb_dataset/dataset.rdf")).unwrap();
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::RdfXml)
        .for_reader(data.as_slice())
        .map(|q| q.unwrap())
        .collect();
    let load = |with_closure: bool| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in &quads {
            tx.insert(quad.as_ref());
        }
        if with_closure {
            for quad in rdfs_closure(&quads) {
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        engine
    };
    let (plain, inferred) = (load(false), load(true));

    let mut queries: Vec<PathBuf> = std::fs::read_dir(dir.join("gsb_queries"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "rq"))
        .collect();
    queries.sort();
    let answers = dir.join("gsb_answers");
    let mut failed: BTreeMap<String, String> = BTreeMap::new();
    let mut passed = 0;
    for path in &queries {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(path).unwrap();
        let ordered = text.contains("ORDER BY");
        let mut alternatives: Vec<PathBuf> = std::fs::read_dir(&answers)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let stem = p.file_stem().unwrap().to_string_lossy();
                stem == name || stem.starts_with(&format!("{name}-alternative-"))
            })
            .collect();
        alternatives.sort();
        let engine = if text.contains("# INFERENCE") {
            &inferred
        } else {
            &plain
        };
        let result = actual(engine, &text).and_then(|answer| {
            for alternative in &alternatives {
                if same(&answer, &expected(alternative)?, ordered) {
                    return Ok(());
                }
            }
            Err(format!("{answer:?}"))
        });
        match result {
            Ok(()) => passed += 1,
            Err(reason) => {
                failed.insert(name, reason.chars().take(4000).collect());
            }
        }
    }
    let expected_failures: std::collections::BTreeSet<&str> = EXPECTED_FAILURES
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|name| !name.is_empty())
        .collect();
    let new: Vec<_> = failed
        .iter()
        .filter(|(name, _)| !expected_failures.contains(name.as_str()))
        .collect();
    let fixed: Vec<_> = expected_failures
        .iter()
        .filter(|name| !failed.contains_key(**name))
        .collect();
    eprintln!(
        "GeoSPARQL compliance benchmark: {passed} of {} queries pass, {} fail ({} expected)",
        queries.len(),
        failed.len(),
        failed.len() - new.len()
    );
    assert!(
        new.is_empty() && fixed.is_empty(),
        "failures not in expected-failures.txt: {new:#?}\nlisted but now passing (remove from expected-failures.txt): {fixed:#?}"
    );
}
