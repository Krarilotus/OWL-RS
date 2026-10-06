//! The perf lab's measurements beyond a query set, for the fast suite (`benches/fast`):
//! commits through the mutation pipeline with readers alongside, clients running the query
//! set concurrently, and blank-node canonicalisation.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nrese_store::{
    CancellationToken, MutationCommand, MutationPipeline, MutationTicket, PreparedQuery, Requester,
    SparqlQueryRequest, SparqlUpdateRequest, StoreService,
};

use crate::CountingSink;

/// `p50`, `p99` and `max` of `times`, in ms (sorted here).
pub fn percentiles(times: &mut [Duration]) -> (f64, f64, f64) {
    if times.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e3;
    (at(0.5), at(0.99), at(1.0))
}

/// Readers cycling through `queries` until `stop`, each from its own offset; their
/// latencies.
fn spawn_readers(
    store: &Arc<StoreService>,
    queries: &[String],
    readers: usize,
    stop: &Arc<AtomicBool>,
) -> Vec<std::thread::JoinHandle<Result<Vec<Duration>, String>>> {
    (0..readers)
        .map(|reader| {
            let (store, stop) = (Arc::clone(store), Arc::clone(stop));
            let queries = queries.to_vec();
            std::thread::spawn(move || {
                let prepared: Vec<PreparedQuery> = queries
                    .iter()
                    .map(|q| PreparedQuery::parse(&SparqlQueryRequest::all(q)))
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?;
                let mut times = Vec::new();
                let mut i = reader;
                while !stop.load(Ordering::Relaxed) && !prepared.is_empty() {
                    let started = Instant::now();
                    store
                        .run_query(
                            &prepared[i % prepared.len()],
                            &CancellationToken::new(),
                            &mut CountingSink::default(),
                        )
                        .map_err(|e| e.to_string())?;
                    times.push(started.elapsed());
                    i += 1;
                }
                Ok(times)
            })
        })
        .collect()
}

fn join_readers(
    handles: Vec<std::thread::JoinHandle<Result<Vec<Duration>, String>>>,
) -> Result<Vec<Duration>, String> {
    let mut all = Vec::new();
    for handle in handles {
        all.extend(
            handle
                .join()
                .map_err(|_| "a reader panicked".to_owned())??,
        );
    }
    Ok(all)
}

/// Applies every line of `file` (one SPARQL update each) as a commit through `pipeline`,
/// with `readers` threads running `queries` meanwhile. The JSON object of the
/// measurement: commit and read latencies, and whether every commit was maintained
/// incrementally (no full rematerialisation since `revision_before`).
pub fn commits(
    pipeline: &MutationPipeline,
    file: &Path,
    queries: &[String],
    readers: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let store = Arc::clone(pipeline.store());
    let updates: Vec<String> = std::fs::read_to_string(file)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    let rematerialised_before = store.last_materialisation().map(|m| m.revision);
    let stop = Arc::new(AtomicBool::new(false));
    let handles = spawn_readers(&store, queries, readers, &stop);
    let (mut times, mut inserted, mut deleted, mut rounds) = (Vec::new(), 0u64, 0u64, 0usize);
    let started = Instant::now();
    let mut failure = None;
    for update in &updates {
        let at = Instant::now();
        match pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update.clone())),
            &Requester::all(),
            &MutationTicket::new(),
        ) {
            Ok(_) => {}
            Err(e) => {
                failure = Some(format!("{e}"));
                break;
            }
        }
        times.push(at.elapsed());
        if let Some(run) = pipeline.last_reasoning_run() {
            inserted += run.inferred_inserted;
            deleted += run.inferred_deleted;
            rounds = rounds.max(run.rounds);
        }
    }
    let whole = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    let mut reads = join_readers(handles)?;
    let incremental = store.last_materialisation().map(|m| m.revision) == rematerialised_before;
    let (p50, p99, max) = percentiles(&mut times);
    let samples: Vec<String> = times
        .iter()
        .map(|t| format!("{:.4}", t.as_secs_f64() * 1e3))
        .collect();
    let (r50, r99, _) = percentiles(&mut reads);
    eprintln!(
        "commits: {} in {:.2} s, p50 {p50:.3} ms, p99 {p99:.3} ms, max {max:.1} ms; inferred +{inserted} -{deleted}; \
         {} reads (p50 {r50:.2} ms, p99 {r99:.2} ms) by {readers} readers; incremental: {incremental}",
        times.len(),
        whole.as_secs_f64(),
        reads.len(),
    );
    Ok(format!(
        "{{\"commits\": {}, \"whole_s\": {:.3}, \"p50_ms\": {p50:.4}, \"p99_ms\": {p99:.4}, \"max_ms\": {max:.3}, \
         \"inferred_inserted\": {inserted}, \"inferred_deleted\": {deleted}, \"max_rounds\": {rounds}, \
         \"incremental\": {incremental}, \"readers\": {readers}, \"reads\": {}, \"read_p50_ms\": {r50:.3}, \
         \"read_p99_ms\": {r99:.3}, \"samples_ms\": [{}]{}}}",
        times.len(),
        whole.as_secs_f64(),
        reads.len(),
        samples.join(", "),
        failure.map_or(String::new(), |f| format!(", \"error\": {f:?}"))
    ))
}

