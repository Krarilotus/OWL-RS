//! Perf lab: the in-process measurement loop for performance work (ROADMAP §3.1, PL).
//!
//! ```text
//! cargo run --release -p nrese-store --example perf_lab -- \
//!     [--store DIR] [--load FILE]... --queries DIR [--runs 5] [--warmup 1] \
//!     [--timeout-s 120] [--only SUBSTRING] [--label NAME] [--json OUT] [--baseline JSON] \
//!     [--explain] [--qerror] [--format tsv|json|xml|csv] [--shapes FILE] [--reason MODE]
//!     [--rules FILE.n3] [--reason-runs 1] [--results DIR] [--routes] [--commits FILE]
//!     [--readers 0] [--clients N --duration-s 10] [--threads N] [--export FILE]
//!     [--canonicalize FILE]...
//! cargo run --release -p nrese-store --example perf_lab -- generate KIND OUT [key=value]...
//! ```
//!
//! - **Data:** `--load` bulk-loads files, into memory or, with `--store`, into an on-disk
//!   store. `--store` alone reopens a store loaded earlier, which skips the load on reruns.
//! - **Queries:** every `*.rq` in `--queries`, in name order. Each runs `--warmup` times
//!   unmeasured, then `--runs` times measured. The result is serialised as the server
//!   would (TSV for SELECT, JSON for ASK, N-Triples for CONSTRUCT/DESCRIBE) into a
//!   counting sink, so evaluation, term decoding and serialisation are all included.
//! - **Report:** rows, p50, min and max per query, peak memory (Linux), and a JSON file
//!   for `benches/baselines/`. `--baseline` adds the ratio against an earlier JSON report.
//!   `--shapes` loads a SHACL shapes file into the shapes graph and times validating the
//!   data against it (`--runs` times), before the queries.
//!   `--format` serialises SELECT results in another format than TSV (JSON is what most
//!   clients ask for).
//!   `--reason` materialises a ruleset (`owl2-rl`, `rdfs`, ...) after the load, timed, and
//!   the queries read the materialised data (the default read model).
//!   `--results` writes each query's serialised answer to `DIR/<query>.out` (once, after
//!   the measurement), to compare answers with another engine's.
//!   `--explain` prints each query's plan after its measurement: every operator with its
//!   estimated and actual rows and its time (inputs included), where the time goes when a
//!   profiler isn't at hand.
//!   `--routes` records the operators each query ran with (from EXPLAIN) in the JSON
//!   report, for the fast suite's route checks; a query with one row also records its
//!   first value there (`value`), the answer of a count.
//!   `--qerror` measures the planner's estimates: every operator with an estimate, over
//!   every query, gets its q-error, `max(estimate, rows) / min(estimate, rows)` (both at
//!   least 1; Moerkotte et al., VLDB 2009), summarised per operator (median, p90, max, the
//!   share within 2×) and in the JSON report.
//!
//! - **The fast suite's measurements** (`benches/fast`): `--reason` takes every reasoning
//!   mode (`rdfs`, `rdfs-full`, `rdfs-plus`, `owl-horst`, `owl2-ql`, `owl2-rl`, `custom`)
//!   and `--rules` adds Notation3 user rules; `--reason-runs N` rematerialises N times (rounds
//!   and phases in the report). `--commits FILE` applies one SPARQL update per line as a
//!   commit through the mutation pipeline, which maintains the closure, with `--readers N`
//!   threads running the query set meanwhile; after the commits the closure is computed
//!   afresh and must have the same size. `--clients N` runs the query set on N threads for
//!   `--duration-s`. `--threads N` sizes the thread pool (1: single-threaded). `--export`
//!   writes the store's statements, inferred ones included, as N-Triples (the closure the
//!   stores without reasoning load). `--canonicalize` canonicalises blank nodes.
//!   `generate` writes the suite's data (`generate.rs`). A failure still writes the report, with
//!   its `error`.
//!
//! This is the loop every Phase 2–5 work package is measured with before the Docker
//! scorecard confirms it against other systems. It runs anywhere the data is; the wrapper
//! `benches/perf-lab.sh` runs it on Linux in Docker against the benchmark volume.

