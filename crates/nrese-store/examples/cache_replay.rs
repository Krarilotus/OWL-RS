//! Replays a repeating query log against a store with the result cache on or off, and
//! reports the hit rate, the time and the serving memory (merge checklist §2 item 7).
//!
//! ```text
//! cargo run --release -p nrese-store --example cache_replay -- \
//!     --store DIR [--load FILE]... --queries DIR [--queries DIR]... \
//!     [--cache BYTES] [--rounds 5] [--seed 1] [--label NAME]
//! ```
//!
//! **The log.** Every SELECT of the query directories, and for each two variants that
//! share it as a part, as a client paging through results and counting them sends them:
//! `SELECT (COUNT(*) AS ?n) WHERE { <query> }` and `SELECT * WHERE { <query> } LIMIT 20
//! OFFSET 20k` for k = 0, 1, 2. ASK, CONSTRUCT and DESCRIBE queries go in as they are.
//! Each round replays the whole log in an order shuffled by the seed, so every query
//! repeats once per round, between others.
//!
//! **The report.** Per round and in total: the queries answered whole from the cache
//! (no part computed), partly (some parts from it), or not at all; the replay's wall
//! time, the first round's apart (where the cache is cold); and the committed memory of
//! the process after the load and after the replay, with the bytes the cache counts.
//! Only the store API every build has is used, so the same file measures today's cache
//! and an earlier one.

use std::path::PathBuf;
use std::time::Instant;

use nrese_store::{BulkLoadRequest, GraphTarget, SparqlQueryRequest, StoreConfig, StoreService};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct Args {
    store: PathBuf,
    load: Vec<PathBuf>,
    queries: Vec<PathBuf>,
    cache: usize,
    rounds: usize,
    seed: u64,
    label: String,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        store: PathBuf::new(),
        load: Vec::new(),
        queries: Vec::new(),
        cache: 256 << 20,
        rounds: 5,
        seed: 1,
        label: "replay".to_owned(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--store" => args.store = PathBuf::from(value()?),
            "--load" => args.load.push(PathBuf::from(value()?)),
            "--queries" => args.queries.push(PathBuf::from(value()?)),
            "--cache" => args.cache = value()?.parse().map_err(|e| format!("--cache: {e}"))?,
            "--rounds" => args.rounds = value()?.parse().map_err(|e| format!("--rounds: {e}"))?,
            "--seed" => args.seed = value()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--label" => args.label = value()?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.store.as_os_str().is_empty() || (args.queries.is_empty() && args.load.is_empty()) {
        return Err("--store, and --load or --queries, are needed".to_owned());
    }
    Ok(args)
}

/// The query log: each query and its variants (module docs).
fn log(dirs: &[PathBuf]) -> Vec<(String, String)> {
    let mut log = Vec::new();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "rq"))
            .collect();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            log.push((name.clone(), text.clone()));
            // The prologue (PREFIX, BASE) stays in front; the query becomes a subquery.
            // Only a SELECT becomes a subquery: the first query form named is the query's.
            let upper = text.to_uppercase();
            let form = ["SELECT", "ASK", "CONSTRUCT", "DESCRIBE"]
                .iter()
                .filter_map(|form| Some((upper.find(form)?, *form)))
                .min();
            let Some((at, "SELECT")) = form else {
                continue;
            };
            let (prologue, body) = text.split_at(at);
            log.push((
                format!("{name}+count"),
                format!("{prologue}SELECT (COUNT(*) AS ?replay_n) WHERE {{ {body} }}"),
            ));
            for page in 0..3 {
                log.push((
                    format!("{name}+page{page}"),
                    format!(
                        "{prologue}SELECT * WHERE {{ {body} }} LIMIT 20 OFFSET {}",
                        page * 20
                    ),
                ));
            }
        }
    }
    log
}