/// `clients` threads cycling through `queries` for `duration`: throughput and latency.
pub fn clients(
    store: &Arc<StoreService>,
    queries: &[String],
    clients: usize,
    duration: Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    let stop = Arc::new(AtomicBool::new(false));
    let handles = spawn_readers(store, queries, clients, &stop);
    std::thread::sleep(duration);
    stop.store(true, Ordering::Relaxed);
    let mut times = join_readers(handles)?;
    let qps = times.len() as f64 / duration.as_secs_f64();
    let (p50, p99, max) = percentiles(&mut times);
    eprintln!(
        "clients: {clients} for {:.0} s: {} queries, {qps:.0}/s, p50 {p50:.3} ms, p99 {p99:.3} ms",
        duration.as_secs_f64(),
        times.len()
    );
    Ok(format!(
        "{{\"clients\": {clients}, \"queries\": {}, \"qps\": {qps:.1}, \"p50_ms\": {p50:.3}, \"p99_ms\": {p99:.3}, \"max_ms\": {max:.3}}}",
        times.len()
    ))
}

/// Canonicalises each N-Triples file `runs` times; every file must come out the same (the
/// fast suite passes one graph under different labels and orders).
pub fn canonicalize(
    files: &[std::path::PathBuf],
    runs: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    use nrese_rdf_io::{RdfFormat, RdfParser};
    let mut graphs = Vec::new();
    for file in files {
        let bytes = std::fs::read(file)?;
        let mut graph = nrese_rdf::Graph::new();
        for triple in RdfParser::from_format(RdfFormat::NTriples).for_slice(&bytes) {
            graph.insert(&nrese_rdf::Triple::from(triple?));
        }
        graphs.push(graph);
    }
    let mut times = Vec::new();
    let mut forms = Vec::new();
    for _ in 0..runs {
        forms.clear();
        for graph in &graphs {
            let mut graph = graph.clone();
            let at = Instant::now();
            graph.canonicalize();
            times.push(at.elapsed());
            forms.push(graph.to_string());
        }
    }
    let equal = forms.windows(2).all(|w| w[0] == w[1]);
    let (p50, _, max) = percentiles(&mut times);
    eprintln!(
        "canonicalised {} graphs × {runs}: p50 {p50:.2} ms, max {max:.2} ms; equal: {equal}",
        graphs.len()
    );
    Ok(format!(
        "{{\"graphs\": {}, \"runs\": {runs}, \"p50_ms\": {p50:.3}, \"max_ms\": {max:.3}, \"equal\": {equal}}}",
        graphs.len()
    ))
}

