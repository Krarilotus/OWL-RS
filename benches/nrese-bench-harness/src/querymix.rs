//! `query-mix`: runs a directory of SPARQL queries against any SPARQL 1.1 endpoint and
//! reports latency and result size per query (the Pf0 scorecard's query column).
//!
//! The runner is system-neutral: it only speaks the SPARQL Protocol (form-encoded POST), so
//! the same queries run unchanged against NRESE, QLever, Fuseki, Oxigraph, Virtuoso, GraphDB
//! and others. Result counts are recorded next to the latencies; they double as a cross-check
//! that all systems answer the same question.
//!
//! Two phases:
//! - **sequential:** every query, one at a time: warm-up, then measured runs. In file
//!   order, each query's runs back to back (`--order fixed`, the default), or in rounds
//!   over all queries, each round in a new order drawn from `--seed` (`--order shuffled`):
//!   then no query runs right after itself, and no fixed order favours a system whose
//!   caches the previous query happened to warm. Each query's first execution is reported
//!   apart (`first_ms`): on a fresh server it is the cold run, before any result cache
//!   could hold it.
//! - **throughput** (`--clients N`): N clients cycle through the *interactive* part of the
//!   mix (queries whose sequential p50 stayed under `--interactive-ms`) for `--duration-s`.
//!   It reports queries/s and per-query p50/p99 under load.
//! - **writes under read load** (`--update-endpoint`): during the throughput phase, one extra
//!   client inserts a fresh triple every `--write-interval-ms` (`INSERT DATA`) and reports the
//!   write latency p50/p99.

use std::path::Path;
use std::sync::Arc;
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
    /// `fixed` or `shuffled`.
    pub order: &'static str,
    pub seed: Option<u64>,
    pub queries: Vec<QueryResult>,
    pub throughput: Option<ThroughputReport>,
}

#[derive(Debug, Serialize)]
pub struct ThroughputReport {
    pub clients: usize,
    pub duration_s: f64,
    pub completed: u64,
    pub errors: u64,
    pub queries_per_s: f64,
    pub per_query: Vec<LoadedQuery>,
    pub writes: Option<WriteReport>,
}