use std::collections::BTreeMap;
use std::io::Write;
mod generate;
mod modes;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphTarget, MutationPipeline, PreparedQuery,
    QueryResultKind, ShaclValidationRequest, SolutionsResultFormat, SparqlQueryRequest,
    StoreConfig, StoreService,
};

/// mimalloc, as in the server; `RUSTFLAGS="--cfg system_alloc"` measures the system allocator.
#[cfg(not(system_alloc))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct Args {
    store: Option<PathBuf>,
    load: Vec<PathBuf>,
    queries: PathBuf,
    runs: usize,
    warmup: usize,
    timeout: Duration,
    only: Option<String>,
    label: String,
    json: Option<PathBuf>,
    baseline: Option<PathBuf>,
    explain: bool,
    qerror: bool,
    format: SolutionsResultFormat,
    shapes: Option<PathBuf>,
    reason: Option<ReasoningMode>,
    rules: Option<PathBuf>,
    reason_runs: usize,
    results: Option<PathBuf>,
    routes: bool,
    commits: Option<PathBuf>,
    readers: usize,
    clients: usize,
    duration: Duration,
    threads: Option<usize>,
    export: Option<PathBuf>,
    canonicalize: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        store: None,
        load: Vec::new(),
        queries: PathBuf::new(),
        runs: 5,
        warmup: 1,
        timeout: Duration::from_secs(120),
        only: None,
        label: "nrese".into(),
        json: None,
        baseline: None,
        explain: false,
        qerror: false,
        format: SolutionsResultFormat::Tsv,
        shapes: None,
        reason: None,
        rules: None,
        reason_runs: 1,
        results: None,
        routes: false,
        commits: None,
        readers: 0,
        clients: 0,
        duration: Duration::from_secs(10),
        threads: None,
        export: None,
        canonicalize: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--store" => args.store = Some(value()?.into()),
            "--load" => args.load.push(value()?.into()),
            "--queries" => args.queries = value()?.into(),
            "--runs" => args.runs = value()?.parse().map_err(|e| format!("--runs: {e}"))?,
            "--warmup" => args.warmup = value()?.parse().map_err(|e| format!("--warmup: {e}"))?,
            "--timeout-s" => {
                args.timeout =
                    Duration::from_secs(value()?.parse().map_err(|e| format!("--timeout-s: {e}"))?)
            }
            "--only" => args.only = Some(value()?),
            "--label" => args.label = value()?,
            "--json" => args.json = Some(value()?.into()),
            "--baseline" => args.baseline = Some(value()?.into()),
            "--explain" => args.explain = true,
            "--results" => args.results = Some(value()?.into()),
            "--qerror" => args.qerror = true,
            "--shapes" => args.shapes = Some(value()?.into()),
            "--reason" => {
                let name = value()?;
                args.reason = Some(
                    ReasoningMode::REASONING
                        .into_iter()
                        .chain([ReasoningMode::Custom])
                        .find(|mode| mode.as_str() == name)
                        .ok_or(format!("--reason: unknown mode {name}"))?,
                );
            }
            "--rules" => args.rules = Some(value()?.into()),
            "--reason-runs" => {
                args.reason_runs = value()?
                    .parse()
                    .map_err(|e| format!("--reason-runs: {e}"))?
            }
            "--routes" => args.routes = true,
            "--commits" => args.commits = Some(value()?.into()),
            "--readers" => {
                args.readers = value()?.parse().map_err(|e| format!("--readers: {e}"))?
            }
            "--clients" => {
                args.clients = value()?.parse().map_err(|e| format!("--clients: {e}"))?
            }
            "--duration-s" => {
                args.duration = Duration::from_secs_f64(
                    value()?.parse().map_err(|e| format!("--duration-s: {e}"))?,
                )
            }
            "--threads" => {
                args.threads = Some(value()?.parse().map_err(|e| format!("--threads: {e}"))?)
            }
            "--export" => args.export = Some(value()?.into()),
            "--canonicalize" => args.canonicalize.push(value()?.into()),
            "--format" => {
                args.format = match value()?.as_str() {
                    "tsv" => SolutionsResultFormat::Tsv,
                    "json" => SolutionsResultFormat::Json,
                    "xml" => SolutionsResultFormat::Xml,
                    "csv" => SolutionsResultFormat::Csv,
                    other => return Err(format!("--format: unknown format {other}")),
                }
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.runs == 0 || args.reason_runs == 0 {
        return Err("--runs and --reason-runs must be > 0".into());
    }
    Ok(args)
}

