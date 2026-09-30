//! End-to-end reasoning benchmark for reasoner v2 (R4): bulk load, materialise into the
//! inferred stack, then answer queries with the native SPARQL engine under the default
//! (materialised) read model. RT1 and RT3 in `docs/design/reasoning-benchmark.md`.
//!
//! ```text
//! cargo run --release -p nrese-store --example reason_query -- \
//!     [--ruleset owl2-rl|rdfs] [--queries dir] [--runs n] [--timeout-s n]
//!     [--memory-mib n] [--explain] [--as-written] input.{nt,ttl,...}...
//! ```
//!
//! - `--runs`: measured runs per query after one warm-up run (default 3); the best counts.
//! - `--timeout-s`, `--memory-mib`: limits per query run (default: none). A query that
//!   exceeds one is reported as `timeout` or `memory`, and the run goes on.
//! - `--as-written` evaluates the operators where the query puts them (no filter
//!   pushdown, no set evaluation, paths in full), to compare with.
//! - `--explain` prints each query's plan (operators with estimated and actual rows, and
//!   times) after its timing line.
//!
//! Prints load and reasoning times, the process's peak memory (Linux), and one
//! `name<TAB>count<TAB>milliseconds` line per query (the scorecard's answer format; in
//! place of the count `timeout`, `memory`, `error`, or `empty` for a file without a
//! query). Counting wraps each query as `SELECT (COUNT(*) AS ?n) WHERE { … }`, so result
//! serialisation isn't timed.

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphTarget, MutationCommand, MutationPipeline,
    MutationTicket, PreparedQuery, QueryEvaluationError, SparqlQueryRequest, SparqlUpdateRequest,
    StoreConfig, StoreError, StoreService,
};

/// `SELECT (COUNT(*) AS ?n) WHERE { <query> }` with the query's prologue kept in front.
fn counting(query: &str) -> String {
    let mut prologue = String::new();
    let mut body = String::new();
    for line in query.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.to_ascii_uppercase().starts_with("PREFIX")
            || trimmed.to_ascii_uppercase().starts_with("BASE")
        {
            prologue.push_str(line);
            prologue.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    format!("{prologue}SELECT (COUNT(*) AS ?n) WHERE {{ {body} }}")
}

/// Whether the text holds a query: something besides comments, `PREFIX` and `BASE`.
fn has_query(text: &str) -> bool {
    text.lines().map(str::trim_start).any(|line| {
        let upper = line.to_ascii_uppercase();
        !(line.is_empty()
            || line.starts_with('#')
            || upper.starts_with("PREFIX")
            || upper.starts_with("BASE"))
    })
}

/// Runs `work`, cancelling `token` if it takes longer than `timeout`.
fn within<T>(timeout: Option<Duration>, token: &CancellationToken, work: impl FnOnce() -> T) -> T {
    let Some(timeout) = timeout else {
        return work();
    };
    let (done, finished) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            if finished.recv_timeout(timeout) == Err(RecvTimeoutError::Timeout) {
                token.cancel();
            }
        });
        let result = work();
        let _ = done.send(());
        result
    })
}

/// The process's peak resident memory in MiB, where the system tells (Linux).
fn peak_memory_mib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024)
}

fn print_peak_memory(after: &str) {
    if let Some(mib) = peak_memory_mib() {
        println!("memory: peak {mib} MiB after {after}");
    }
}

