//! `--durable NAME`: the durable engine's own costs, timed on the engine directly. Every run
//! gets a fresh data directory under `--store` (removed after it), so every side of a
//! comparison writes to the same file system:
//!
//! - `commit-every-N`, `commit-buffered-N`: N one-statement commits under
//!   `SyncPolicy::EveryCommit` or `OsBuffered`; each commit's latency is a sample;
//! - `checkpoint-N`: a store of N statements (bulk-loaded), one commit, then a checkpoint:
//!   its image written, renamed and synced;
//! - `open-S-K`: opening a store whose log is S segments of one K-statement commit each:
//!   recovery's replay and its check of each segment's tail;
//! - `load-N`, `load-spill-N`: a durable bulk load of N statements, its checkpoint
//!   included; spilled with a budget of a quarter of its quads.
//!
//! Each run's data is checked before its time counts: outside the measured interval the
//! store is reopened, and its revision and exact contents (a digest of every decoded
//! statement, compared with the generator's) must be what the workload wrote. A failed
//! check fails the run, which then reports no time. The report's `verified` (true only then),
//! `statements` and `revision` are what a case's `check` pins (benches/cases/README.md).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::hash::{DefaultHasher, Hash, Hasher};

use nrese_engine::{BulkMode, DurabilityConfig, Engine, EngineConfig, QuadPattern, SyncPolicy};
use nrese_rdf::{GraphName, NamedNode, Quad};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn durable(name: &str, root: &Path, runs: usize) -> Result<String> {
    let (workload, sizes) = name
        .split_once(|c: char| c.is_ascii_digit())
        .map(|(head, _)| (head.trim_end_matches('-'), &name[head.len()..]))
        .ok_or(format!("unknown durable workload {name}"))?;
    let sizes: Vec<u64> = sizes
        .split('-')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let size = |i: usize| {
        sizes
            .get(i)
            .copied()
            .ok_or(format!("{name}: a size missing"))
    };
    std::fs::create_dir_all(root)?;
    // Per run: its samples.
    let mut per_run: Vec<Vec<Duration>> = Vec::new();
    // What the runs' checks found: (statements, revision), the same for every run.
    let mut checked = None;
    for run in 0..runs.max(1) {
        let dir = fresh(root, run)?;
        let (samples, found) = match workload {
            "commit-every" => commits(&dir, SyncPolicy::EveryCommit, size(0)?)?,
            "commit-buffered" => commits(&dir, SyncPolicy::OsBuffered, size(0)?)?,
            "checkpoint" => checkpoint(&dir, size(0)?)?,
            "open" => open(&dir, size(0)?, size(1)?)?,
            "load" => load(&dir, size(0)?, false)?,
            "load-spill" => load(&dir, size(0)?, true)?,
            other => return Err(format!("unknown durable workload {other}").into()),
        };
        if checked.is_some_and(|before| before != found) {
            return Err(format!("{name}: runs found different contents").into());
        }
        checked = Some(found);
        std::fs::remove_dir_all(&dir)?;
        per_run.push(samples);
    }
    let ms = |t: &Duration| t.as_secs_f64() * 1e3;
    let mut all: Vec<Duration> = per_run.iter().flatten().copied().collect();
    all.sort();
    let at = |q: f64| ms(&all[((all.len() - 1) as f64 * q).round() as usize]);
    let run_p50: Vec<String> = per_run
        .iter_mut()
        .map(|samples| {
            samples.sort();
            format!("{:.4}", ms(&samples[samples.len() / 2]))
        })
        .collect();
    let samples: Vec<String> = match all.len() > 1000 {
        // Commit streams: the per-run medians carry the spread.
        true => Vec::new(),
        false => all.iter().map(|t| format!("{:.4}", ms(t))).collect(),
    };
    eprintln!(
        "{name}: p50 {:.3} ms, p90 {:.3} ms, max {:.3} ms over {} samples, run p50s [{}]",
        at(0.5),
        at(0.9),
        at(1.0),
        all.len(),
        run_p50.join(", ")
    );
    let (statements, revision) = checked.expect("at least one run");
    Ok(format!(
        "{{\"name\": {name:?}, \"verified\": true, \"statements\": {statements}, \
         \"revision\": {revision}, \"runs\": {}, \"count\": {}, \"p50_ms\": {:.4}, \
         \"p90_ms\": {:.4}, \"max_ms\": {:.4}, \"run_p50_ms\": [{}], \"samples_ms\": [{}]}}",
        per_run.len(),
        all.len(),
        at(0.5),
        at(0.9),
        at(1.0),
        run_p50.join(", "),
        samples.join(", ")
    ))
}

