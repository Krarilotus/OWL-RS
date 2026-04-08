use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::GraphNameRef;
use oxigraph::store::Store;
use oxrdf::graph::{CanonicalizationAlgorithm, Graph};
use serde_json::Value;

use crate::model::LatencySummary;

pub fn parse_json(payload: &[u8]) -> Result<Value> {
    serde_json::from_slice(payload).context("failed to parse JSON payload")
}

#[cfg(test)]
pub fn canonicalize_ntriples_set(payload: &[u8]) -> Result<BTreeSet<String>> {
    let text = std::str::from_utf8(payload).context("graph payload is not valid utf-8")?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub fn canonicalize_rdf_graph_set(
    content_type: Option<&str>,
    payload: &[u8],
) -> Result<BTreeSet<String>> {
    let format = infer_rdf_format_from_content_type(content_type)
        .ok_or_else(|| anyhow!("unsupported RDF graph content type for canonicalization"))?;
    let store = Store::new().context("failed to allocate temporary RDF canonicalization store")?;
    let parser = RdfParser::from_format(format).without_named_graphs();
    store
        .load_from_slice(parser, payload)
        .context("failed to parse RDF graph payload")?;

    let mut graph = Graph::new();
    for quad in store.quads_for_pattern(None, None, None, Some(GraphNameRef::DefaultGraph)) {
        graph.insert(quad?.as_ref());
    }
    graph.canonicalize(CanonicalizationAlgorithm::Unstable);

    let mut writer = RdfSerializer::from_format(RdfFormat::NTriples).for_writer(Vec::new());
    for triple in &graph {
        writer
            .serialize_triple(triple)
            .context("failed to serialize canonical graph triple")?;
    }

    let canonical = writer
        .finish()
        .context("failed to finish canonical graph serializer")?;
    let text = std::str::from_utf8(&canonical).context("graph payload is not valid utf-8")?;

    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetComparisonSummary {
    pub matched: bool,
    pub left_count: usize,
    pub right_count: usize,
    pub left_only_sample: Vec<String>,
    pub right_only_sample: Vec<String>,
}

pub fn compare_canonical_sets(
    left: &BTreeSet<String>,
    right: &BTreeSet<String>,
) -> SetComparisonSummary {
    const SAMPLE_LIMIT: usize = 5;

    SetComparisonSummary {
        matched: left == right,
        left_count: left.len(),
        right_count: right.len(),
        left_only_sample: left
            .difference(right)
            .take(SAMPLE_LIMIT)
            .cloned()
            .collect(),
        right_only_sample: right
            .difference(left)
            .take(SAMPLE_LIMIT)
            .cloned()
            .collect(),
    }
}

pub fn extract_ask_boolean(value: &Value) -> Result<bool> {
    value
        .get("boolean")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("missing SPARQL boolean result"))
}

pub fn extract_binding_count(value: &Value) -> Result<usize> {
    value
        .get("results")
        .and_then(|results| results.get("bindings"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| anyhow!("missing SPARQL bindings array"))
}

pub fn canonicalize_bindings_set(value: &Value) -> Result<BTreeSet<String>> {
    let bindings = value
        .get("results")
        .and_then(|results| results.get("bindings"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing SPARQL bindings array"))?;

    bindings
        .iter()
        .map(canonicalize_binding_row)
        .collect::<Result<BTreeSet<_>>>()
}

fn canonicalize_binding_row(binding: &Value) -> Result<String> {
    let object = binding
        .as_object()
        .ok_or_else(|| anyhow!("binding row is not an object"))?;

    let mut entries = object
        .iter()
        .map(|(name, value)| canonicalize_binding_value(name, value))
        .collect::<Result<Vec<_>>>()?;
    entries.sort_unstable();

    Ok(entries.join("|"))
}

fn canonicalize_binding_value(name: &str, value: &Value) -> Result<String> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("binding value is not an object"))?;
    let term_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("binding value missing type"))?;
    let lexical = object
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("binding value missing lexical form"))?;
    let datatype = object
        .get("datatype")
        .and_then(Value::as_str)
        .unwrap_or("");
    let language = object.get("xml:lang").and_then(Value::as_str).unwrap_or("");

    Ok(format!("{name}={term_type}|{datatype}|{language}|{lexical}"))
}

pub fn normalize_content_type(content_type: Option<&str>) -> Option<String> {
    content_type.map(|value| {
        value
            .split(';')
            .next()
            .unwrap_or(value)
            .trim()
            .to_ascii_lowercase()
    })
}

fn infer_rdf_format_from_content_type(content_type: Option<&str>) -> Option<RdfFormat> {
    match normalize_content_type(content_type).as_deref() {
        Some("text/turtle") | Some("application/x-turtle") => Some(RdfFormat::Turtle),
        Some("application/n-triples") => Some(RdfFormat::NTriples),
        Some("application/rdf+xml") => Some(RdfFormat::RdfXml),
        _ => None,
    }
}

pub fn classify_http_body(content_type: Option<&str>, payload: &[u8]) -> &'static str {
    if payload.is_empty() {
        return "empty";
    }

    match normalize_content_type(content_type).as_deref() {
        Some("application/problem+json") => "problem-json",
        Some("application/sparql-results+json") => "sparql-results-json",
        Some("application/json") => "json",
        Some("text/plain") => "plain-text",
        Some("text/html") => "html",
        Some("text/turtle")
        | Some("application/x-turtle")
        | Some("application/n-triples")
        | Some("application/rdf+xml")
        | Some("application/ld+json") => "rdf",
        Some(_) => "typed-body",
        None => "untyped-body",
    }
}

