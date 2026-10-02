//! Restart benchmark (Pf2): time to reopen an on-disk store.
//!
//! ```text
//! cargo run --release -p nrese-store --example restart -- <data-dir> [input.nt...]
//! ```
//!
//! With input files and an empty `data-dir`, bulk-loads them first (which writes a
//! checkpoint). Then reopens the store three times and prints each time. Set
//! `NRESE_RECOVERY_TIMING=1` for the phases of recovery.

use std::path::PathBuf;
use std::time::Instant;

use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .ok_or("usage: restart <data-dir> [input.nt...]")?,
    );
    let files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if !files.is_empty() && !dir.join("LOCK").exists() {
        let started = Instant::now();
        let store = StoreService::new(StoreConfig::on_disk(&dir))?;
        let load = store.bulk_load(&BulkLoadRequest {
            files,
            replace: false,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })?;
        println!(
            "loaded {} quads in {:.2} s",
            load.inserted,
            started.elapsed().as_secs_f64()
        );
    }
    let size: u64 = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum();
    println!(
        "data dir: {:.1} MiB in files",
        size as f64 / (1 << 20) as f64
    );
    for _ in 0..3 {
        let started = Instant::now();
        let store = StoreService::new(StoreConfig::on_disk(&dir))?;
        let opened = started.elapsed();
        let quads = store.stats()?.quad_count;
        println!("reopen: {:.3} s ({quads} quads)", opened.as_secs_f64());
        drop(store);
    }
    Ok(())
}