/// SplitMix64, for the kernels' data (the same as the generators').
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A kernel on generated data, `runs` times: `sort-u128-N` sorts N random 128-bit keys
/// (two id columns) with the executor's `IdTable::sort_by`, and, for scale, the same keys as
/// `u128` with the standard library's unstable sort.
pub fn kernel(name: &str, runs: usize) -> Result<String, Box<dyn std::error::Error>> {
    let rows: usize = name
        .strip_prefix("sort-u128-")
        .ok_or(format!("unknown kernel {name}"))?
        .parse()?;
    let mut state = 1u64;
    let a: Vec<u64> = (0..rows).map(|_| splitmix(&mut state)).collect();
    let b: Vec<u64> = (0..rows).map(|_| splitmix(&mut state)).collect();
    let (mut table_times, mut std_times) = (Vec::new(), Vec::new());
    let mut sorted = true;
    for _ in 0..runs {
        let mut table = nrese_exec::IdTable::from_columns(vec![a.clone(), b.clone()]);
        let at = Instant::now();
        table.sort_by(&[0, 1]);
        table_times.push(at.elapsed());
        sorted &= (1..table.len()).all(|i| {
            (table.get(i - 1, 0), table.get(i - 1, 1)) <= (table.get(i, 0), table.get(i, 1))
        });
        let mut keys: Vec<u128> = a
            .iter()
            .zip(&b)
            .map(|(&x, &y)| (u128::from(x) << 64) | u128::from(y))
            .collect();
        let at = Instant::now();
        keys.sort_unstable();
        std_times.push(at.elapsed());
    }
    let samples: Vec<String> = table_times
        .iter()
        .map(|t| format!("{:.3}", t.as_secs_f64() * 1e3))
        .collect();
    let (p50, _, _) = percentiles(&mut table_times);
    let (std50, _, _) = percentiles(&mut std_times);
    let megabytes = (rows * 16) as f64 / 1048576.0;
    eprintln!(
        "{name}: IdTable::sort_by p50 {p50:.1} ms ({:.0} MB/s), std sort_unstable {std50:.1} ms; sorted: {sorted}",
        megabytes / (p50 / 1e3)
    );
    Ok(format!(
        "{{\"name\": {name:?}, \"rows\": {rows}, \"p50_ms\": {p50:.3}, \"mb_per_s\": {:.1}, \"std_p50_ms\": {std50:.3}, \
         \"sorted\": {sorted}, \"samples_ms\": [{}]}}",
        megabytes / (p50 / 1e3),
        samples.join(", ")
    ))
}

/// Parsing and planning a query without running it, `runs` times: the cost of the parser
/// and the rewrites on large or deeply nested queries. The JSON object of a query in the
/// report, with the plan's size (steps and rewrites) as its counts.
pub fn parse_and_plan(
    store: &StoreService,
    name: &str,
    text: &str,
    runs: usize,
) -> Result<(String, f64), Box<dyn std::error::Error>> {
    let mut times = Vec::new();
    let (mut steps, mut rewrites) = (0, 0);
    for _ in 0..runs {
        let at = Instant::now();
        let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(text))?;
        let plan = store.plan_query(&prepared)?;
        times.push(at.elapsed());
        steps = plan.steps.len();
        rewrites = plan.rewrites.len();
    }
    let samples: Vec<String> = times
        .iter()
        .map(|t| format!("{:.3}", t.as_secs_f64() * 1e3))
        .collect();
    let (p50, _, max) = percentiles(&mut times);
    println!("{name:<28} parse and plan p50 {p50:.2} ms, {steps} steps, {rewrites} rewrites");
    let json = format!(
        "    {{\n      \"name\": \"{name}\",\n      \"rows\": {steps},\n      \"p50_ms\": {p50:.3},\n      \"max_ms\": {max:.3},\n      \"steps\": {steps},\n      \"rewrites\": {rewrites},\n      \"samples_ms\": [{}]\n    }}",
        samples.join(", ")
    );
    Ok((json, p50))
}
