//! The `owl2-dl` mode's costs in the store (G3): the first commit (the RL closure, U1
//! compiled and evaluated, the consistency check), then single-assertion commits under
//! `owl2-dl` and `owl2-rl`, interleaved, and the bounds' state.
//!
//! ```text
//! cargo run --release -p nrese-store --example dl_lab -- \
//!     [--commits N] [--rounds R] [--class IRI] [--property IRI] [--queries DIR] input.nt...
//! ```
//!
//! - Each round builds one store per mode from the same files (bulk loaded), commits
//!   once (the first commit materialises and prepares the bounds), then `--commits`
//!   single assertions: alternately `new_i a CLASS` and `new_i PROPERTY new_{i-1}`.
//! - Prints, per mode, the medians over rounds of the first commit and of the per-commit
//!   median, the engine that decided the last commit's consistency, and U1's size; counts
//!   (statements) before times.
//! - `DL_LAB_TIMEOUT_MS`: a shorter `dl.timeout`, to see where a DL task gives up;
//!   `DL_LAB_RL_ONLY`: both stores under `owl2-rl` (an ontology `owl2-dl` can't take).
//! - `--queries DIR`: then each `.rq` file of DIR in both modes (one warm-up, then the
//!   median of 5, interleaved): rows, under `owl2-dl` the status and the paths that
//!   decided, and the times.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, GraphTarget, MutationCommand, MutationPipeline, MutationTicket, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

