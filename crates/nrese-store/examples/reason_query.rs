//! End-to-end reasoning benchmark for reasoner v2 (R4): bulk load, materialise into the
//! inferred stack, then answer queries with the native SPARQL engine under the default
//! (materialised) read model. RT1 and RT3 in `docs/design/reasoning-benchmark.md`.
//!
//! ```text
//! cargo run --release -p nrese-store --example reason_query -- \
//!     [--ruleset owl2-rl|rdfs] [--queries dir] [--explain] input.nt...
//! ```
//!
//! `--explain` prints each query's plan (operators with estimated and actual rows, and
//! times) after its timing line.
//!
//! Prints load, reasoning and per-query times, and one `qNN<TAB>count` line per query
//! (the scorecard's answer format). Counting wraps each query as
//! `SELECT (COUNT(*) AS ?n) WHERE { … }`, so result serialisation isn't timed.

use std::path::PathBuf;
use std::time::Instant;

use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphTarget, MutationCommand, MutationPipeline,
    MutationTicket, PreparedQuery, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig,
    StoreService,
};

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
    let mut commits = 0usize;
    let mut explain = false;
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
            "--commits" => commits = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            "--explain" => explain = true,
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if files.is_empty() {
        return Err(
            "usage: reason_query [--ruleset owl2-rl|rdfs] [--queries dir] input.nt...".into(),
        );
    }

    let store = std::sync::Arc::new(StoreService::new(StoreConfig::in_memory())?);
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

    if commits > 0 {
        commit_latency(&store, ruleset, commits)?;
    }
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
        if explain {
            let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(query.clone()))?;
            let plan = store.explain_query(&prepared, &CancellationToken::new())?;
            eprintln!("  executor {} | {} rows", plan.executor, plan.rows);
            for step in &plan.steps {
                let estimate = step
                    .estimated_rows
                    .map_or_else(String::new, |e| format!(" est {e}"));
                eprintln!(
                    "  {}{} {} |{} rows {} | {:.2} ms",
                    "  ".repeat(step.depth),
                    step.operator,
                    step.detail,
                    estimate,
                    step.rows,
                    step.micros as f64 / 1000.0
                );
            }
        }
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

/// Commit-path latency (RT2): deletes and re-inserts `n` asserted `rdf:type` statements one
/// at a time through the mutation pipeline, which maintains the inferred stack with the
/// delta executor. Prints p50/p99 per operation.
fn commit_latency(
    store: &std::sync::Arc<StoreService>,
    ruleset: Ruleset,
    n: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = match ruleset {
        Ruleset::Rdfs => ReasoningMode::Rdfs,
        Ruleset::Owl2Rl => ReasoningMode::Owl2Rl,
    };
    let pipeline = MutationPipeline::new(
        std::sync::Arc::clone(store),
        std::sync::Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    // Asserted ABox type statements (IRIs, classes outside the W3C vocabularies), spread
    // over the dataset.
    let mut request = nrese_store::SparqlQueryRequest::new(format!(
        "SELECT ?s ?c WHERE {{ ?s a ?c FILTER(isIRI(?s) && !STRSTARTS(STR(?c), \"http://www.w3.org/\")) }} LIMIT {}",
        n * 50
    ));
    request.read_model = Some(nrese_store::ReadModel::Asserted);
    request.solutions_format = nrese_store::SolutionsResultFormat::Tsv;
    let tsv = String::from_utf8(store.execute_query(&request)?.payload)?;
    let pairs: Vec<(String, String)> = tsv
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (s, c) = line.split_once('\t')?;
            Some((s.to_owned(), c.to_owned()))
        })
        .step_by(50)
        .take(n)
        .collect();
    let (mut deletes, mut inserts) = (Vec::new(), Vec::new());
    // Per pair: delete an asserted statement (timed), put it back, insert a statement
    // about a new entity (timed: a fact new to the state), remove it again.
    for (i, (s, c)) in pairs.iter().enumerate() {
        let existing = format!("{s} a {c}");
        let fresh = format!("<urn:nrese:bench:{i}> a {c}");
        for (update, triple, timed) in [
            ("DELETE DATA", &existing, Some(&mut deletes)),
            ("INSERT DATA", &existing, None),
            ("INSERT DATA", &fresh, Some(&mut inserts)),
            ("DELETE DATA", &fresh, None),
        ] {
            let command = MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{update} {{ {triple} }}"
            )));
            let started = Instant::now();
            pipeline.apply(command, &MutationTicket::new())?;
            if let Some(times) = timed {
                times.push(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    let stats = |times: &mut Vec<f64>| {
        times.sort_by(f64::total_cmp);
        let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
        (at(0.5), at(0.99))
    };
    let (d50, d99) = stats(&mut deletes);
    let (i50, i99) = stats(&mut inserts);
    println!(
        "commits: {} deletes p50 {d50:.2} ms p99 {d99:.2} ms | inserts p50 {i50:.2} ms p99 {i99:.2} ms",
        pairs.len()
    );
    Ok(())
}
