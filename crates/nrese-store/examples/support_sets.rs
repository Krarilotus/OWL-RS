//! Inferred statements under graph access, timed (`inferred = "supported"`): the data's
//! statements spread over N named graphs by subject, the schema in the default graph; a
//! reader of half of the graphs. Prints the first restricted read (the sets computed,
//! the view built), then commits through the mutation pipeline, each followed by a
//! restricted read (the sets updated, the view built).
//!
//! ```text
//! cargo run --release -p nrese-store --example support_sets -- \
//!     [--graphs N] [--ruleset owl2-rl|rdfs] schema.nt data.nt
//! ```

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::Instant;

use nrese_engine::ReadModel;
use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::GraphAccess;
use nrese_store::{
    BulkLoadRequest, GraphTarget, MutationCommand, MutationPipeline, MutationTicket, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

const GRAPH: &str = "urn:nrese:example:graph:";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut graphs, mut ruleset, mut inputs) = (8u64, "owl2-rl".to_owned(), Vec::new());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--graphs" => graphs = args.next().and_then(|n| n.parse().ok()).unwrap_or(8),
            "--ruleset" => ruleset = args.next().unwrap_or(ruleset),
            _ => inputs.push(arg),
        }
    }
    let [schema, data] = <[String; 2]>::try_from(inputs).map_err(|_| "schema.nt data.nt")?;
    let (rules, mode) = match ruleset.as_str() {
        "rdfs" => (Ruleset::Rdfs, ReasoningMode::Rdfs),
        _ => (Ruleset::Owl2Rl, ReasoningMode::Owl2Rl),
    };
    // The data as N-Quads, each statement in the graph of its subject's hash.
    let scratch = tempfile::tempdir_in(".")?;
    let quads = scratch.path().join("data.nq");
    {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&quads)?);
        for line in std::io::BufReader::new(std::fs::File::open(&data)?).lines() {
            let line = line?;
            let Some(body) = line.trim_end().strip_suffix('.') else {
                continue;
            };
            let subject = body.split(' ').next().unwrap_or_default();
            let hash = subject
                .bytes()
                .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
            writeln!(out, "{body} <{GRAPH}{}> .", hash % graphs)?;
        }
    }
    let store = Arc::new(StoreService::new(StoreConfig::in_memory())?);
    let started = Instant::now();
    store.bulk_load(&BulkLoadRequest {
        files: vec![schema.into(), quads],
        replace: false,
        graph: GraphTarget::DefaultGraph,
        skip_errors: false,
    })?;
    let report = store.rematerialise(rules)?;
    println!(
        "load and closure {:.2} s: {} inferred",
        started.elapsed().as_secs_f64(),
        report.inferred
    );
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    let access = Arc::new(GraphAccess {
        graphs: (0..graphs / 2).map(|g| format!("{GRAPH}{g}")).collect(),
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    });
    let read = |what: &str| {
        let started = Instant::now();
        let view = store.read_snapshot(Some(&access));
        let elapsed = started.elapsed();
        println!(
            "{what}: {:.1} ms, {} of {} inferred visible",
            elapsed.as_secs_f64() * 1e3,
            view.len_in(ReadModel::Inferred),
            store.read_snapshot(None).len_in(ReadModel::Inferred),
        );
    };
    read("first restricted read");
    read("again (kept)");
    let commit = |what: &str, update: String| -> Result<(), Box<dyn std::error::Error>> {
        let started = Instant::now();
        pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &Requester::all(),
            &MutationTicket::new(),
        )?;
        println!(
            "commit {what}: {:.1} ms",
            started.elapsed().as_secs_f64() * 1e3
        );
        read("  restricted read");
        Ok(())
    };
    let ub = "PREFIX ub: <http://www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#>";
    commit(
        "1 student",
        format!("{ub} INSERT DATA {{ GRAPH <{GRAPH}0> {{ <urn:x:s0> a ub:GraduateStudent }} }}"),
    )?;
    let many: String = (0..1000)
        .map(|i| {
            format!(
                "GRAPH <{GRAPH}{}> {{ <urn:x:s{i}> a ub:GraduateStudent ; ub:takesCourse <urn:x:c{}> }}\n",
                i % graphs,
                i % 7
            )
        })
        .collect();
    commit("1000 students", format!("{ub} INSERT DATA {{ {many} }}"))?;
    commit("delete them", format!("{ub} DELETE DATA {{ {many} }}"))?;
    let statistics = store.support_statistics();
    println!("{statistics:?}");
    Ok(())
}
