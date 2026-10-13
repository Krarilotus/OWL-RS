//! What a query's evaluation holds against what it charges its memory budget, counted by
//! the allocator (docs/design/performance.md §0): Sparqloscope's shapes on DBLP-like data
//! at a small scale. A test binary of its own for its counting allocator.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::{SharedBudget, heap};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{QueryOptions, QueryResults, Resources, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

const EX: &str = "http://example.org/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// `n` publications, each with four signatures (each pointing back), three creators, a
/// type among five classes besides `Pub`, a label and a year; the classes are subclasses
/// of `Pub`.
fn engine(n: u32) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let mut add = |s: NamedNode, p: &str, o: Term| {
        tx.insert(Quad::new(s, ex(p), o, GraphName::DefaultGraph).as_ref());
    };
    for k in 0..5 {
        add(ex(&format!("C{k}")), "subClassOf", ex("Pub").into());
    }
    for i in 0..n {
        let s = ex(&format!("s{i}"));
        for j in 0..4 {
            let sig = ex(&format!("sig{i}-{j}"));
            add(s.clone(), "hasSignature", sig.clone().into());
            add(sig, "signaturePublication", s.clone().into());
        }
        for j in 0..3 {
            add(
                s.clone(),
                "createdBy",
                ex(&format!("c{}", (i * 3 + j) % (n / 2))).into(),
            );
        }
        add(s.clone(), "type", ex(&format!("C{}", i % 5)).into());
        add(s.clone(), "type", ex("Pub").into());
        add(
            s.clone(),
            "label",
            Literal::new_simple_literal(format!("label {i}")).into(),
        );
        add(
            s,
            "year",
            Literal::new_typed_literal(
                format!("{}", 1990 + i % 30),
                nrese_rdf::vocab::xsd::INTEGER,
            )
            .into(),
        );
    }
    tx.commit().unwrap();
    engine
}

fn queries() -> Vec<(&'static str, String)> {
    let p = |local: &str| format!("<{EX}{local}>");
    let (sig, by, ty, sub, pubof, label) = (
        p("hasSignature"),
        p("createdBy"),
        p("type"),
        p("subClassOf"),
        p("signaturePublication"),
        p("label"),
    );
    vec![
        (
            "004 count",
            format!("SELECT (COUNT(*) AS ?n) WHERE {{ ?s {sig} ?o1 . ?s {by} ?o2 }}"),
        ),
        (
            "004 rows",
            format!("SELECT ?s ?o1 ?o2 WHERE {{ ?s {sig} ?o1 . ?s {by} ?o2 }}"),
        ),
        (
            "019 count",
            format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ ?a {pubof} ?b . ?b {ty} ?c OPTIONAL {{ ?c {sub} ?d }} }}"
            ),
        ),
        (
            "019 rows",
            format!("SELECT * WHERE {{ ?a {pubof} ?b . ?b {ty} ?c OPTIONAL {{ ?c {sub} ?d }} }}"),
        ),
        (
            "013 rows",
            format!("SELECT * WHERE {{ ?s {ty} ?o1 OPTIONAL {{ ?s {sig} ?o2 }} }}"),
        ),
        (
            "051 group",
            format!(
                "SELECT ?c (COUNT(?s) AS ?n) (MIN(?s) AS ?min) (MAX(?s) AS ?max) (SAMPLE(?s) AS ?x) WHERE {{ ?s {ty} ?c . ?s {sig} ?o }} GROUP BY ?c"
            ),
        ),
        (
            "060 concat",
            format!(
                "SELECT (SUM(STRLEN(?cat)) AS ?sum) {{ {{ SELECT (GROUP_CONCAT(?o) AS ?cat) {{ ?s {label} ?o }} GROUP BY ?s }} }}"
            ),
        ),
        (
            "063 distinct count",
            format!("SELECT (COUNT(DISTINCT ?o2) AS ?n) WHERE {{ ?s {sig} ?o1 . ?s {by} ?o2 }}"),
        ),
        (
            "distinct rows",
            format!("SELECT DISTINCT ?o2 WHERE {{ ?s {sig} ?o1 . ?s {by} ?o2 }}"),
        ),
        (
            "order limit",
            format!("SELECT ?s ?o WHERE {{ ?s {sig} ?o }} ORDER BY DESC(?o) LIMIT 10"),
        ),
        (
            "095 filter",
            format!("SELECT (COUNT(?s) AS ?n) {{ ?s {ty} ?o FILTER(?s != ?o) }}"),
        ),
        (
            "029 minus",
            format!("SELECT * WHERE {{ ?a {pubof} ?b . ?b {ty} ?c MINUS {{ ?c {sub} ?d }} }}"),
        ),
    ]
}

