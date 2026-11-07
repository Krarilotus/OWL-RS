//! What a search under `GRAPH ?g` holds (native/search.rs, evaluated per named graph):
//! charged to the query's envelope before it is allocated (brief v3 §3; review r1 R1-C1),
//! so a query whose graphs or rows outgrow the envelope is refused before the heap grows
//! past it, and every charge is released after. Two dimensions grow: the graphs a
//! `bds:minRank` counts ranks in (no row passes), and the rows a literal in many graphs
//! fans out to. Measured with a counting allocator, so this file is a test binary of its
//! own; its tests run one at a time.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::{Envelope, SharedBudget, heap};
use nrese_rdf::{Literal, NamedNode, Quad};
use nrese_sparql::{QueryOptions, QueryResults, Resources, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The tests of this binary one at a time: the allocator counts the whole process.
static HEAP: std::sync::Mutex<()> = std::sync::Mutex::new(());

const PREFIXES: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> ";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

/// `literals` labels matching `alpha`, each in `graphs` named graphs (`spread`: literal i in
/// graph i alone, one graph each).
fn engine(literals: usize, graphs: usize, spread: bool) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let label = ex("label");
    for i in 0..literals {
        let text = Literal::new_simple_literal(format!("alpha {i}"));
        let range = match spread {
            true => i..i + 1,
            false => 0..graphs,
        };
        for g in range {
            let subject = ex(&format!("s{i}-{g}"));
            let graph = ex(&format!("g{g}"));
            tx.insert(Quad::new(subject, label.clone(), text.clone(), graph).as_ref());
        }
    }
    tx.commit().unwrap();
    engine
}

/// How a query ran: its rows or its error, the most heap it held beyond what was live
/// before, and what its envelope's shared budget still holds once it is done.
struct Run {
    outcome: Result<usize, String>,
    peak: usize,
    held_after: usize,
}

fn run(engine: &Engine, query: &str, limit: Option<usize>) -> Run {
    let shared = SharedBudget::new(usize::MAX);
    let options = QueryOptions {
        resources: Resources {
            memory: Envelope::new(limit, Some(Arc::clone(&shared))),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let text = format!("{PREFIXES}{query}");
    let parsed = SparqlParser::new().parse_query(&text).unwrap();
    let snapshot = engine.snapshot();
    let before = heap::live();
    heap::start("query");
    let outcome = match evaluate_query(&snapshot, &parsed, &options) {
        Ok(QueryResults::Solutions(solutions)) => solutions
            .map(|solution| solution.map(|_| ()))
            .collect::<Result<Vec<()>, _>>()
            .map(|rows| rows.len())
            .map_err(|error| format!("{error:?}")),
        Ok(_) => Err("not solutions".to_owned()),
        Err(error) => Err(format!("{error:?}")),
    };
    let phases = heap::finish();
    let peak = phases.first().map_or(0, |p| p.peak).saturating_sub(before);
    drop(options);
    Run {
        outcome,
        peak,
        held_after: shared.used(),
    }
}

fn refused(outcome: &Result<usize, String>) -> bool {
    // The executor's typed refusal (`QueryEvaluationError::MemoryLimit`), by its name.
    outcome.as_ref().is_err_and(|e| e.contains("MemoryLimit"))
}

/// Graphs a `bds:minRank 2` counts ranks in, each with one match (rank 1), so no row passes
/// and the answer is empty: the counts are charged as they grow. Rows: few graphs within the
/// envelope (guard, the empty answer); many graphs past it (refused, nothing held after).
#[test]
fn ranks_in_many_graphs_are_refused_before_they_outgrow_the_envelope() {
    let _serial = HEAP.lock().unwrap_or_else(|p| p.into_inner());
    let query = "SELECT ?o ?g WHERE { GRAPH ?g { ?o bds:search \"alpha\" ; bds:minRank 2 } }";
    let limit = 64 << 10;
    let mut failed = Vec::new();
    for (name, graphs, want_refused) in [("few graphs", 50, false), ("many graphs", 20_000, true)] {
        let engine = engine(graphs, graphs, true);
        let run = run(&engine, query, Some(limit));
        eprintln!(
            "{name}: {graphs} graphs, limit {limit}: {:?}, peak {} bytes, held after {}",
            run.outcome, run.peak, run.held_after
        );
        let ok = match want_refused {
            true => refused(&run.outcome),
            false => run.outcome == Ok(0),
        };
        if !ok || run.held_after != 0 {
            failed.push(format!(
                "{name}: got {:?}, held after {}; want {}",
                run.outcome,
                run.held_after,
                if want_refused {
                    "a memory refusal"
                } else {
                    "no rows"
                }
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// A literal in many graphs fans out to a row per graph: 500 literals in 200 graphs are
/// 100,000 rows of two ids (1.6 MB). Rows: within an unlimited envelope, every row (guard);
/// within 128 KiB, refused with the heap's peak below a quarter of the rows' bytes (they were
/// not built first), nothing held after.
#[test]
fn fan_out_is_refused_before_the_rows_outgrow_the_envelope() {
    let _serial = HEAP.lock().unwrap_or_else(|p| p.into_inner());
    let (literals, graphs) = (500, 200);
    let engine = engine(literals, graphs, false);
    let query = "SELECT ?o ?g WHERE { GRAPH ?g { ?o bds:search \"alpha\" } }";
    let rows = literals * graphs;
    let row_bytes = rows * 2 * 8;
    let mut failed = Vec::new();
    let all = run(&engine, query, None);
    eprintln!("unlimited: {:?}, peak {} bytes", all.outcome, all.peak);
    if all.outcome != Ok(rows) {
        failed.push(format!(
            "unlimited: got {:?}, want {rows} rows",
            all.outcome
        ));
    }
    let limit = 128 << 10;
    let small = run(&engine, query, Some(limit));
    eprintln!(
        "limit {limit}: {:?}, peak {} bytes (rows {row_bytes}), held after {}",
        small.outcome, small.peak, small.held_after
    );
    if !refused(&small.outcome) || small.peak >= row_bytes / 4 || small.held_after != 0 {
        failed.push(format!(
            "limit {limit}: got {:?}, peak {} bytes, held after {}; want a memory refusal \
             with a peak below {} bytes and nothing held",
            small.outcome,
            small.peak,
            small.held_after,
            row_bytes / 4
        ));
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