/// Counts bytes and lines written, discarding the data.
#[derive(Default)]
pub(crate) struct CountingSink {
    bytes: u64,
    lines: u64,
}

impl Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes += buf.len() as u64;
        self.lines += buf.iter().filter(|&&b| b == b'\n').count() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Measured {
    rows: u64,
    times: Vec<Duration>,
    error: Option<String>,
}

fn run_once(
    store: &StoreService,
    prepared: &PreparedQuery,
    timeout: Duration,
) -> Result<(u64, Duration), String> {
    let token = CancellationToken::new();
    let canceller = {
        let token = token.clone();
        let (done, wait) = std::sync::mpsc::channel::<()>();
        (
            std::thread::spawn(move || {
                if wait.recv_timeout(timeout).is_err() {
                    token.cancel();
                }
            }),
            done,
        )
    };
    let mut sink = CountingSink::default();
    let started = Instant::now();
    let result = store.run_query(prepared, &token, &mut sink);
    let elapsed = started.elapsed();
    let _ = canceller.1.send(());
    let _ = canceller.0.join();
    result.map_err(|e| {
        if elapsed >= timeout {
            format!("timeout after {} s", timeout.as_secs())
        } else {
            e.to_string()
        }
    })?;
    // TSV and N-Triples: one line per row, plus the TSV header. ASK (JSON): one row.
    let rows = match prepared.kind() {
        QueryResultKind::Solutions => sink.lines.saturating_sub(1),
        QueryResultKind::Boolean => 1,
        QueryResultKind::Graph => sink.lines,
    };
    Ok((rows, elapsed))
}