/// The first integer value in a SPARQL JSON result.
fn first_integer(json: &str) -> Option<u64> {
    let start = json.find("\"value\"")?;
    let rest = &json[start + 7..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut ruleset, mut queries, mut files) = (Ruleset::Owl2Rl, None, Vec::new());
    let mut commits = 0usize;
    let mut explain = false;
    let mut as_written = false;
    let mut equality_report = false;
    let (mut runs, mut timeout, mut memory_limit) = (3usize, None, None);
    let number = |value: Option<String>, option: &str| -> Result<u64, String> {
        value
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("{option} takes a number"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ruleset" => {
                ruleset = match args.next().as_deref() {
                    Some(name) if Ruleset::from_name(name).is_some() => {
                        Ruleset::from_name(name).expect("checked")
                    }
                    other => return Err(format!("unknown ruleset {other:?}").into()),
                }
            }
            "--queries" => queries = args.next().map(PathBuf::from),
            "--commits" => commits = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            "--explain" => explain = true,
            "--as-written" => as_written = true,
            "--equality-report" => equality_report = true,
            "--runs" => runs = number(args.next(), "--runs")?.max(1) as usize,
            "--timeout-s" => {
                timeout = Some(Duration::from_secs(number(args.next(), "--timeout-s")?));
            }
            "--memory-mib" => {
                memory_limit = Some(number(args.next(), "--memory-mib")? as usize * 1024 * 1024);
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if files.is_empty() {
        return Err(
            "usage: reason_query [--ruleset owl2-rl|rdfs] [--queries dir] [--runs n] \
             [--timeout-s n] [--memory-mib n] [--explain] input.nt..."
                .into(),
        );
    }

    // No result cache: repeated runs measure evaluation.
    let config = StoreConfig {
        query_cache_bytes: 0,
        ..StoreConfig::in_memory()
    };
    let store = std::sync::Arc::new(StoreService::new(config)?);
    let started = Instant::now();
    let load = store.bulk_load(&BulkLoadRequest {
        files,
        replace: false,
        graph: GraphTarget::DefaultGraph,
    })?;
    let load_time = started.elapsed();
    if equality_report {
        println!("{}", store.equality_report(ruleset));
    }
    let report = store.rematerialise(ruleset)?;
    println!(
        "{}: asserted {} | load {:.2} s | closure {:.2} s in {} rounds | derived {} | violations {}",
        ruleset.name(),
        load.inserted,
        load_time.as_secs_f64(),
        report.elapsed.as_secs_f64(),
        report.rounds,
        report.inferred,
        report.violations
    );

    print_peak_memory("load and closure");

    if commits > 0 {
        commit_latency(&store, ruleset, commits)?;
    }
    let Some(dir) = queries else {
        return Ok(());
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "rq"));
    paths.sort();
    let mut total = 0.0;
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_owned();
        let text = std::fs::read_to_string(&path)?;
        if !has_query(&text) {
            println!("{name}\tempty");
            continue;
        }
        let query = counting(&text);
        let mut request = SparqlQueryRequest::new(query.clone());
        request.memory_limit = memory_limit;
        request.as_written = as_written;
        let prepared = match PreparedQuery::parse(&request) {
            Ok(prepared) => prepared,
            Err(error) => {
                eprintln!("{name}: {error}");
                println!("{name}\terror");
                continue;
            }
        };
        // The best of the measured runs, after one warm-up run. A run that fails ends
        // the query: it would fail again.
        let mut best = f64::MAX;
        let mut outcome = String::from("error");
        for _ in 0..=runs {
            let token = CancellationToken::new();
            let mut payload = Vec::new();
            let started = Instant::now();
            let result = within(timeout, &token, || {
                store.run_query(&prepared, &token, &mut payload)
            });
            let elapsed = started.elapsed().as_secs_f64();
            match result {
                Ok(()) => {
                    best = best.min(elapsed);
                    outcome = first_integer(&String::from_utf8_lossy(&payload))
                        .map_or_else(|| "error".to_owned(), |count| count.to_string());
                }
                Err(error) => {
                    best = elapsed;
                    outcome = if error.is_memory_limit() {
                        "memory"
                    } else if matches!(
                        error,
                        StoreError::SparqlEvaluation(QueryEvaluationError::Cancelled)
                    ) {
                        "timeout"
                    } else {
                        "error"
                    }
                    .to_owned();
                    eprintln!("{name}: {error}");
                    break;
                }
            }
        }
        let failed = outcome.parse::<u64>().is_err();
        if !failed {
            total += best;
        }
        eprintln!("{name}: {:.2} ms", best * 1000.0);
        if explain && !failed {
            let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(query.clone()))?;
            let plan = store.explain_query(&prepared, &CancellationToken::new())?;
            eprintln!("  executor {} | {} rows", plan.executor, plan.rows);
            for step in &plan.steps {
                let estimate = step
                    .estimated_rows
                    .map_or_else(String::new, |e| format!(" est {e}"));
                eprintln!(
                    "  {}{} {} |{} rows {} | {:.2} ms",
                    "  ".repeat(step.depth),
                    step.operator,
                    step.detail,
                    estimate,
                    step.rows,
                    step.micros as f64 / 1000.0
                );
            }
        }
        println!("{name}\t{outcome}\t{:.2}", best * 1000.0);
    }
    eprintln!(
        "queries: {:.2} ms in total (best of {runs} each, failed ones not counted)",
        total * 1000.0
    );
    print_peak_memory("the queries");
    Ok(())
}

/// Commit-path latency (RT2): deletes and re-inserts `n` asserted `rdf:type` statements one
/// at a time through the mutation pipeline, which maintains the inferred stack with the
/// delta executor. Prints p50/p99 per operation.
fn commit_latency(
    store: &std::sync::Arc<StoreService>,
    ruleset: Ruleset,
    n: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = ReasoningMode::for_ruleset(ruleset);
    let pipeline = MutationPipeline::new(
        std::sync::Arc::clone(store),
        std::sync::Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    // Asserted ABox type statements (IRIs, classes outside the W3C vocabularies), spread
    // over the dataset.
    let mut request = nrese_store::SparqlQueryRequest::new(format!(
        "SELECT ?s ?c WHERE {{ ?s a ?c FILTER(isIRI(?s) && !STRSTARTS(STR(?c), \"http://www.w3.org/\")) }} LIMIT {}",
        n * 50
    ));
    request.read_model = Some(nrese_store::ReadModel::Asserted);
    request.solutions_format = nrese_store::SolutionsResultFormat::Tsv;
    let tsv = String::from_utf8(store.execute_query(&request)?.payload)?;
    let pairs: Vec<(String, String)> = tsv
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (s, c) = line.split_once('\t')?;
            Some((s.to_owned(), c.to_owned()))
        })
        .step_by(50)
        .take(n)
        .collect();
    let (mut deletes, mut inserts) = (Vec::new(), Vec::new());
    // Per pair: delete an asserted statement (timed), put it back, insert a statement
    // about a new entity (timed: a fact new to the state), remove it again.
    for (i, (s, c)) in pairs.iter().enumerate() {
        let existing = format!("{s} a {c}");
        let fresh = format!("<urn:nrese:bench:{i}> a {c}");
        for (update, triple, timed) in [
            ("DELETE DATA", &existing, Some(&mut deletes)),
            ("INSERT DATA", &existing, None),
            ("INSERT DATA", &fresh, Some(&mut inserts)),
            ("DELETE DATA", &fresh, None),
        ] {
            let command = MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{update} {{ {triple} }}"
            )));
            let started = Instant::now();
            pipeline.apply(command, &MutationTicket::new())?;
            if let Some(times) = timed {
                times.push(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    let stats = |times: &mut Vec<f64>| {
        times.sort_by(f64::total_cmp);
        let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
        (at(0.5), at(0.99))
    };
    let (d50, d99) = stats(&mut deletes);
    let (i50, i99) = stats(&mut inserts);
    println!(
        "commits: {} deletes p50 {d50:.2} ms p99 {d99:.2} ms | inserts p50 {i50:.2} ms p99 {i99:.2} ms",
        pairs.len()
    );
    Ok(())
}
