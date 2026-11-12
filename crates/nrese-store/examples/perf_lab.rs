//! Perf lab: the in-process measurement loop for performance work (ROADMAP §3.1, PL).
//!
//! ```text
//! cargo run --release -p nrese-store --example perf_lab -- \
//!     [--store DIR] [--load FILE]... [--queries DIR] [--runs 5] [--warmup 1] \
//!     [--timeout-s 120] [--only SUBSTRING] [--label NAME] [--json OUT] [--baseline JSON] \
//!     [--explain] [--format tsv|json|xml|csv] [--shapes FILE] [--shacl-report FILE] [--reason RULESET]
//!     [--reason-runs 1] [--routes]
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
//!   data against it (`--runs` times), before the queries (`shacl` in the JSON: results,
//!   samples, p50, max); `--shacl-report FILE` writes the last run's validation report there
//!   as N-Triples, after the timing.
//!   `--format` serialises SELECT results in another format than TSV (JSON is what most
//!   clients ask for).
//!   `--reason` materialises a ruleset (`owl2-rl`, `rdfs`, ...) after the load, timed, and
//!   the queries read the materialised data (the default read model). `--reason-runs N`
//!   materialises it N times, each replacing the inferred stack, and reports each run's time
//!   as the store measures it (`reason` in the JSON: samples, p50, max).
//!   `--explain` prints each query's plan after its measurement: every operator with its
//!   estimated and actual rows and its time (inputs included), where the time goes when a
//!   profiler isn't at hand.
//!   `--routes` records, after a query's measured runs, the operators it ran with (from
//!   EXPLAIN) and, for a one-row result, its first value (`value`, the answer of a count)
//!   in the JSON report, for the fast suite's content and route checks. The JSON also
//!   records the quads loaded (`loaded`) and the materialisation's counts (`reason`).
//!   This perf lab has no query status and no rewrites: those checks find nothing here.
//!
//! This is the loop every Phase 2–5 work package is measured with before the Docker
//! scorecard confirms it against other systems. It runs anywhere the data is; the wrapper
//! `benches/perf-lab.sh` runs it on Linux in Docker against the benchmark volume.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphResultFormat, GraphTarget, PreparedQuery,
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
    format: SolutionsResultFormat,
    shapes: Option<PathBuf>,
    shacl_report: Option<PathBuf>,
    reason: Option<nrese_reasoner::v2::rulesets::Ruleset>,
    reason_runs: usize,
    routes: bool,
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
        shacl_report: None,
        reason: None,
        reason_runs: 1,
        routes: false,
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
            "--routes" => args.routes = true,
            "--reason-runs" => {
                args.reason_runs = value()?
                    .parse()
                    .map_err(|e| format!("--reason-runs: {e}"))?
            }
            "--shapes" => args.shapes = Some(value()?.into()),
            "--shacl-report" => args.shacl_report = Some(value()?.into()),
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
    if args.runs == 0 {
        return Err("--runs must be > 0".into());
    }
    if args.reason_runs == 0 {
        return Err("--reason-runs must be > 0".into());
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
    // The process's watchdog cancels at the timeout: armed before the timed window, disarmed
    // after it, so that no thread starts inside it (benches/fast/patches/perf_lab_watchdog.py).
    let armed = perf_lab_watchdog::arm(&token, timeout);
    let mut sink = CountingSink::default();
    let started = Instant::now();
    let result = store.run_query(prepared, &token, &mut sink);
    let elapsed = started.elapsed();
    drop(armed);
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

/// The first value of a one-row result (a count's answer), as its lexical form (`--routes`,
/// after the measured runs).
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

/// The distinct operators `text` runs with, in the order EXPLAIN lists them (`--routes`,
/// after the measured runs).
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
    // SAFETY: mimalloc's `mi_collect` may be called at any time, from any thread.
    nrese_engine::memory::set_release(|force| unsafe { libmimalloc_sys::mi_collect(force) });
    let args = parse_args().map_err(|e| format!("{e}\nsee the usage at the top of perf_lab.rs"))?;
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
    let config = StoreConfig {
        query_cache_bytes: 0,
        bulk_load_memory_bytes,
        index_encoding,
        vocabulary,
        equality_by_representatives,
        equality_compact,
        ..config
    };
    let started = Instant::now();
    let store = StoreService::new(config)?;
    let open_s = started.elapsed().as_secs_f64();
    let mut load_s = 0.0;
    // The report's own fields (JSON `loaded`, `reason`): counts the run has anyway.
    let mut sections: Vec<(&str, String)> = Vec::new();
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
        sections.push(("loaded", report.inserted.to_string()));
    }
    if let Some(ruleset) = args.reason {
        // `--reason-runs` times: each run replaces the inferred stack; its time is the one the
        // store measures for it (`elapsed`), as the head's perf lab reports it.
        let mut times = Vec::with_capacity(args.reason_runs);
        let mut last = None;
        for _ in 0..args.reason_runs {
            let report = store.rematerialise(ruleset)?;
            times.push(report.elapsed);
            last = Some(report);
        }
        let report = last.expect("--reason-runs is at least 1");
        times.sort();
        let at = |q: f64| ms(times[((times.len() - 1) as f64 * q).round() as usize]);
        let (p50, max) = (at(0.5), at(1.0));
        eprintln!(
            "reasoned ({}): {} inferred, {} rounds, p50 {p50:.1} ms over {} runs",
            ruleset.name(),
            report.inferred,
            report.rounds,
            args.reason_runs
        );
        let samples: Vec<String> = times.iter().map(|t| format!("{:.3}", ms(*t))).collect();
        sections.push((
            "reason",
            format!(
                "{{\"program\": {:?}, \"asserted\": {}, \"inferred\": {}, \"violations\": {}, \"rounds\": {}, \
                 \"runs\": {}, \"p50_ms\": {p50:.3}, \"max_ms\": {max:.3}, \"samples_ms\": [{}]}}",
                report.ruleset,
                report.asserted,
                report.inferred,
                report.violations,
                report.rounds,
                args.reason_runs,
                samples.join(", ")
            ),
        ));
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
        let mut last = None;
        for run in 0..args.runs {
            let started = Instant::now();
            let validation = store.validate_shacl(
                &nrese_store::ReadScope::All,
                &ShaclValidationRequest::default(),
            )?;
            times.push(started.elapsed());
            results = validation.report.results.len();
            // Only the last run's report is kept, so every earlier one is dropped where it was.
            last = (run + 1 == args.runs).then_some(validation);
        }
        if let (Some(path), Some(validation)) = (&args.shacl_report, &last) {
            std::fs::write(path, validation.serialize(GraphResultFormat::NTriples)?)?;
        }
        let samples: Vec<String> = times.iter().map(|t| format!("{:.3}", ms(*t))).collect();
        times.sort();
        eprintln!(
            "shacl: {results} results, p50 {:.2} ms (min {:.2})",
            ms(times[times.len() / 2]),
            ms(times[0])
        );
        sections.push((
            "shacl",
            format!(
                "{{\"results\": {results}, \"runs\": {}, \"p50_ms\": {:.3}, \"max_ms\": {:.3}, \"samples_ms\": [{}]}}",
                args.runs,
                ms(times[times.len() / 2]),
                ms(times[times.len() - 1]),
                samples.join(", ")
            ),
        ));
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

    // No `--queries`: a load-only run (load, persist, exit), the harness's setup step.
    let listed = (!args.queries.as_os_str().is_empty())
        .then(|| std::fs::read_dir(&args.queries))
        .transpose()?;
    let mut files: Vec<PathBuf> = listed
        .into_iter()
        .flatten()
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
        // After the measured runs, outside every timed window (as the head's perf lab).
        let mut extra = String::new();
        if args.routes {
            if m.rows == 1
                && let Some(value) = first_value(&store, &text)
            {
                extra.push_str(&format!(",\n      \"value\": {value:?}"));
            }
            extra.push_str(&format!(
                ",\n      \"operators\": {:?}",
                operators(&store, &text)
            ));
        }
        report.push(format!(
            "    {{\n      \"name\": \"{name}\",\n      \"rows\": {},\n      \"p50_ms\": {p50:.3},\n      \"min_ms\": {min:.3},\n      \"max_ms\": {max:.3}{extra}\n    }}",
            m.rows
        ));
    }
    let memory = memory_mib();
    println!("sum of p50: {sum_p50:.1} ms");
    if let Some((peak, rss)) = memory {
        println!("memory: peak {peak} MiB, resident {rss} MiB");
    }
    if let Some(path) = &args.json {
        let fields: String = sections
            .iter()
            .map(|(key, value)| format!("  \"{key}\": {value},\n"))
            .collect();
        let json = format!(
            "{{\n  \"label\": {:?},\n  \"open_s\": {open_s:.6},\n  \"load_s\": {load_s:.3},\n  \"peak_mib\": {},\n  \"sum_p50_ms\": {sum_p50:.3},\n{fields}  \"queries\": [\n{}\n  ]\n}}\n",
            args.label,
            memory.map_or(0, |(peak, _)| peak),
            report.join(",\n")
        );
        std::fs::write(path, json)?;
    }
    Ok(())
}

