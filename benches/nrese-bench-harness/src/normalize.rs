use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::model::LatencySummary;

pub fn parse_json(payload: &[u8]) -> Result<Value> {
    serde_json::from_slice(payload).context("failed to parse JSON payload")
}

pub fn canonicalize_ntriples_set(payload: &[u8]) -> Result<BTreeSet<String>> {
    let text = std::str::from_utf8(payload).context("graph payload is not valid utf-8")?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
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
        canonicalize_ntriples_set, classify_http_body, extract_ask_boolean, extract_binding_count,
        normalize_content_type, percentile,
    };

    #[test]
    fn canonicalizes_ntriples_lines() {
        let canonical = canonicalize_ntriples_set(
            b"<http://example.com/a> <http://example.com/p> <http://example.com/b> .\n\n",
        )
        .expect("canonical");

        assert_eq!(canonical.len(), 1);
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
}