/// A new, empty data directory for run `run`.
fn fresh(root: &Path, run: usize) -> Result<PathBuf> {
    let dir = root.join(format!("durable-{}-{run}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn config(sync: SyncPolicy, durability: DurabilityConfig) -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig { sync, ..durability },
        ..EngineConfig::default()
    }
}

fn quad(i: u64) -> Quad {
    let iri = |s: String| NamedNode::new_unchecked(s);
    Quad::new(
        iri(format!("http://example.com/s{}", i / 4)),
        iri(format!("http://example.com/p{}", i % 13)),
        iri(format!("http://example.com/o{i}")),
        GraphName::DefaultGraph,
    )
}

/// The contents of a set of statements, whatever their order: their number and the sum of
/// their canonical texts' hashes.
fn digest(quads: impl Iterator<Item = Quad>) -> (u64, u64) {
    quads.fold((0, 0), |(count, sum), quad| {
        let mut hasher = DefaultHasher::new();
        quad.to_string().hash(&mut hasher);
        (count + 1, sum.wrapping_add(hasher.finish()))
    })
}

/// Checks that `engine` holds exactly statements `0..quads` at `revision`; returns them.
pub fn check(engine: &Engine, quads: u64, revision: u64) -> Result<(u64, u64)> {
    let snapshot = engine.snapshot();
    let found = digest(snapshot.quads_for_pattern(&QuadPattern::all()).map(|quad| {
        snapshot
            .decode_quad(quad)
            .expect("a stored statement decodes")
    }));
    let expected = digest((0..quads).map(quad));
    match (found, snapshot.revision()) == (expected, revision) {
        true => Ok((quads, revision)),
        false => Err(format!(
            "expected statements 0..{quads} at revision {revision}, found {} statements              (digest {:#x}, expected {:#x}) at revision {}",
            found.0,
            found.1,
            expected.1,
            snapshot.revision()
        )
        .into()),
    }
}

/// Reopens the store in `dir` (outside the measured interval) and [`check`]s it.
fn reopened(dir: &Path, config: EngineConfig, quads: u64, revision: u64) -> Result<(u64, u64)> {
    check(&Engine::open(dir, config)?, quads, revision)
}

/// A workload's samples and what its check verified (statements, revision).
type Measured = Result<(Vec<Duration>, (u64, u64))>;

fn commits(dir: &Path, sync: SyncPolicy, n: u64) -> Measured {
    let config = config(sync, DurabilityConfig::default());
    let engine = Engine::open(dir, config.clone())?;
    let mut times = Vec::with_capacity(n as usize);
    for i in 0..n {
        let at = Instant::now();
        let mut tx = engine.transaction();
        tx.insert(quad(i).as_ref());
        tx.commit()?;
        times.push(at.elapsed());
    }
    drop(engine);
    let found = reopened(dir, config, n, n)?;
    Ok((times, found))
}

fn bulk(engine: &Engine, n: u64) -> Result<()> {
    let load = engine.bulk_load(BulkMode::Replace);
    let mut batch = Vec::with_capacity(10_000);
    for i in 0..n {
        batch.push(quad(i));
        if batch.len() == batch.capacity() {
            load.add(&batch);
            batch.clear();
        }
    }
    load.add(&batch);
    load.finish()?;
    Ok(())
}

fn checkpoint(dir: &Path, n: u64) -> Measured {
    let config = config(SyncPolicy::EveryCommit, DurabilityConfig::default());
    let engine = Engine::open(dir, config.clone())?;
    bulk(&engine, n)?;
    let mut tx = engine.transaction();
    tx.insert(quad(n).as_ref());
    tx.commit()?;
    let at = Instant::now();
    engine.checkpoint()?;
    let time = at.elapsed();
    drop(engine);
    let found = reopened(dir, config, n + 1, 2)?;
    Ok((vec![time], found))
}

fn open(dir: &Path, segments: u64, per_commit: u64) -> Measured {
    // A segment per commit: each append starts a new one.
    let durability = DurabilityConfig {
        wal_segment_bytes: 1,
        checkpoint_after_wal_bytes: u64::MAX,
        ..DurabilityConfig::default()
    };
    let config = config(SyncPolicy::EveryCommit, durability);
    {
        let engine = Engine::open(dir, config.clone())?;
        for commit in 0..segments {
            let mut tx = engine.transaction();
            for i in 0..per_commit {
                tx.insert(quad(commit * per_commit + i).as_ref());
            }
            tx.commit()?;
        }
    }
    let at = Instant::now();
    let engine = Engine::open(dir, config)?;
    let time = at.elapsed();
    let found = check(&engine, segments * per_commit, segments)?;
    Ok((vec![time], found))
}

fn load(dir: &Path, n: u64, spill: bool) -> Measured {
    let durability = DurabilityConfig {
        bulk_load_memory: spill.then_some(n * 32 / 4),
        ..DurabilityConfig::default()
    };
    let config = config(SyncPolicy::EveryCommit, durability);
    let engine = Engine::open(dir, config.clone())?;
    let at = Instant::now();
    bulk(&engine, n)?;
    let time = at.elapsed();
    drop(engine);
    let found = reopened(dir, config, n, 1)?;
    Ok((vec![time], found))
}
