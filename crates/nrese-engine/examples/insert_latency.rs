//! Measures single-quad commit latency at a given dataset size (M1 gate P1: < 5 ms at 10 M).
//!
//! ```text
//! cargo run --release -p nrese-engine --example insert_latency -- 10000000 1000 [data-dir]
//! ```
//!
//! With a data directory the engine is durable (WAL fsync on every commit), and the run
//! also reports checkpoint time and recovery time.

use std::time::{Duration, Instant};

use nrese_engine::{Engine, EngineConfig};
use oxrdf::{GraphName, Literal, NamedNode, Quad};

fn quad(n: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/entity/{}", n / 10)),
        NamedNode::new_unchecked(format!("http://example.com/p/{}", n % 10)),
        Literal::new_simple_literal(format!("value {n}")),
        GraphName::DefaultGraph,
    )
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let size: u64 = args
        .next()
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let commits: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(1_000);

    let dir = args.next().map(std::path::PathBuf::from);
    let engine = match &dir {
        Some(dir) => Engine::open(dir, EngineConfig::default()).expect("open"),
        None => Engine::new(EngineConfig::default()).expect("engine"),
    };
    let started = Instant::now();
    let mut tx = engine.transaction();
    for n in 0..size {
        tx.insert(quad(n).as_ref());
    }
    tx.commit().expect("commit");
    let load = started.elapsed();
    println!(
        "load: {size} quads in {load:.2?} ({:.0} quads/s, via one transaction)",
        size as f64 / load.as_secs_f64()
    );

    let mut latencies = Vec::with_capacity(commits as usize);
    for n in size..size + commits {
        let started = Instant::now();
        let mut tx = engine.transaction();
        tx.insert(quad(n).as_ref());
        tx.commit().expect("commit");
        latencies.push(started.elapsed());
    }
    latencies.sort();
    println!(
        "1-quad commit over {commits} commits: p50 {:.1?}  p99 {:.1?}  max {:.1?}",
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.99),
        latencies[latencies.len() - 1],
    );
    engine.compact();
    if let Some(dir) = &dir {
        let started = Instant::now();
        engine.checkpoint().expect("checkpoint");
        println!("checkpoint: {:.2?}", started.elapsed());
        drop(engine);
        let started = Instant::now();
        let engine = Engine::open(dir, EngineConfig::default()).expect("reopen");
        println!(
            "recovery: {:.2?} ({} quads)",
            started.elapsed(),
            engine.stats().quads
        );
        return;
    }
    let stats = engine.stats();
    println!(
        "runs {}  index {:.0} MiB  dictionary {} terms / {:.0} MiB",
        stats.runs,
        stats.index_bytes as f64 / 1048576.0,
        stats.dictionary.terms,
        stats.dictionary.arena_bytes as f64 / 1048576.0,
    );
}