fn commit(pipeline: &MutationPipeline, update: &str) -> Duration {
    let started = Instant::now();
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("commit");
    started.elapsed()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

struct Run {
    pipeline: MutationPipeline,
    first: Duration,
    per_commit: Duration,
    statements: u64,
    engine: String,
    upper: String,
}

fn run(mode: ReasoningMode, files: &[PathBuf], commits: usize, class: &str, property: &str) -> Run {
    // No result cache: every run evaluates (repeats would be copies from it).
    // `DL_LAB_TIMEOUT_MS`: a shorter `dl.timeout`, to see where a DL task gives up.
    let mut dl = nrese_store::DlConfig::default();
    if let Some(ms) = std::env::var("DL_LAB_TIMEOUT_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
    {
        dl.timeout = Duration::from_millis(ms);
    }
    let store = StoreService::new(StoreConfig {
        query_cache_bytes: 0,
        dl,
        ..StoreConfig::in_memory()
    })
    .expect("store");
    store
        .bulk_load(&BulkLoadRequest {
            files: files.to_vec(),
            replace: false,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })
        .expect("load");
    let pipeline = MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    let first = commit(
        &pipeline,
        &format!("INSERT DATA {{ <urn:lab:new0> a <{class}> }}"),
    );
    let mut times = Vec::with_capacity(commits);
    for i in 1..=commits {
        let update = match i % 2 {
            0 => format!("INSERT DATA {{ <urn:lab:new{i}> a <{class}> }}"),
            _ => format!(
                "INSERT DATA {{ <urn:lab:new{i}> <{property}> <urn:lab:new{}> }}",
                i - 1
            ),
        };
        times.push(commit(&pipeline, &update));
    }
    let store = pipeline.store().clone();
    let statements = store.stats().map(|s| s.quad_count as u64).unwrap_or(0);
    let (engine, upper) = match mode.is_dl() {
        true => {
            let status = store.dl().status().expect("a DL status");
            let bounds = store.dl_bounds();
            (
                format!(
                    "{} ({})",
                    status.consistency.engine,
                    status.consistency.verdict.as_str()
                ),
                format!(
                    "L: {} memberships from the taxonomy; U1: {} facts beyond L, {} classes and {} predicates in the gap, last update {}{}",
                    bounds.lower_facts,
                    bounds.upper_facts,
                    bounds.gap_classes,
                    bounds.gap_predicates,
                    bounds.last,
                    bounds
                        .unavailable
                        .map(|why| format!(" ({why})"))
                        .unwrap_or_default()
                ) + &gap(&store),
            )
        }
        false => ("rules".to_owned(), String::new()),
    };
    Run {
        pipeline,
        first,
        per_commit: median(times),
        statements,
        engine,
        upper,
    }
}

/// U1's facts beyond L per predicate (`a C` for a class), and how many of them name no
/// Skolem constant: the facts that can make an answer of their own.
fn gap(store: &StoreService) -> String {
    const SKOLEM: &str = "urn:nrese:u1:";
    const TYPE: &str = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
    let mut by: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for [s, p, o] in store.dl_upper_facts(false).unwrap_or_default() {
        let key = match p == TYPE {
            true => format!("a {o}"),
            false => p,
        };
        let named = !s.contains(SKOLEM) && !o.contains(SKOLEM);
        let entry = by.entry(key).or_default();
        entry.0 += 1;
        entry.1 += usize::from(named);
    }
    by.iter()
        .map(|(key, (all, named))| {
            format!("\n  gap {key}: {all} facts, {named} without a Skolem constant")
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let commits: usize = flag("--commits").and_then(|n| n.parse().ok()).unwrap_or(50);
    let rounds: usize = flag("--rounds").and_then(|n| n.parse().ok()).unwrap_or(3);
    let class = flag("--class").unwrap_or_else(|| {
        "http://www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#GraduateStudent".to_owned()
    });
    let property = flag("--property").unwrap_or_else(|| {
        "http://www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#advisor".to_owned()
    });
    let mut skip = false;
    let files: Vec<PathBuf> = args
        .iter()
        .filter(|a| {
            let keep = !skip && !a.starts_with("--");
            skip = a.starts_with("--");
            keep
        })
        .map(PathBuf::from)
        .collect();
    let modes = match std::env::var("DL_LAB_RL_ONLY").is_ok() {
        true => [ReasoningMode::Owl2Rl, ReasoningMode::Owl2Rl],
        false => [ReasoningMode::Owl2Dl, ReasoningMode::Owl2Rl],
    };
    let mut results: Vec<Vec<Run>> = modes.iter().map(|_| Vec::new()).collect();
    for round in 0..rounds {
        // Interleaved, in alternating order.
        let order: Vec<usize> = match round % 2 {
            0 => vec![0, 1],
            _ => vec![1, 0],
        };
        for m in order {
            results[m].push(run(modes[m], &files, commits, &class, &property));
        }
    }
    let queries = flag("--queries");
    let last: Vec<&MutationPipeline> = results
        .iter()
        .map(|runs| &runs.last().expect("a round").pipeline)
        .collect();
    if let Some(dir) = &queries {
        compare_queries(dir, &last);
    }
    for (mode, runs) in modes.iter().zip(&results) {
        let first = median(runs.iter().map(|r| r.first).collect());
        let per = median(runs.iter().map(|r| r.per_commit).collect());
        let last = runs.last().expect("a round");
        let _ = &last.pipeline;
        println!(
            "{}: {} statements; first commit {:.1?} (median of {rounds}); single-assertion \
             commit {:.2?} (median of {rounds} per-run medians of {commits}); consistency by {}",
            mode.as_str(),
            last.statements,
            first,
            per,
            last.engine
        );
        if !last.upper.is_empty() {
            println!("  {}", last.upper);
        }
    }
}

/// One query's rows (each row once) and status, timed.
fn timed(
    pipeline: &MutationPipeline,
    text: &str,
) -> (
    usize,
    Option<(nrese_sparql::Completeness, nrese_store::DlDetail)>,
    Duration,
) {
    let store = pipeline.store();
    let prepared = store
        .prepare_query(&nrese_store::SparqlQueryRequest::all(text))
        .expect("query parses");
    let started = Instant::now();
    let mut out = Vec::new();
    let status = store
        .run_query_dl(&prepared, &nrese_store::CancellationToken::new(), &mut out)
        .expect("query runs");
    let elapsed = started.elapsed();
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap_or_default();
    let rows = json["results"]["bindings"].as_array().map_or(0, |rows| {
        rows.iter()
            .map(|r| r.to_string())
            .collect::<std::collections::HashSet<_>>()
            .len()
    });
    (rows, status, elapsed)
}

/// Each query of `dir` on both stores (owl2-dl first), interleaved medians of 5 runs.
fn compare_queries(dir: &str, stores: &[&MutationPipeline]) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("queries")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "rq"))
        .collect();
    files.sort();
    println!("query	dl rows	rl rows	dl status	dl paths	dl ms	rl ms");
    for file in files {
        let text = std::fs::read_to_string(&file).expect("query file");
        let name = file.file_stem().unwrap_or_default().to_string_lossy();
        let mut times = [Vec::new(), Vec::new()];
        let mut rows = [0, 0];
        let mut status = None;
        for run in 0..6 {
            for (i, pipeline) in stores.iter().enumerate() {
                let (n, s, t) = timed(pipeline, &text);
                if run > 0 {
                    times[i].push(t);
                }
                rows[i] = n;
                if i == 0 {
                    status = s;
                }
            }
        }
        let [dl, rl] = times;
        let (label, paths) = status.map_or(("-".to_owned(), String::new()), |s| {
            (s.0.as_str().to_owned(), s.1.paths.join(","))
        });
        println!(
            "{name}	{}	{}	{label}	{paths}	{:.2}	{:.2}",
            rows[0],
            rows[1],
            median(dl).as_secs_f64() * 1000.0,
            median(rl).as_secs_f64() * 1000.0
        );
    }
}