/// The query's answer, serialised as the measurement does, into `path`.
fn write_results(
    store: &StoreService,
    text: &str,
    args: &Args,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut request = SparqlQueryRequest::all(text);
    request.solutions_format = args.format;
    let mut prepared = PreparedQuery::parse(&request)?;
    if prepared.kind() == QueryResultKind::Boolean {
        request.solutions_format = SolutionsResultFormat::Json;
        prepared = PreparedQuery::parse(&request)?;
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    store.run_query(&prepared, &CancellationToken::new(), file)?;
    Ok(())
}

fn measure(store: &StoreService, text: &str, args: &Args) -> Measured {
    let mut request = SparqlQueryRequest::all(text);
    request.solutions_format = args.format;
    let prepared = match PreparedQuery::parse(&request) {
        Ok(prepared) if prepared.kind() != QueryResultKind::Boolean => prepared,
        Ok(_) => {
            // ASK needs a boolean-capable format.
            request.solutions_format = SolutionsResultFormat::Json;
            PreparedQuery::parse(&request).expect("parsed once already")
        }
        Err(e) => {
            return Measured {
                rows: 0,
                times: Vec::new(),
                error: Some(e.to_string()),
            };
        }
    };
    let mut measured = Measured {
        rows: 0,
        times: Vec::new(),
        error: None,
    };
    for i in 0..args.warmup + args.runs {
        match run_once(store, &prepared, args.timeout) {
            Ok((rows, elapsed)) => {
                measured.rows = rows;
                if i >= args.warmup {
                    measured.times.push(elapsed);
                }
            }
            Err(e) => {
                measured.error = Some(e);
                break;
            }
        }
    }
    measured.times.sort();
    measured
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Prints how `text` runs: each operator, indented by depth, with estimated and actual
/// rows and its time.
fn explain(store: &StoreService, text: &str) {
    let explained = PreparedQuery::parse(&SparqlQueryRequest::all(text))
        .map_err(|e| e.to_string())
        .and_then(|prepared| {
            store
                .explain_query(&prepared, &CancellationToken::new())
                .map_err(|e| e.to_string())
        });
    match explained {
        Ok(explanation) => {
            for step in &explanation.steps {
                let mut detail = step.detail.clone();
                if detail.len() > 90 {
                    let cut = (0..=90)
                        .rev()
                        .find(|&i| detail.is_char_boundary(i))
                        .unwrap_or(0);
                    detail.truncate(cut);
                    detail.push('…');
                }
                println!(
                    "    {:indent$}{} {detail}  [est {} rows {} {:.2} ms]",
                    "",
                    step.operator,
                    step.estimated_rows
                        .map_or("-".to_owned(), |n| n.to_string()),
                    step.rows,
                    step.micros as f64 / 1000.0,
                    indent = 2 * step.depth,
                );
            }
        }
        Err(error) => println!("    explain failed: {error}"),
    }
}

/// The q-errors of `text`'s estimated operators, by operator.
fn qerrors(store: &StoreService, text: &str, into: &mut BTreeMap<String, Vec<f64>>) {
    let Ok(prepared) = PreparedQuery::parse(&SparqlQueryRequest::all(text)) else {
        return;
    };
    let Ok(explanation) = store.explain_query(&prepared, &CancellationToken::new()) else {
        return;
    };
    for step in &explanation.steps {
        if let Some(estimate) = step.estimated_rows {
            let (e, r) = (estimate.max(1) as f64, step.rows.max(1) as f64);
            into.entry(step.operator.clone())
                .or_default()
                .push(e.max(r) / e.min(r));
        }
    }
}

/// `operator  n  median  p90  max  within 2x` per operator and over all; the JSON object.
fn qerror_summary(by_operator: &BTreeMap<String, Vec<f64>>) -> String {
    let stats = |values: &[f64]| {
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        let within = v.iter().filter(|&&x| x <= 2.0).count() as f64 / v.len() as f64;
        (v.len(), at(0.5), at(0.9), *v.last().unwrap(), within)
    };
    println!(
        "{:<20} {:>6} {:>9} {:>9} {:>11} {:>9}",
        "q-error", "n", "median", "p90", "max", "within 2x"
    );
    let mut all: Vec<f64> = Vec::new();
    let mut json = Vec::new();
    for (operator, values) in by_operator {
        let (n, median, p90, max, within) = stats(values);
        println!(
            "{operator:<20} {n:>6} {median:>9.2} {p90:>9.2} {max:>11.1} {:>8.0}%",
            within * 100.0
        );
        json.push(format!(
            "    {:?}: {{\"n\": {n}, \"median\": {median:.3}, \"p90\": {p90:.3}, \"max\": {max:.1}, \"within_2x\": {within:.3}}}",
            operator
        ));
        all.extend(values);
    }
    if !all.is_empty() {
        let (n, median, p90, max, within) = stats(&all);
        println!(
            "{:<20} {n:>6} {median:>9.2} {p90:>9.2} {max:>11.1} {:>8.0}%",
            "all",
            within * 100.0
        );
        json.push(format!(
            "    \"all\": {{\"n\": {n}, \"median\": {median:.3}, \"p90\": {p90:.3}, \"max\": {max:.1}, \"within_2x\": {within:.3}}}"
        ));
    }
    format!("{{\n{}\n  }}", json.join(",\n"))
}

/// Peak and current resident memory in MiB, where the OS reports it (Linux).
fn memory_mib() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status
            .lines()
            .find(|l| l.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()
            .map(|kib| kib / 1024)
    };
    Some((field("VmHWM:")?, field("VmRSS:")?))
}

/// `query -> p50 ms` from an earlier `--json` report.
fn read_baseline(path: &PathBuf) -> BTreeMap<String, f64> {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("baseline {} not readable; ignoring it", path.display());
        return BTreeMap::new();
    };
    // The report format is ours and flat: `"name": "...", ... "p50_ms": x` per query.
    let mut out = BTreeMap::new();
    let mut name = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("\"name\": \"") {
            name = rest.split('"').next().map(str::to_owned);
        } else if let Some(rest) = line.strip_prefix("\"p50_ms\": ")
            && let (Some(n), Ok(v)) = (name.take(), rest.trim_end_matches(',').parse::<f64>())
        {
            out.insert(n, v);
        }
    }
    out
}

