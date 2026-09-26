//! Reasoning benchmark baseline for the v1 reasoner (`rules-mvp`): RT1 (materialisation) and
//! RT4 (consistency) in the terms of `docs/design/reasoning-benchmark.md`.
//!
//! ```text
//! cargo run --release -p nrese-store --example reason_bench -- [--derived out.nt] input.nt...
//! ```
//!
//! Bulk-loads the inputs into an in-memory store, takes the snapshot the commit gate uses,
//! and runs `rules-mvp` once. It reports load, snapshot and reasoning times, the number of
//! derived triples and consistency violations, and optionally writes the derived triples as
//! N-Triples to diff against an oracle.
//!
//! This is v1's honest baseline. Inferences aren't queryable yet (asserted-only read model),
//! and v1 skips triples with literals or blank nodes, so it can't answer LUBM queries. M3
//! replaces it.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use nrese_core::ReasonerEngine;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).peekable();
    let mut derived_out = None;
    if args.peek().is_some_and(|arg| arg == "--derived") {
        args.next();
        derived_out = args.next().map(PathBuf::from);
    }
    let files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if files.is_empty() {
        return Err("usage: reason_bench [--derived out.nt] input.nt...".into());
    }

    let store = StoreService::new(StoreConfig::in_memory())?;
    let started = Instant::now();
    let report = store.bulk_load(&BulkLoadRequest {
        files,
        replace: false,
        graph: GraphTarget::DefaultGraph,
    })?;
    let load = started.elapsed();

    let started = Instant::now();
    let snapshot = store.dataset_snapshot()?;
    let snapshot_time = started.elapsed();

    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let started = Instant::now();
    let plan = reasoner.plan(&snapshot)?;
    let output = reasoner.run(&snapshot, &plan)?;
    let reasoning = started.elapsed();

    let inferred = &output.inferred;
    println!(
        "asserted {} | load {:.2} s | snapshot {:.2} s | rules-mvp {:.2} s | derived {} | violations {}",
        report.inserted,
        load.as_secs_f64(),
        snapshot_time.as_secs_f64(),
        reasoning.as_secs_f64(),
        inferred.inferred_triples,
        inferred.consistency_violations,
    );
    for diagnostic in inferred.diagnostics.iter().take(10) {
        println!("  diagnostic: {diagnostic}");
    }
    if let Some(path) = derived_out {
        let mut out = String::new();
        for (subject, predicate, object) in &inferred.derived_triples {
            let _ = writeln!(out, "<{subject}> <{predicate}> <{object}> .");
        }
        std::fs::write(path, out)?;
    }
    Ok(())
}
