//! Dates, times and durations in queries (SEP-0002): the arithmetic, comparisons,
//! extractors, `ADJUST`, casts and aggregates the baseline had through spareval's
//! `sep-0002` and `calendar-ext` features, on the native executor.

use nrese_engine::{Engine, EngineConfig};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query};
use oxrdf::Term;
use spargebra::SparqlParser;

const PREFIXES: &str = "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

/// The single row of `query`, its cells as N-Triples terms (`-` for unbound), joined.
fn rows(query: &str) -> Vec<String> {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    assert_eq!(
        explain_query(&snapshot, &query, &options).unwrap().executor,
        "native"
    );
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, &options).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|solution| {
            let solution = solution.unwrap();
            variables
                .iter()
                .map(|v| solution.get(v).map_or("-".to_owned(), Term::to_string))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn one(expression: &str) -> String {
    let result = rows(&format!("SELECT ?x WHERE {{ BIND({expression} AS ?x) }}"));
    assert_eq!(result.len(), 1);
    result[0].clone()
}

const DT: &str = "<http://www.w3.org/2001/XMLSchema#dateTime>";
const DTD: &str = "<http://www.w3.org/2001/XMLSchema#dayTimeDuration>";
const YMD: &str = "<http://www.w3.org/2001/XMLSchema#yearMonthDuration>";

#[test]
fn arithmetic() {
    assert_eq!(
        one(r#""2000-10-30T06:12:00-05:00"^^xsd:dateTime - "1999-11-28T09:00:00Z"^^xsd:dateTime"#),
        format!("\"P337DT2H12M\"^^{DTD}")
    );
    assert_eq!(
        one(r#""2000-10-30T11:12:00"^^xsd:dateTime + "P1Y2M"^^xsd:yearMonthDuration"#),
        format!("\"2001-12-30T11:12:00\"^^{DT}")
    );
    assert_eq!(
        one(r#""P2Y11M"^^xsd:yearMonthDuration * 2"#),
        format!("\"P5Y10M\"^^{YMD}")
    );
    assert_eq!(
        one(r#"-"PT1H"^^xsd:dayTimeDuration"#),
        format!("\"-PT1H\"^^{DTD}")
    );
    assert_eq!(
        one(r#""11:12:00"^^xsd:time - "04:00:00"^^xsd:time"#),
        format!("\"PT7H12M\"^^{DTD}")
    );
    // Not defined by XPath: an error, so unbound.
    assert_eq!(
        one(r#""11:12:00"^^xsd:time + "P1M"^^xsd:yearMonthDuration"#),
        "-"
    );
}

#[test]
fn adjust_and_extractors() {
    assert_eq!(
        one(r#"ADJUST("2002-03-07T10:00:00-07:00"^^xsd:dateTime, "PT10H"^^xsd:dayTimeDuration)"#),
        format!("\"2002-03-08T03:00:00+10:00\"^^{DT}")
    );
    let integer = |n: &str| format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>");
    assert_eq!(one(r#"HOURS("13:20:00"^^xsd:time)"#), integer("13"));
    assert_eq!(one(r#"YEAR("2026-10"^^xsd:gYearMonth)"#), integer("2026"));
    assert_eq!(one(r#"DAY("--12-25"^^xsd:gMonthDay)"#), integer("25"));
    assert_eq!(
        one(r#"TIMEZONE("2026"^^xsd:gYear)"#),
        "-",
        "no timezone: an error"
    );
}

#[test]
fn comparisons_and_order() {
    assert_eq!(
        one(r#""P1Y"^^xsd:duration = "P12M"^^xsd:yearMonthDuration"#),
        "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"
    );
    assert_eq!(
        one(r#""P1M"^^xsd:duration < "P30D"^^xsd:duration"#),
        "-",
        "incomparable"
    );
    let ordered = rows(
        r#"SELECT ?t WHERE { VALUES ?t { "12:00:00"^^xsd:time "09:30:00"^^xsd:time "10:00:00Z"^^xsd:time } } ORDER BY ?t"#,
    );
    let times: Vec<&str> = ordered
        .iter()
        .map(|r| r.split('"').nth(1).unwrap_or(""))
        .collect();
    assert_eq!(times, ["09:30:00", "10:00:00Z", "12:00:00"]);
    let filtered = rows(
        r#"SELECT ?d WHERE { VALUES ?d { "PT1H"^^xsd:dayTimeDuration "PT3H"^^xsd:dayTimeDuration } FILTER(?d > "PT2H"^^xsd:dayTimeDuration) }"#,
    );
    assert_eq!(filtered, [format!("\"PT3H\"^^{DTD}")]);
}

#[test]
fn casts() {
    assert_eq!(
        one(r#"xsd:dayTimeDuration("P1DT2H")"#),
        format!("\"P1DT2H\"^^{DTD}")
    );
    assert_eq!(
        one(r#"xsd:yearMonthDuration("P1Y2M3DT4H"^^xsd:duration)"#),
        format!("\"P1Y2M\"^^{YMD}")
    );
    assert_eq!(
        one(r#"xsd:gYear("2026-10-01Z"^^xsd:date)"#),
        "\"2026Z\"^^<http://www.w3.org/2001/XMLSchema#gYear>"
    );
    assert_eq!(
        one(r#"xsd:time("2026-10-01T13:14:15"^^xsd:dateTime)"#),
        "\"13:14:15\"^^<http://www.w3.org/2001/XMLSchema#time>"
    );
}

#[test]
fn aggregates() {
    let result = rows(
        r#"SELECT (SUM(?d) AS ?sum) (AVG(?d) AS ?avg) (MAX(?d) AS ?max) WHERE {
            VALUES ?d { "PT1H"^^xsd:dayTimeDuration "PT2H"^^xsd:dayTimeDuration "PT30M"^^xsd:dayTimeDuration }
        }"#,
    );
    assert_eq!(
        result,
        [format!(
            "\"PT3H30M\"^^{DTD} \"PT1H10M\"^^{DTD} \"PT2H\"^^{DTD}"
        )]
    );
    // Durations of two types don't add up.
    let mixed = rows(
        r#"SELECT (SUM(?d) AS ?sum) WHERE { VALUES ?d { "PT1H"^^xsd:dayTimeDuration "P1M"^^xsd:yearMonthDuration } }"#,
    );
    assert_eq!(mixed, ["-"]);
}