#[derive(Debug, Serialize)]
pub struct WriteReport {
    pub interval_ms: u64,
    pub completed: u64,
    pub errors: u64,
    pub first_error: Option<String>,
    pub p50_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct LoadedQuery {
    pub id: String,
    pub completed: u64,
    pub p50_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub id: String,
    /// Solutions (SELECT), 0/1 (ASK) or triples (CONSTRUCT/DESCRIBE) of the last run.
    pub rows: Option<u64>,
    /// The query's first execution (a warm-up run unless `--warmup 0`).
    pub first_ms: Option<f64>,
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
    let queries = run_sequential(&client, config, &files).await;
    let throughput = match config.clients {
        0 => None,
        clients => {
            let interactive: Vec<(String, String)> = files
                .iter()
                .zip(&queries)
                .filter(|(_, result)| {
                    result.error.is_none()
                        && result
                            .p50_ms
                            .is_some_and(|p50| p50 <= config.interactive_ms)
                })
                .filter_map(|(file, result)| {
                    Some((result.id.clone(), std::fs::read_to_string(file).ok()?))
                })
                .collect();
            Some(run_throughput(&client, config, clients, interactive).await)
        }
    };
    let report = QueryMixReport {
        label: config.label.clone(),
        endpoint: config.endpoint.clone(),
        warmup: config.warmup,
        runs: config.runs,
        order: if config.shuffle_seed.is_some() {
            "shuffled"
        } else {
            "fixed"
        },
        seed: config.shuffle_seed,
        queries,
        throughput,
    };
    if let Some(path) = &config.report_json_path {
        write_json_report(path.clone(), &report)?;
    }
    Ok(report)
}

/// N clients cycle through `queries` until the duration is over. Each client starts at a
/// different query so the mix is spread evenly.
async fn run_throughput(
    client: &Client,
    config: &QueryMixConfig,
    clients: usize,
    queries: Vec<(String, String)>,
) -> ThroughputReport {
    let queries = Arc::new(queries);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(config.duration_s);
    let mut tasks = tokio::task::JoinSet::new();
    for client_index in 0..clients {
        let (client, queries, endpoint) = (
            client.clone(),
            Arc::clone(&queries),
            config.endpoint.clone(),
        );
        tasks.spawn(async move {
            let mut samples: Vec<(usize, Option<f64>)> = Vec::new();
            let mut next = client_index;
            while !queries.is_empty() && Instant::now() < deadline {
                let index = next % queries.len();
                next += 1;
                let started = Instant::now();
                let latency = execute(&client, &endpoint, &queries[index].1)
                    .await
                    .ok()
                    .map(|_| started.elapsed().as_secs_f64() * 1000.0);
                samples.push((index, latency));
            }
            samples
        });
    }
    let writer = config.update_endpoint.clone().map(|endpoint| {
        let (client, interval) = (
            client.clone(),
            Duration::from_millis(config.write_interval_ms),
        );
        let graph = config.write_graph.clone();
        tokio::spawn(async move {
            write_loop(&client, &endpoint, graph.as_deref(), interval, deadline).await
        })
    });
    let mut per_query: Vec<Vec<u128>> = vec![Vec::new(); queries.len()];
    let mut errors = 0;
    while let Some(Ok(samples)) = tasks.join_next().await {
        for (index, latency) in samples {
            match latency {
                Some(ms) => per_query[index].push((ms * 1000.0) as u128),
                None => errors += 1,
            }
        }
    }
    let duration_s = started.elapsed().as_secs_f64();
    let completed: u64 = per_query.iter().map(|samples| samples.len() as u64).sum();
    let per_query = per_query
        .into_iter()
        .zip(queries.iter())
        .map(|(mut micros, (id, _))| {
            micros.sort_unstable();
            let at = |p| (!micros.is_empty()).then(|| percentile(&micros, p) as f64 / 1000.0);
            LoadedQuery {
                id: id.clone(),
                completed: micros.len() as u64,
                p50_ms: at(50),
                p99_ms: at(99),
            }
        })
        .collect();
    let writes = match writer {
        Some(task) => task.await.ok(),
        None => None,
    };
    let report = ThroughputReport {
        clients,
        duration_s,
        completed,
        errors,
        queries_per_s: completed as f64 / duration_s,
        per_query,
        writes: writes.map(|(micros, errors, first_error)| {
            let at = |p| (!micros.is_empty()).then(|| percentile(&micros, p) as f64 / 1000.0);
            WriteReport {
                interval_ms: config.write_interval_ms,
                completed: micros.len() as u64,
                errors,
                p50_ms: at(50),
                p99_ms: at(99),
                first_error,
            }
        }),
    };
    println!(
        "throughput: {clients} clients, {:.1} queries/s ({} completed, {} errors)",
        report.queries_per_s, report.completed, report.errors
    );
    if let Some(writes) = &report.writes {
        println!(
            "writes under load: {} done, {} errors, p50 {:?} ms, p99 {:?} ms",
            writes.completed, writes.errors, writes.p50_ms, writes.p99_ms
        );
    }
    report
}

/// Inserts one fresh triple per interval until the deadline. Returns the sorted latencies
/// (µs), the error count and the first error.
async fn write_loop(
    client: &Client,
    endpoint: &str,
    graph: Option<&str>,
    interval: Duration,
    deadline: Instant,
) -> (Vec<u128>, u64, Option<String>) {
    let (mut micros, mut errors, mut first_error) = (Vec::new(), 0, None);
    let mut n: u64 = 0;
    while Instant::now() + interval < deadline {
        tokio::time::sleep(interval).await;
        n += 1;
        let triple =
            format!("<http://example.org/bench/write/{n}> <http://example.org/bench/p> {n}");
        let update = match graph {
            Some(graph) => format!("INSERT DATA {{ GRAPH <{graph}> {{ {triple} }} }}"),
            None => format!("INSERT DATA {{ {triple} }}"),
        };
        let started = Instant::now();
        let result = client
            .post(endpoint)
            .form(&[("update", update.as_str())])
            .send()
            .await;
        match result {
            Ok(response) if response.status().is_success() => {
                let _ = response.bytes().await;
                micros.push(started.elapsed().as_micros());
            }
            Ok(response) => {
                errors += 1;
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                first_error.get_or_insert_with(|| {
                    format!(
                        "HTTP {status}: {}",
                        body.chars().take(200).collect::<String>()
                    )
                });
            }
            Err(error) => {
                errors += 1;
                first_error.get_or_insert_with(|| error.to_string());
            }
        }
    }
    micros.sort_unstable();
    (micros, errors, first_error)
}

/// The sequential phase: every query's warm-up and measured runs, in the configured order.
async fn run_sequential(
    client: &Client,
    config: &QueryMixConfig,
    files: &[std::path::PathBuf],
) -> Vec<QueryResult> {
    let mut results: Vec<QueryResult> = files.iter().map(|file| new_result(file)).collect();
    let texts: Vec<Option<String>> = files
        .iter()
        .zip(&mut results)
        .map(|(file, result)| match std::fs::read_to_string(file) {
            Ok(query) => Some(query),
            Err(error) => {
                result.error = Some(error.to_string());
                None
            }
        })
        .collect();
    let iterations = config.warmup + config.runs;
    for (index, iteration) in schedule(files.len(), iterations, config.shuffle_seed) {
        let (result, Some(query)) = (&mut results[index], &texts[index]) else {
            continue;
        };
        if result.error.is_some() {
            continue;
        }
        let started = Instant::now();
        match execute(client, &config.endpoint, query).await {
            Ok(rows) => {
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                if iteration == 0 {
                    result.first_ms = Some(elapsed);
                }
                if iteration >= config.warmup {
                    result.latencies_ms.push(elapsed);
                }
                result.rows = Some(rows);
            }
            Err(error) => result.error = Some(format!("{error:#}")),
        }
        if iteration + 1 == iterations || result.error.is_some() {
            summarise(result);
            if config.shuffle_seed.is_none() {
                print_row(result);
            }
        }
    }
    if config.shuffle_seed.is_some() {
        results.iter().for_each(print_row);
    }
    results
}

/// The order of `(query, iteration)` runs: each query's iterations back to back, or with a
/// seed, rounds of every query, each round shuffled anew.
fn schedule(queries: usize, iterations: usize, seed: Option<u64>) -> Vec<(usize, usize)> {
    let Some(seed) = seed else {
        return (0..queries)
            .flat_map(|q| (0..iterations).map(move |i| (q, i)))
            .collect();
    };
    // SplitMix64: a small, well-mixed generator, so a seed names the same orders anywhere.
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut out = Vec::with_capacity(queries * iterations);
    for iteration in 0..iterations {
        let mut round: Vec<usize> = (0..queries).collect();
        for i in (1..round.len()).rev() {
            round.swap(i, (next() % (i as u64 + 1)) as usize);
        }
        out.extend(round.into_iter().map(|q| (q, iteration)));
    }
    out
}

fn new_result(file: &Path) -> QueryResult {
    QueryResult {
        id: file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default(),
        rows: None,
        first_ms: None,
        latencies_ms: Vec::new(),
        min_ms: None,
        p50_ms: None,
        max_ms: None,
        error: None,
    }
}

fn summarise(result: &mut QueryResult) {
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

#[cfg(test)]
mod tests {
    use super::schedule;

    #[test]
    fn a_schedule_runs_every_query_every_iteration_once() {
        assert_eq!(
            schedule(2, 2, None),
            vec![(0, 0), (0, 1), (1, 0), (1, 1)]
        );
        let shuffled = schedule(10, 3, Some(7));
        assert_eq!(shuffled, schedule(10, 3, Some(7)), "a seed names one order");
        assert_ne!(shuffled, schedule(10, 3, Some(8)));
        for iteration in 0..3 {
            let mut round: Vec<usize> = shuffled[iteration * 10..(iteration + 1) * 10]
                .iter()
                .map(|&(q, i)| {
                    assert_eq!(i, iteration);
                    q
                })
                .collect();
            round.sort_unstable();
            assert_eq!(round, (0..10).collect::<Vec<_>>());
        }
    }
}