/// The first value of a one-row result (a count's answer), as its lexical form.
fn first_value(store: &StoreService, text: &str) -> Option<String> {
    let mut request = SparqlQueryRequest::all(text);
    request.solutions_format = SolutionsResultFormat::Tsv;
    let prepared = PreparedQuery::parse(&request).ok()?;
    if prepared.kind() != QueryResultKind::Solutions {
        return None;
    }
    let mut out = Vec::new();
    store
        .run_query(&prepared, &CancellationToken::new(), &mut out)
        .ok()?;
    let text = String::from_utf8(out).ok()?;
    let cell = text.lines().nth(1)?.split('\t').next()?;
    // `"12"^^<…#integer>` or a plain term: the lexical form.
    Some(
        cell.strip_prefix('"')
            .and_then(|c| c.split('"').next())
            .unwrap_or(cell)
            .to_owned(),
    )
}

/// The distinct operators `text` runs with, in the order EXPLAIN lists them.
fn operators(store: &StoreService, text: &str) -> Vec<String> {
    let Ok(prepared) = PreparedQuery::parse(&SparqlQueryRequest::all(text)) else {
        return Vec::new();
    };
    let Ok(explanation) = store.explain_query(&prepared, &CancellationToken::new()) else {
        return Vec::new();
    };
    let mut seen = Vec::new();
    for step in explanation.steps {
        if !seen.contains(&step.operator) {
            seen.push(step.operator);
        }
    }
    seen
}

/// The container's peak memory in MiB, where it runs under cgroup v2 (Docker on Linux).
fn cgroup_peak_mib() -> Option<u64> {
    std::fs::read_to_string("/sys/fs/cgroup/memory.peak")
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|bytes| bytes / 1048576)
}