/// The process's memory limit is the process's: the tests that read or set it take turns.
static PROCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What a query's budget doesn't account is still bounded: past the process's memory limit
/// a query ends with a memory error (the server answers 503), never the process.
#[test]
fn a_query_past_the_process_limit_ends_with_a_memory_error() {
    let _turn = PROCESS.lock().unwrap_or_else(|e| e.into_inner());
    let engine = engine(2_000);
    let text =
        format!("SELECT ?s ?o1 ?o2 WHERE {{ ?s <{EX}hasSignature> ?o1 . ?s <{EX}createdBy> ?o2 }}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let snapshot = engine.snapshot();
    nrese_exec::memory::set_process_limit(1);
    let refused = evaluate_query(&snapshot, &query, &QueryOptions::default()).err();
    nrese_exec::memory::set_process_limit(0);
    assert!(
        matches!(
            refused,
            Some(nrese_sparql::QueryEvaluationError::MemoryLimit(_))
        ),
        "{refused:?}"
    );
    let answered = evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    let QueryResults::Solutions(solutions) = answered else {
        panic!("a SELECT")
    };
    assert_eq!(solutions.count(), 24_000);
}

/// The heap `text` holds while it is evaluated and its rows read under a memory limit of
/// `limit` bytes, with what it charged; `None` for held where it ended with a memory error.
fn run(snapshot: &nrese_engine::Snapshot, text: &str, limit: usize) -> (Option<usize>, usize) {
    let query = SparqlParser::new().parse_query(text).unwrap();
    let shared = SharedBudget::new(usize::MAX / 2);
    let options = QueryOptions {
        resources: Resources {
            memory_limit: Some(limit),
            shared_memory: Some(Arc::clone(&shared)),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let before = heap::live();
    heap::start("query");
    let answered = match evaluate_query(snapshot, &query, &options) {
        Ok(QueryResults::Solutions(solutions)) => {
            solutions.count();
            true
        }
        Ok(_) => true,
        Err(nrese_sparql::QueryEvaluationError::MemoryLimit(_)) => false,
        Err(e) => panic!("{text}: {e}"),
    };
    let held = heap::peak().saturating_sub(before);
    heap::finish();
    (answered.then_some(held), shared.peak())
}

/// The memory envelope holds: under a limit, every query either answers holding no more
/// than the limit (and 2 MiB of bounded caches: the decoders' of each parallel part, on one
/// thread here), or ends with a memory error. On 13 Oct, before the charges of working
/// memory, at 20,000 publications: MIN/MAX/SAMPLE per class held 39 MB within 16 MiB,
/// GROUP_CONCAT per publication 8.5 MB within 4 MiB, ORDER BY … LIMIT 17 MB within 4 MiB.
#[test]
fn a_query_holds_no_more_than_its_memory_limit() {
    let _turn = PROCESS.lock().unwrap_or_else(|e| e.into_inner());
    let engine = engine(20_000);
    let snapshot = engine.snapshot();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut over = Vec::new();
    for (name, text) in queries() {
        for limit in [4usize << 20, 16 << 20, 64 << 20] {
            let (held, charged) = pool.install(|| run(&snapshot, &text, limit));
            eprintln!(
                "{name:>20} within {:>3} MiB: {}, charged {charged:>9}",
                limit >> 20,
                held.map_or("memory error".to_owned(), |h| format!(
                    "held {h:>9} = {:.2} of the limit",
                    h as f64 / limit as f64
                )),
            );
            if let Some(held) = held
                && held > limit + (2 << 20)
            {
                over.push(format!("{name}: {held} bytes within {limit}"));
            }
        }
    }
    assert!(over.is_empty(), "{over:#?}");
}
