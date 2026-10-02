//! Perf lab: the in-process measurement loop for performance work (ROADMAP §3.1, PL).
//!
//! ```text
//! cargo run --release -p nrese-store --example perf_lab -- \
//!     [--store DIR] [--load FILE]... --queries DIR [--runs 5] [--warmup 1] \
//!     [--timeout-s 120] [--only SUBSTRING] [--label NAME] [--json OUT] [--baseline JSON] \
//!     [--explain] [--format tsv|json|xml|csv] [--shapes FILE] [--reason RULESET]
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
//!   `--explain` prints each query's plan after its measurement: every operator with its
//!   estimated and actual rows and its time (inputs included), where the time goes when a
//!   profiler isn't at hand.
//!
//! This is the loop every Phase 2–5 work package is measured with before the Docker
//! scorecard confirms it against other systems. It runs anywhere the data is; the wrapper
//! `benches/perf-lab.sh` runs it on Linux in Docker against the benchmark volume.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphTarget, PreparedQuery, QueryResultKind,
    ShaclValidationRequest, SolutionsResultFormat, SparqlQueryRequest, StoreConfig, StoreService,
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
    format: SolutionsResultFormat,
    shapes: Option<PathBuf>,
    reason: Option<nrese_reasoner::v2::rulesets::Ruleset>,
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
        format: SolutionsResultFormat::Tsv,
        shapes: None,
        reason: None,
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
            "--shapes" => args.shapes = Some(value()?.into()),
            "--reason" => {
                let name = value()?;
                args.reason = Some(
                    nrese_reasoner::v2::rulesets::Ruleset::from_name(&name)
                        .ok_or(format!("--reason: unknown ruleset {name}"))?,
                );
            }
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
    if args.queries.as_os_str().is_empty() || args.runs == 0 {
        return Err("--queries DIR is required and --runs must be > 0".into());
    }
    Ok(args)
}

/// Counts bytes and lines written, discarding the data.
#[derive(Default)]
struct CountingSink {
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

fn measure(store: &StoreService, text: &str, args: &Args) -> Measured {
    let mut request = SparqlQueryRequest::new(text);
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
    let explained = PreparedQuery::parse(&SparqlQueryRequest::new(text))
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(system_alloc))]
    nrese_engine::memory::set_release(|force| unsafe { libmimalloc_sys::mi_collect(force) });
    let args = parse_args().map_err(|e| format!("{e}\nsee the usage at the top of perf_lab.rs"))?;
    let config = match &args.store {
        Some(dir) => StoreConfig::on_disk(dir),
        None => StoreConfig::in_memory(),
    };
    // No result cache: repeated runs measure evaluation.
    let config = StoreConfig {
        query_cache_bytes: 0,
        ..config
    };
    let started = Instant::now();
    let store = StoreService::new(config)?;
    let open_s = started.elapsed().as_secs_f64();
    let mut load_s = 0.0;
    if !args.load.is_empty() {
        let started = Instant::now();
        let report = store.bulk_load(&BulkLoadRequest {
            files: args.load.clone(),
            replace: false,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })?;
        load_s = started.elapsed().as_secs_f64();
        eprintln!("loaded {} quads in {load_s:.2} s", report.inserted);
    }
    if let Some(ruleset) = args.reason {
        let started = Instant::now();
        let report = store.rematerialise(ruleset)?;
        eprintln!(
            "reasoned ({}): {} inferred in {:.2} s",
            ruleset.name(),
            report.inferred,
            started.elapsed().as_secs_f64()
        );
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
            let validation = store.validate_shacl(&ShaclValidationRequest::default())?;
            times.push(started.elapsed());
            results = validation.report.results.len();
        }
        times.sort();
        eprintln!(
            "shacl: {results} results, p50 {:.2} ms (min {:.2})",
            ms(times[times.len() / 2]),
            ms(times[0])
        );
    }
    let memory_after_load = memory_mib();
    eprintln!(
        "open {open_s:.2} s{}",
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

    let mut files: Vec<PathBuf> = std::fs::read_dir(&args.queries)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rq"))
        .filter(|p| {
            args.only.as_ref().is_none_or(|only| {
                p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().contains(only.as_str()))
            })
        })
        .collect();
    files.sort();
    let baseline = args
        .baseline
        .as_ref()
        .map(read_baseline)
        .unwrap_or_default();

    println!(
        "{:<28} {:>10} {:>11} {:>10} {:>10} {:>9}",
        "query", "rows", "p50 ms", "min", "max", "vs base"
    );
    let mut report = Vec::new();
    let mut sum_p50 = 0.0;
    for file in &files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(file)?;
        let m = measure(&store, &text, &args);
        if args.explain {
            explain(&store, &text);
        }
        if let Some(error) = &m.error {
            println!("{name:<28} error: {error}");
            report.push(format!(
                "    {{\n      \"name\": \"{name}\",\n      \"error\": {:?}\n    }}",
                error
            ));
            continue;
        }
        let p50 = ms(m.times[m.times.len() / 2]);
        let (min, max) = (ms(m.times[0]), ms(*m.times.last().unwrap()));
        sum_p50 += p50;
        let ratio = baseline
            .get(&name)
            .map_or(String::from("-"), |base| format!("{:.2}x", base / p50));
        println!(
            "{name:<28} {:>10} {p50:>11.2} {min:>10.2} {max:>10.2} {ratio:>9}",
            m.rows
        );
        report.push(format!(
            "    {{\n      \"name\": \"{name}\",\n      \"rows\": {},\n      \"p50_ms\": {p50:.3},\n      \"min_ms\": {min:.3},\n      \"max_ms\": {max:.3}\n    }}",
            m.rows
        ));
    }
    let memory = memory_mib();
    println!("sum of p50: {sum_p50:.1} ms");
    if let Some((peak, rss)) = memory {
        println!("memory: peak {peak} MiB, resident {rss} MiB");
    }
    if let Some(path) = &args.json {
        let json = format!(
            "{{\n  \"label\": {:?},\n  \"open_s\": {open_s:.3},\n  \"load_s\": {load_s:.3},\n  \"peak_mib\": {},\n  \"sum_p50_ms\": {sum_p50:.3},\n  \"queries\": [\n{}\n  ]\n}}\n",
            args.label,
            memory.map_or(0, |(peak, _)| peak),
            report.join(",\n")
        );
        std::fs::write(path, json)?;
    }
    Ok(())
}