/// The timeout of every timed run, by one thread for the whole process (release gate harness:
/// benches/fast/patches/perf_lab_watchdog.py): started once, before any timing, and running
/// before it returns; it polls every 10 ms and takes the slot's lock only then, which the timed
/// call never takes.
mod perf_lab_watchdog {
    use super::CancellationToken;
    use std::sync::{Mutex, OnceLock, mpsc};
    use std::time::{Duration, Instant};

    type Slot = Mutex<Option<(Instant, CancellationToken)>>;
    const POLL: Duration = Duration::from_millis(10);

    fn slot() -> &'static Slot {
        static SLOT: OnceLock<Slot> = OnceLock::new();
        static STARTED: OnceLock<()> = OnceLock::new();
        let slot: &'static Slot = SLOT.get_or_init(|| Mutex::new(None));
        STARTED.get_or_init(|| {
            let (ready, started) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = ready.send(());
                loop {
                    std::thread::sleep(POLL);
                    let mut armed = slot.lock().unwrap_or_else(|e| e.into_inner());
                    if armed
                        .as_ref()
                        .is_some_and(|(deadline, _)| Instant::now() >= *deadline)
                        && let Some((_, token)) = armed.take()
                    {
                        token.cancel();
                    }
                }
            });
            let _ = started.recv();
        });
        slot
    }

    /// Disarms the watchdog when dropped.
    pub struct Armed;

    impl Drop for Armed {
        fn drop(&mut self) {
            *slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// Cancels `token` once `timeout` has passed, unless the guard is dropped first.
    pub fn arm(token: &CancellationToken, timeout: Duration) -> Armed {
        *slot().lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now() + timeout, token.clone()));
        Armed
    }
}
