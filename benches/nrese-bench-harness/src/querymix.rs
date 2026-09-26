//! `query-mix`: runs a directory of SPARQL queries against any SPARQL 1.1 endpoint and
//! reports latency and result size per query (the Pf0 scorecard's query column).
//!
//! The runner is system-neutral: it only speaks the SPARQL Protocol (form-encoded POST), so
//! the same queries run unchanged against NRESE, QLever, Fuseki, Oxigraph, Virtuoso, GraphDB
//! and others. Result counts are recorded next to the latencies; they double as a cross-check
//! that all systems answer the same question.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Serialize;

use crate::io::write_json_report;
use crate::model::QueryMixConfig;
use crate::normalize::percentile;

#[derive(Debug, Serialize)]
pub struct QueryMixReport {
    pub label: String,
    pub endpoint: String,
    pub warmup: usize,
    pub runs: usize,
    pub queries: Vec<QueryResult>,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub id: String,
    /// Solutions (SELECT), 0/1 (ASK) or triples (CONSTRUCT/DESCRIBE) of the last run.
    pub rows: Option<u64>,
    pub latencies_ms: Vec<f64>,
    pub min_ms: Option<f64>,
    pub p50_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub error: Option<String>,
}

pub async fn run_query_mix(config: &QueryMixConfig) -> Result<QueryMixReport> {
    let client = Client::builder()
        .timeout(Duration::from_secs(config.timeout_s))
        .build()?;
    let mut files: Vec<_> = std::fs::read_dir(&config.queries)
        .with_context(|| format!("reading {}", config.queries.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "rq"))
        .collect();
    files.sort();
    if files.is_empty() {
        bail!("no .rq files in {}", config.queries.display());
    }
    let mut queries = Vec::new();
    for file in &files {
        let result = run_one(&client, config, file).await;
        print_row(&result);
        queries.push(result);
    }
    let report = QueryMixReport {
        label: config.label.clone(),
        endpoint: config.endpoint.clone(),
        warmup: config.warmup,
        runs: config.runs,
        queries,
    };
    if let Some(path) = &config.report_json_path {
        write_json_report(path.clone(), &report)?;
    }
    Ok(report)
}

async fn run_one(client: &Client, config: &QueryMixConfig, file: &Path) -> QueryResult {
    let id = file
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut result = QueryResult {
        id,
        rows: None,
        latencies_ms: Vec::new(),
        min_ms: None,
        p50_ms: None,
        max_ms: None,
        error: None,
    };
    let query = match std::fs::read_to_string(file) {
        Ok(query) => query,
        Err(error) => {
            result.error = Some(error.to_string());
            return result;
        }
    };
    for iteration in 0..config.warmup + config.runs {
        let started = Instant::now();
        match execute(client, &config.endpoint, &query).await {
            Ok(rows) => {
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                if iteration >= config.warmup {
                    result.latencies_ms.push(elapsed);
                }
                result.rows = Some(rows);
            }
            Err(error) => {
                result.error = Some(format!("{error:#}"));
                return result;
            }
        }
    }
    let mut micros: Vec<u128> = result
        .latencies_ms
        .iter()
        .map(|ms| (ms * 1000.0) as u128)
        .collect();
    micros.sort_unstable();
    let ms = |micros: u128| micros as f64 / 1000.0;
    result.min_ms = micros.first().map(|&m| ms(m));
    result.p50_ms = (!micros.is_empty()).then(|| ms(percentile(&micros, 50)));
    result.max_ms = micros.last().map(|&m| ms(m));
    result
}

/// Sends one query and returns the result size once the whole body has arrived.
async fn execute(client: &Client, endpoint: &str, query: &str) -> Result<u64> {
    let response = client
        .post(endpoint)
        .header(
            "Accept",
            "application/sparql-results+json, application/n-triples;q=0.9",
        )
        .form(&[("query", query)])
        .send()
        .await?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = response.bytes().await?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        bail!(
            "HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    if content_type.contains("json") {
        let value: serde_json::Value = serde_json::from_slice(&body)?;
        if let Some(boolean) = value.get("boolean").and_then(|b| b.as_bool()) {
            return Ok(u64::from(boolean));
        }
        let bindings = value
            .pointer("/results/bindings")
            .and_then(|b| b.as_array())
            .context("JSON results without bindings")?;
        return Ok(bindings.len() as u64);
    }
    // Line-based RDF (N-Triples): one triple per non-empty line.
    Ok(body
        .split(|&b| b == b'\n')
        .filter(|line| line.iter().any(|b| !b.is_ascii_whitespace()))
        .count() as u64)
}

fn print_row(result: &QueryResult) {
    match (&result.error, result.p50_ms) {
        (Some(error), _) => println!(
            "{:<32} ERROR {}",
            result.id,
            error.lines().next().unwrap_or("")
        ),
        (None, Some(p50)) => println!(
            "{:<32} rows {:>10}  p50 {:>10.1} ms  min {:>10.1}  max {:>10.1}",
            result.id,
            result.rows.unwrap_or(0),
            p50,
            result.min_ms.unwrap_or(0.0),
            result.max_ms.unwrap_or(0.0)
        ),
        (None, None) => println!("{:<32} (no measured runs)", result.id),
    }
}