pub fn summarize(mut latencies: Vec<u128>, success: usize, failure: usize) -> LatencySummary {
    latencies.sort_unstable();

    let samples = latencies.len();
    let total_ms = latencies.iter().copied().sum();
    let min_ms = latencies.first().copied().unwrap_or(0);
    let max_ms = latencies.last().copied().unwrap_or(0);
    let p50_ms = percentile(&latencies, 50);
    let p95_ms = percentile(&latencies, 95);
    let p99_ms = percentile(&latencies, 99);

    LatencySummary {
        samples,
        success,
        failure,
        min_ms,
        p50_ms,
        p95_ms,
        p99_ms,
        max_ms,
        total_ms,
    }
}

pub fn percentile(sorted_latencies: &[u128], p: usize) -> u128 {
    if sorted_latencies.is_empty() {
        return 0;
    }

    let index = ((sorted_latencies.len() - 1) * p) / 100;
    sorted_latencies[index]
}

#[cfg(test)]
mod tests {
    use super::{
        canonicalize_bindings_set, canonicalize_ntriples_set, canonicalize_rdf_graph_set,
        classify_http_body, compare_canonical_sets, extract_ask_boolean, extract_binding_count,
        normalize_content_type, percentile,
    };
    use std::collections::BTreeSet;

    #[test]
    fn canonicalizes_ntriples_lines() {
        let canonical = canonicalize_ntriples_set(
            b"<http://example.com/a> <http://example.com/p> <http://example.com/b> .\n\n",
        )
        .expect("canonical");

        assert_eq!(canonical.len(), 1);
    }

    #[test]
    fn canonicalizes_rdf_xml_to_graph_set() {
        let payload = br#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:ex="http://example.com/">
  <rdf:Description rdf:about="http://example.com/a">
    <ex:p rdf:resource="http://example.com/b"/>
  </rdf:Description>
</rdf:RDF>
"#;

        let canonical = canonicalize_rdf_graph_set(Some("application/rdf+xml"), payload)
            .expect("canonical graph");
        assert!(canonical.contains(
            "<http://example.com/a> <http://example.com/p> <http://example.com/b> ."
        ));
    }

    #[test]
    fn canonicalizes_isomorphic_blank_nodes_to_the_same_graph_set() {
        let left = br#"
<http://example.com/a> <http://example.com/p> _:b0 .
_:b0 <http://example.com/p> <http://example.com/c> .
"#;
        let right = br#"
<http://example.com/a> <http://example.com/p> _:other .
_:other <http://example.com/p> <http://example.com/c> .
"#;

        let left_set = canonicalize_rdf_graph_set(Some("application/n-triples"), left)
            .expect("left graph");
        let right_set = canonicalize_rdf_graph_set(Some("application/n-triples"), right)
            .expect("right graph");

        assert_eq!(left_set, right_set);
    }

    #[test]
    fn extracts_boolean() {
        let value = serde_json::json!({ "boolean": true });
        assert!(extract_ask_boolean(&value).expect("boolean"));
    }

    #[test]
    fn extracts_binding_count() {
        let value = serde_json::json!({
            "results": { "bindings": [{}, {}] }
        });
        assert_eq!(extract_binding_count(&value).expect("count"), 2);
    }

    #[test]
    fn canonicalizes_bindings_set_by_variable_content() {
        let value = serde_json::json!({
            "results": {
                "bindings": [
                    {
                        "s": { "type": "uri", "value": "http://example.com/b" },
                        "label": { "type": "literal", "value": "B" }
                    },
                    {
                        "label": { "type": "literal", "value": "A" },
                        "s": { "type": "uri", "value": "http://example.com/a" }
                    }
                ]
            }
        });

        let canonical = canonicalize_bindings_set(&value).expect("bindings set");
        assert_eq!(canonical.len(), 2);
        assert!(canonical.iter().any(|row| row.contains("http://example.com/a")));
        assert!(canonical.iter().any(|row| row.contains("http://example.com/b")));
    }

    #[test]
    fn percentile_uses_sorted_index() {
        assert_eq!(percentile(&[10, 20, 30, 40, 50], 95), 40);
    }

    #[test]
    fn normalize_content_type_strips_parameters() {
        assert_eq!(
            normalize_content_type(Some("application/problem+json; charset=utf-8")).as_deref(),
            Some("application/problem+json")
        );
    }

    #[test]
    fn classify_http_body_uses_content_type_and_empty_detection() {
        assert_eq!(
            classify_http_body(Some("text/plain"), b"oops"),
            "plain-text"
        );
        assert_eq!(
            classify_http_body(Some("application/problem+json"), br#"{"title":"bad"}"#),
            "problem-json"
        );
        assert_eq!(classify_http_body(Some("text/turtle"), b""), "empty");
    }

    #[test]
    fn compare_canonical_sets_returns_diff_samples() {
        let left = BTreeSet::from([
            "<http://example.com/a> <http://example.com/p> <http://example.com/b> .".to_owned(),
            "<http://example.com/a> <http://example.com/p> <http://example.com/c> .".to_owned(),
        ]);
        let right = BTreeSet::from([
            "<http://example.com/a> <http://example.com/p> <http://example.com/b> .".to_owned(),
            "<http://example.com/a> <http://example.com/p> <http://example.com/d> .".to_owned(),
        ]);

        let summary = compare_canonical_sets(&left, &right);

        assert!(!summary.matched);
        assert_eq!(summary.left_count, 2);
        assert_eq!(summary.right_count, 2);
        assert_eq!(summary.left_only_sample.len(), 1);
        assert_eq!(summary.right_only_sample.len(), 1);
    }
}
