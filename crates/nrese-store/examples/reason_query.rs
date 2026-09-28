//! End-to-end reasoning benchmark for reasoner v2 (R4): bulk load, materialise into the
//! inferred stack, then answer queries with the native SPARQL engine under the default
//! (materialised) read model. RT1 and RT3 in `docs/design/reasoning-benchmark.md`.
//!
//! ```text
//! cargo run --release -p nrese-store --example reason_query -- \
//!     [--ruleset owl2-rl|rdfs] [--queries dir] input.nt...
//! ```
//!
//! Prints load, reasoning and per-query times, and one `qNN<TAB>count` line per query
//! (the scorecard's answer format). Counting wraps each query as
//! `SELECT (COUNT(*) AS ?n) WHERE { … }`, so result serialisation isn't timed.

use std::path::PathBuf;
use std::time::Instant;

use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};

/// `SELECT (COUNT(*) AS ?n) WHERE { <query> }` with the query's prologue kept in front.
fn counting(query: &str) -> String {
    let mut prologue = String::new();
    let mut body = String::new();
    for line in query.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.to_ascii_uppercase().starts_with("PREFIX")
            || trimmed.to_ascii_uppercase().starts_with("BASE")
        {
            prologue.push_str(line);
            prologue.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    format!("{prologue}SELECT (COUNT(*) AS ?n) WHERE {{ {body} }}")
}

/// The first integer value in a SPARQL JSON result.
fn first_integer(json: &str) -> Option<u64> {
    let start = json.find("\"value\"")?;
    let rest = &json[start + 7..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut ruleset, mut queries, mut files) = (Ruleset::Owl2Rl, None, Vec::new());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ruleset" => {
                ruleset = match args.next().as_deref() {
                    Some("rdfs") => Ruleset::Rdfs,
                    Some("owl2-rl") => Ruleset::Owl2Rl,
                    other => return Err(format!("unknown ruleset {other:?}").into()),
                }
            }
            "--queries" => queries = args.next().map(PathBuf::from),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if files.is_empty() {
        return Err(
            "usage: reason_query [--ruleset owl2-rl|rdfs] [--queries dir] input.nt...".into(),
        );
    }

    let store = StoreService::new(StoreConfig::in_memory())?;
    let started = Instant::now();
    let load = store.bulk_load(&BulkLoadRequest {
        files,
        replace: false,
        graph: GraphTarget::DefaultGraph,
    })?;
    let load_time = started.elapsed();
    let report = store.rematerialise(ruleset)?;
    println!(
        "{}: asserted {} | load {:.2} s | closure {:.2} s in {} rounds | derived {} | violations {}",
        ruleset.name(),
        load.inserted,
        load_time.as_secs_f64(),
        report.elapsed.as_secs_f64(),
        report.rounds,
        report.inferred,
        report.violations
    );

    let Some(dir) = queries else {
        return Ok(());
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "rq"));
    paths.sort();
    let mut total = 0.0;
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_owned();
        let query = counting(&std::fs::read_to_string(&path)?);
        // Best of three, after one warm-up run.
        let mut best = f64::MAX;
        let mut count = None;
        for _ in 0..4 {
            let started = Instant::now();
            let result = store.execute_query_str(&query)?;
            let elapsed = started.elapsed().as_secs_f64();
            count = first_integer(&String::from_utf8_lossy(&result.payload));
            best = best.min(elapsed);
        }
        total += best;
        eprintln!("{name}: {:.2} ms", best * 1000.0);
        println!(
            "{name}\t{}",
            count.map_or_else(|| "error".to_owned(), |c| c.to_string())
        );
    }
    eprintln!(
        "queries: {:.2} ms in total (best of 4 each)",
        total * 1000.0
    );
    Ok(())
}