/// SplitMix64: the seeded order of each round.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, (self.next() % (i as u64 + 1)) as usize);
        }
    }
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or("?".to_owned(), |b| {
        format!("{:.1}", b as f64 / f64::from(1 << 20))
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: mimalloc's `mi_collect` may be called at any time, from any thread.
    nrese_engine::memory::set_release(|force| unsafe { libmimalloc_sys::mi_collect(force) });
    let args =
        parse_args().map_err(|e| format!("{e}\nsee the usage at the top of cache_replay.rs"))?;
    let store = StoreService::new(StoreConfig {
        query_cache_bytes: args.cache,
        ..StoreConfig::on_disk(&args.store)
    })?;
    if !args.load.is_empty() {
        let started = Instant::now();
        let report = store.bulk_load(&BulkLoadRequest {
            files: args.load.clone(),
            replace: true,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })?;
        eprintln!(
            "loaded {} quads in {:.1} s",
            report.inserted,
            started.elapsed().as_secs_f64()
        );
        return Ok(());
    }
    let mut log = log(&args.queries);
    // Queries this build can't run are left out of every round (and reported).
    log.retain(
        |(name, text)| match store.execute_query(&SparqlQueryRequest::all(text.as_str())) {
            Ok(_) => true,
            Err(error) => {
                eprintln!("left out {name}: {error}");
                false
            }
        },
    );
    // The checks above read every query's data once (the store's files are mapped and
    // in the page cache for every build alike) and filled the cache: open again, empty.
    drop(store);
    let store = StoreService::new(StoreConfig {
        query_cache_bytes: args.cache,
        ..StoreConfig::on_disk(&args.store)
    })?;
    let after_open = nrese_exec::memory::process_bytes();
    let mut rng = Rng(args.seed);
    let (mut whole, mut partly, mut none) = (0usize, 0usize, 0usize);
    let mut first_round_s = 0.0;
    let mut total_s = 0.0;
    let mut per_round = Vec::new();
    // Each query's times after the first round, and its result's bytes.
    let mut later: Vec<Vec<f64>> = vec![Vec::new(); log.len()];
    let mut sizes = vec![0usize; log.len()];
    for round in 0..args.rounds {
        let mut order: Vec<usize> = (0..log.len()).collect();
        rng.shuffle(&mut order);
        let started = Instant::now();
        let (mut round_whole, mut round_partly) = (0, 0);
        for &i in &order {
            let before = store.query_cache_stats();
            let query_started = Instant::now();
            let result = store.execute_query(&SparqlQueryRequest::all(log[i].1.as_str()))?;
            if round > 0 {
                later[i].push(query_started.elapsed().as_secs_f64() * 1e3);
            }
            sizes[i] = std::hint::black_box(result.payload.len());
            let after = store.query_cache_stats();
            let (hits, misses) = (after.hits - before.hits, after.misses - before.misses);
            match (hits > 0, misses > 0) {
                (true, false) => round_whole += 1,
                (true, true) => round_partly += 1,
                _ => none += 1,
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        whole += round_whole;
        partly += round_partly;
        total_s += seconds;
        if round == 0 {
            first_round_s = seconds;
        }
        per_round.push(format!(
            "{seconds:.3} s ({round_whole} whole, {round_partly} partly)"
        ));
    }
    let after_replay = nrese_exec::memory::process_bytes();
    let stats = store.query_cache_stats();
    let queries = log.len() * args.rounds;
    println!(
        "{} | {} queries x {} rounds | whole {whole} ({:.1} %), partly {partly}, none {none} | \
         time {total_s:.3} s, first round {first_round_s:.3} s, later rounds {:.3} s | \
         committed after open {} MiB, after replay {} MiB, cache {} MiB in {} entries, \
         hits {} misses {}",
        args.label,
        log.len(),
        args.rounds,
        100.0 * whole as f64 / queries as f64,
        total_s - first_round_s,
        mib(after_open),
        mib(after_replay),
        mib(Some(stats.bytes as u64)),
        stats.entries,
        stats.hits,
        stats.misses,
    );
    eprintln!("rounds: {}", per_round.join("; "));
    // The queries taking the most time after the first round.
    let mut medians: Vec<(f64, usize)> = later
        .iter_mut()
        .enumerate()
        .filter(|(_, times)| !times.is_empty())
        .map(|(i, times)| {
            times.sort_by(f64::total_cmp);
            (times[times.len() / 2], i)
        })
        .collect();
    medians.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (median, i) in medians.iter().take(8) {
        eprintln!(
            "  {:<36} {median:8.3} ms  {:>10} bytes",
            log[*i].0, sizes[*i]
        );
    }
    Ok(())
}
