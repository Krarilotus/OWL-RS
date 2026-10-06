//! The `owl2-dl` mode's costs in the store (G3): the first commit (the RL closure, U1
//! compiled and evaluated, the consistency check), then single-assertion commits under
//! `owl2-dl` and `owl2-rl`, interleaved, and the bounds' state.
//!
//! ```text
//! cargo run --release -p nrese-store --example dl_lab -- \
//!     [--commits N] [--rounds R] [--class IRI] [--property IRI] input.nt...
//! ```
//!
//! - Each round builds one store per mode from the same files (bulk loaded), commits
//!   once (the first commit materialises and prepares the bounds), then `--commits`
//!   single assertions: alternately `new_i a CLASS` and `new_i PROPERTY new_{i-1}`.
//! - Prints, per mode, the medians over rounds of the first commit and of the per-commit
//!   median, the engine that decided the last commit's consistency, and U1's size; counts
//!   (statements) before times.

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
    first: Duration,
    per_commit: Duration,
    statements: u64,
    engine: String,
    upper: String,
}

fn run(mode: ReasoningMode, files: &[PathBuf], commits: usize, class: &str, property: &str) -> Run {
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
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
    let store = pipeline.store();
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
                    "U1: {} facts beyond L, {} classes and {} predicates in the gap, last update {}{}",
                    bounds.upper_facts,
                    bounds.gap_classes,
                    bounds.gap_predicates,
                    bounds.last,
                    bounds
                        .unavailable
                        .map(|why| format!(" ({why})"))
                        .unwrap_or_default()
                ),
            )
        }
        false => ("rules".to_owned(), String::new()),
    };
    Run {
        first,
        per_commit: median(times),
        statements,
        engine,
        upper,
    }
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
        "http://swat.cse.lehigh.edu/onto/univ-bench.owl#GraduateStudent".to_owned()
    });
    let property = flag("--property")
        .unwrap_or_else(|| "http://swat.cse.lehigh.edu/onto/univ-bench.owl#advisor".to_owned());
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
    let modes = [ReasoningMode::Owl2Dl, ReasoningMode::Owl2Rl];
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
    for (mode, runs) in modes.iter().zip(results) {
        let first = median(runs.iter().map(|r| r.first).collect());
        let per = median(runs.iter().map(|r| r.per_commit).collect());
        let last = runs.last().expect("a round");
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