/// What a run measured, written as the JSON report even when the run fails.
#[derive(Default)]
struct Report {
    open_s: f64,
    load_s: f64,
    sum_p50: f64,
    /// `"key": value` members beyond the query set (reasoning, commits, clients, ...).
    sections: Vec<(&'static str, String)>,
    queries: Vec<String>,
    qerror: Option<String>,
}

impl Report {
    fn json(&self, label: &str) -> String {
        let memory = memory_mib();
        let sections: String = self
            .sections
            .iter()
            .map(|(key, value)| format!("\n  \"{key}\": {value},"))
            .collect();
        format!(
            "{{\n  \"label\": {label:?},\n  \"open_s\": {:.3},\n  \"load_s\": {:.3},\n  \"peak_mib\": {},\n  \"cgroup_peak_mib\": {},\n  \"sum_p50_ms\": {:.3},{}{}\n  \"queries\": [\n{}\n  ]\n}}\n",
            self.open_s,
            self.load_s,
            memory.map_or(0, |(peak, _)| peak),
            cgroup_peak_mib().map_or("null".to_owned(), |m| m.to_string()),
            self.sum_p50,
            sections,
            self.qerror
                .as_ref()
                .map_or(String::new(), |q| format!("\n  \"qerror\": {q},")),
            self.queries.join(",\n")
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("generate") {
        return generate::main(&raw[1..]);
    }
    #[cfg(not(system_alloc))]
    // SAFETY: mimalloc's `mi_collect` may be called at any time, from any thread.
    nrese_engine::memory::set_release(|force| unsafe { libmimalloc_sys::mi_collect(force) });
    let args =
        parse_args().map_err(|e| format!("{e}\nsee the usage at the top of perf_lab/main.rs"))?;
    let mut report = Report::default();
    let result = run(&args, &mut report);
    if let Err(error) = &result {
        eprintln!("error: {error}");
        report
            .sections
            .push(("error", format!("{:?}", error.to_string())));
    }
    if let Some(path) = &args.json {
        std::fs::write(path, report.json(&args.label))?;
    }
    result
}

fn run(args: &Args, report: &mut Report) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(threads) = args.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }
    let config = match &args.store {
        Some(dir) => StoreConfig::on_disk(dir),
        None => StoreConfig::in_memory(),
    };
    // No result cache: repeated runs measure evaluation. NRESE_BULK_LOAD_MEMORY (bytes)
    // bounds the load's quads (spilled past it).
    let bulk_load_memory_bytes = std::env::var("NRESE_BULK_LOAD_MEMORY")
        .ok()
        .and_then(|bytes| bytes.parse().ok())
        .unwrap_or(0);
    // The index encoding as the server takes it (`NRESE_INDEX_ENCODING`: fast, compact).
    let index_encoding = std::env::var("NRESE_INDEX_ENCODING")
        .ok()
        .and_then(|name| nrese_engine::IndexEncoding::from_name(&name))
        .unwrap_or_default();
    // The vocabulary as the server takes it (`NRESE_VOCABULARY`: plain, fsst).
    let vocabulary = std::env::var("NRESE_VOCABULARY")
        .ok()
        .and_then(|name| nrese_engine::VocabularyEncoding::from_name(&name))
        .unwrap_or_default();
    // Equality reasoning as the server takes it (`NRESE_REASONING_EQUALITY`).
    let equality = std::env::var("NRESE_REASONING_EQUALITY").unwrap_or_default();
    let equality_by_representatives = equality != "replicate";
    let equality_compact = equality == "compact";
    // Where classes are expanded and what answers name (`NRESE_REASONING_EQUALITY_EXPANSION`,
    // `NRESE_REASONING_EQUALITY_ANSWERS`).
    let setting = |name: &str| std::env::var(name).unwrap_or_default();
    let config = StoreConfig {
        query_cache_bytes: 0,
        bulk_load_memory_bytes,
        index_encoding,
        vocabulary,
        equality_by_representatives,
        equality_compact,
        equality_early_expansion: setting("NRESE_REASONING_EQUALITY_EXPANSION") == "early",
        equality_canonical_answers: setting("NRESE_REASONING_EQUALITY_ANSWERS") == "canonical",
        ..config
    };
    let started = Instant::now();
    let store = Arc::new(StoreService::new(config)?);
    report.open_s = started.elapsed().as_secs_f64();
    if !args.load.is_empty() {
        let started = Instant::now();
        let loaded = store.bulk_load(&BulkLoadRequest {
            files: args.load.clone(),
            replace: false,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })?;
        report.load_s = started.elapsed().as_secs_f64();
        eprintln!("loaded {} quads in {:.2} s", loaded.inserted, report.load_s);
        report
            .sections
            .push(("loaded", loaded.inserted.to_string()));
        if let Some((peak, _)) = memory_mib() {
            report.sections.push(("load_peak_mib", peak.to_string()));
        }
    }
    let rules = match &args.rules {
        Some(path) => Some(Arc::new(UserRules::n3(
            path.display().to_string(),
            std::fs::read_to_string(path)?,
        )?)),
        None => None,
    };
    let reasoner = ReasonerConfig::for_mode(args.reason.unwrap_or(ReasoningMode::Disabled))
        .with_rules(rules)?;
    let program = reasoner.materialised_program();
    if let Some(program) = &program {
        let mut times = Vec::new();
        let mut last = None;
        for _ in 0..args.reason_runs {
            let done = store.rematerialise(program)?;
            times.push(done.elapsed);
            last = Some(done);
        }
        let done = last.expect("at least one run");
        let (p50, _, max) = modes::percentiles(&mut times);
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        eprintln!(
            "reasoned ({}): {} inferred, {} rounds, p50 {p50:.1} ms over {} runs (modules {:.1} ms)",
            done.ruleset,
            done.inferred,
            done.rounds,
            args.reason_runs,
            ms(done.phases.modules)
        );
        let samples: Vec<String> = times.iter().map(|t| format!("{:.3}", ms(*t))).collect();
        report.sections.push((
            "reason",
            format!(
                "{{\"program\": {:?}, \"asserted\": {}, \"inferred\": {}, \"violations\": {}, \"rounds\": {}, \
                 \"runs\": {}, \"p50_ms\": {p50:.3}, \"max_ms\": {max:.3}, \"samples_ms\": [{}], \"grounding_ms\": {:.3}, \
                 \"joins_ms\": {:.3}, \"modules_ms\": {:.3}, \"merge_ms\": {:.3}, \"consistency_ms\": {:.3}}}",
                done.ruleset,
                done.asserted,
                done.inferred,
                done.violations,
                done.rounds,
                args.reason_runs,
                samples.join(", "),
                ms(done.phases.grounding),
                ms(done.phases.joins),
                ms(done.phases.modules),
                ms(done.phases.merge),
                ms(done.phases.consistency),
            ),
        ));
        if let Some((peak, _)) = memory_mib() {
            report.sections.push(("reason_peak_mib", peak.to_string()));
        }
    }
    if let Some(shapes) = &args.shapes {
        store.bulk_load(&BulkLoadRequest {
            files: vec![shapes.clone()],
            replace: false,
            graph: GraphTarget::NamedGraph(nrese_store::DEFAULT_SHAPES_GRAPH.to_owned()),
            skip_errors: false,
        })?;
        let mut times = Vec::new();
        let mut results = 0;
        for _ in 0..args.runs {
            let started = Instant::now();
            let validation = store.validate_shacl(
                &nrese_store::ReadScope::All,
                &ShaclValidationRequest::default(),
            )?;
            times.push(started.elapsed());
            results = validation.report.results.len();
        }
        let samples: Vec<String> = times
            .iter()
            .map(|t| format!("{:.3}", t.as_secs_f64() * 1e3))
            .collect();
        let (p50, _, max) = modes::percentiles(&mut times);
        eprintln!("shacl: {results} results, p50 {p50:.2} ms (max {max:.2})");
        report.sections.push((
            "shacl",
            format!(
                "{{\"results\": {results}, \"runs\": {}, \"p50_ms\": {p50:.3}, \"max_ms\": {max:.3}, \"samples_ms\": [{}]}}",
                args.runs,
                samples.join(", ")
            ),
        ));
    }
    let mut files: Vec<PathBuf> = if args.queries.as_os_str().is_empty() {
        Vec::new()
    } else {
        std::fs::read_dir(&args.queries)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "rq"))
            .filter(|p| {
                args.only.as_ref().is_none_or(|only| {
                    p.file_stem()
                        .is_some_and(|s| s.to_string_lossy().contains(only.as_str()))
                })
            })
            .collect()
    };
    files.sort();
    let texts: Vec<String> = files
        .iter()
        .map(std::fs::read_to_string)
        .collect::<Result<_, _>>()?;
    if let Some(file) = &args.commits {
        let pipeline = MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(reasoner.clone())),
        );
        report.sections.push((
            "commits",
            modes::commits(&pipeline, file, &texts, args.readers)?,
        ));
        if let Some(program) = &program {
            // The maintained closure against one computed afresh.
            let held = store.engine_stats().inferred;
            let fresh = store.rematerialise(program)?;
            eprintln!(
                "closure after the commits: maintained {held}, recomputed {}",
                fresh.inferred
            );
            report.sections.push((
                "closure_after_commits",
                format!(
                    "{{\"maintained\": {held}, \"recomputed\": {}, \"equal\": {}}}",
                    fresh.inferred,
                    held == fresh.inferred
                ),
            ));
        }
    }
    if args.clients > 0 {
        report.sections.push((
            "clients",
            modes::clients(&store, &texts, args.clients, args.duration)?,
        ));
    }
    if !args.canonicalize.is_empty() {
        report.sections.push((
            "canonicalize",
            modes::canonicalize(&args.canonicalize, args.runs)?,
        ));
    }
    if let Some(path) = &args.export {
        let started = Instant::now();
        let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(
            "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
        ))?;
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        store.run_query(&prepared, &CancellationToken::new(), file)?;
        eprintln!("exported in {:.2} s", started.elapsed().as_secs_f64());
    }
    let memory_after_load = memory_mib();
    eprintln!(
        "open {:.2} s{}",
        report.open_s,
        memory_after_load.map_or(String::new(), |(peak, rss)| format!(
            ", memory: peak {peak} MiB, resident {rss} MiB"
        ))
    );
    // Where the resident memory goes: the indexes, the dictionary's text and its index.
    let stats = store.engine_stats();
    let mib = |bytes: u64| bytes / 1048576;
    eprintln!(
        "held: {} quads + {} inferred in {} runs; index {} MiB heap + {} MiB mapped; \
         dictionary {} terms: text {} MiB, heap index {} MiB, mapped {} MiB",
        stats.quads,
        stats.inferred,
        stats.runs,
        mib(stats.index_bytes),
        mib(stats.index_mapped_bytes),
        stats.dictionary.terms,
        mib(stats.dictionary.arena_bytes),
        mib(stats.dictionary.index_bytes),
        mib(stats.dictionary.mapped_bytes),
    );
    report.sections.push((
        "held",
        format!(
            "{{\"quads\": {}, \"inferred\": {}, \"index_mib\": {}, \"dictionary_terms\": {}}}",
            stats.quads,
            stats.inferred,
            mib(stats.index_bytes),
            stats.dictionary.terms
        ),
    ));

    let baseline = args
        .baseline
        .as_ref()
        .map(read_baseline)
        .unwrap_or_default();
    if !files.is_empty() {
        println!(
            "{:<28} {:>10} {:>11} {:>10} {:>10} {:>9}",
            "query", "rows", "p50 ms", "min", "max", "vs base"
        );
    }
    let mut estimates: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for (file, text) in files.iter().zip(&texts) {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        let m = measure(&store, text, args);
        if let Some(dir) = &args.results {
            write_results(&store, text, args, &dir.join(format!("{name}.out")))?;
        }
        if args.explain {
            explain(&store, text);
        }
        if args.qerror {
            qerrors(&store, text, &mut estimates);
        }
        if let Some(error) = &m.error {
            println!("{name:<28} error: {error}");
            report.queries.push(format!(
                "    {{\n      \"name\": \"{name}\",\n      \"error\": {:?}\n    }}",
                error
            ));
            continue;
        }
        let p50 = ms(m.times[m.times.len() / 2]);
        let (min, max) = (ms(m.times[0]), ms(*m.times.last().unwrap()));
        report.sum_p50 += p50;
        let ratio = baseline
            .get(&name)
            .map_or(String::from("-"), |base| format!("{:.2}x", base / p50));
        println!(
            "{name:<28} {:>10} {p50:>11.2} {min:>10.2} {max:>10.2} {ratio:>9}",
            m.rows
        );
        let mut extra = String::new();
        if args.routes {
            if m.rows == 1
                && let Some(value) = first_value(&store, text)
            {
                extra.push_str(&format!(",\n      \"value\": {value:?}"));
            }
            extra.push_str(&format!(
                ",\n      \"operators\": {:?}",
                operators(&store, text)
            ));
        }
        let samples: Vec<String> = m.times.iter().map(|t| format!("{:.3}", ms(*t))).collect();
        report.queries.push(format!(
            "    {{\n      \"name\": \"{name}\",\n      \"rows\": {},\n      \"p50_ms\": {p50:.3},\n      \"min_ms\": {min:.3},\n      \"max_ms\": {max:.3},\n      \"samples_ms\": [{}]{extra}\n    }}",
            m.rows,
            samples.join(", ")
        ));
    }
    if !files.is_empty() {
        println!("sum of p50: {:.1} ms", report.sum_p50);
    }
    report.qerror = (args.qerror && !estimates.is_empty()).then(|| qerror_summary(&estimates));
    if let Some((peak, rss)) = memory_mib() {
        println!("memory: peak {peak} MiB, resident {rss} MiB");
    }
    Ok(())
}
