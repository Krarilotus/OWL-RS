//! D16-066: native evaluation and serialization must preserve RDF/SPARQL semantics.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term, Triple};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

// Spec: legal result variable names have no 64 KiB limit. A header larger than the
// writer's chunk with 4097 rows must preserve the header and every row (D16-066).
// This regression is a checked-arithmetic witness, not a demonstrated release crash.
#[test]
fn d066_large_header_streams_all_rows() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let name = "x".repeat(65_540);
    let values = (0..4097)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let query = SparqlParser::new()
        .parse_query(&format!(
            "SELECT ?{name} ?i WHERE {{ VALUES ?i {{ {values} }} }}"
        ))
        .unwrap();
    for format in [
        nrese_sparql::ResultsFormat::Tsv,
        nrese_sparql::ResultsFormat::Csv,
        nrese_sparql::ResultsFormat::Json,
    ] {
        let mut out = Vec::new();
        nrese_sparql::write_results(
            &snapshot,
            &query,
            &QueryOptions::default(),
            format,
            None,
            &mut out,
        )
        .unwrap()
        .unwrap();
        let mut actual: Vec<usize> = if format == nrese_sparql::ResultsFormat::Json {
            let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(doc["head"]["vars"], serde_json::json!([name, "i"]));
            doc["results"]["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["i"]["value"].as_str().unwrap().parse().unwrap())
                .collect()
        } else {
            let text = std::str::from_utf8(&out).unwrap();
            let mut lines = text.lines();
            let header = if format == nrese_sparql::ResultsFormat::Tsv {
                format!("?{name}\t?i")
            } else {
                format!("{name},i")
            };
            assert_eq!(lines.next(), Some(header.as_str()));
            lines.map(|row| row[1..].parse().unwrap()).collect()
        };
        actual.sort_unstable();
        assert_eq!(actual, (0..4097).collect::<Vec<_>>(), "{format:?}");
    }
}

// Spec: RDF 1.2 triple terms are not literals; SPARQL LANG accepts only literals.
// The optimized FILTER must agree for both classification and language predicates.
#[test]
fn d066_stored_triple_is_not_a_literal_or_a_language_argument() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let node = |s: &str| NamedNode::new_unchecked(format!("urn:{s}"));
    let mut tx = engine.transaction();
    for (name, term) in [
        (
            "triple",
            Term::Triple(Box::new(Triple::new(node("s"), node("p"), node("o")))),
        ),
        ("string", Literal::new_simple_literal("text").into()),
        (
            "language",
            Literal::new_language_tagged_literal("text", "en")
                .unwrap()
                .into(),
        ),
        ("integer", Literal::from(1).into()),
    ] {
        tx.insert(Quad::new(node(name), node("value"), term, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    for (filter, expected) in [
        ("isLiteral(?t)", vec!["integer", "language", "string"]),
        ("LANG(?t) = \"\"", vec!["integer", "string"]),
        ("LANGMATCHES(LANG(?t), \"\")", vec!["integer", "string"]),
        ("LANG(?t) = \"en\"", vec!["language"]),
        ("!(LANG(?t) = \"\")", vec!["language"]),
    ] {
        let query = SparqlParser::new()
            .parse_query(&format!(
                "SELECT ?s {{ ?s <urn:value> ?t FILTER({filter}) }}"
            ))
            .unwrap();
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
        else {
            panic!("solutions expected")
        };
        let mut actual: Vec<_> = solutions
            .map(|s| s.unwrap().get("s").unwrap().to_string())
            .collect();
        actual.sort();
        let expected: Vec<_> = expected.iter().map(|s| format!("<urn:{s}>")).collect();
        assert_eq!(actual, expected, "{filter}");
    }
}
