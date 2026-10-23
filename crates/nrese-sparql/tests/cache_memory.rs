//! Guard of the result cache's budget (docs/design/performance.md §0): the heap the cache
//! holds, counted by the allocator, is never more than its byte budget, however many
//! parts the queries offer it; and its own count is honest (not far above what it holds,
//! which would waste the budget). Measured with a counting allocator, so this file is a
//! test binary of its own.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::heap;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{
    CachedOutput, QueryOptions, ResultCache, cached_output, evaluate_query, write_results,
};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

const EX: &str = "http://example.org/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// 3,000 nodes, each knowing three others, with a label and one of ten kinds.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..3000u32 {
        let node = ex(&format!("n{i}"));
        let mut add = |p: &str, o: nrese_rdf::Term| {
            tx.insert(Quad::new(node.clone(), ex(p), o, GraphName::DefaultGraph).as_ref());
        };
        for step in [1, 7, 31] {
            add("knows", ex(&format!("n{}", (i + step) % 3000)).into());
        }
        add(
            "label",
            Literal::new_simple_literal(format!("label {i}")).into(),
        );
        add("kind", ex(&format!("k{}", i % 10)).into());
    }
    tx.commit().unwrap();
    engine
}

/// Distinct queries of many sizes: joins per kind, groups, computed values.
fn queries() -> Vec<String> {
    let mut queries = Vec::new();
    for k in 0..10 {
        queries.push(format!(
            "SELECT ?a ?b WHERE {{ ?a <{EX}knows> ?b . ?b <{EX}kind> <{EX}k{k}> }}"
        ));
        queries.push(format!(
            "SELECT ?a ?l WHERE {{ ?a <{EX}knows> ?b . ?b <{EX}kind> <{EX}k{k}> . ?a <{EX}label> ?l }}"
        ));
        queries.push(format!(
            "SELECT ?b (COUNT(*) AS ?n) WHERE {{ ?a <{EX}knows> ?b . ?a <{EX}kind> <{EX}k{k}> }} GROUP BY ?b"
        ));
        queries.push(format!(
            "SELECT ?a ?s WHERE {{ ?a <{EX}kind> <{EX}k{k}> ; <{EX}label> ?l BIND(CONCAT(?l, \"!\") AS ?s) }}"
        ));
    }
    queries
}

/// The tests of this binary one at a time: the allocator counts the whole process.
static HEAP: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A hit is charged before its copy is made (an independent audit, 13 Oct 2026: concurrent
/// hits of a large part each copied it before their charge failed): a cached part of 27,000
/// rows, about 650 KB, asked for again under a query budget of 128 KiB ends with a memory
/// error, and the heap's peak while it runs stays below half the part.
#[test]
fn a_cache_hit_past_the_query_budget_copies_nothing() {
    let _serial = HEAP.lock().unwrap_or_else(|p| p.into_inner());
    let engine = engine();
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query(&format!(
            "SELECT ?a ?b ?c WHERE {{ ?a <{EX}knows> ?b . ?b <{EX}knows> ?c }}"
        ))
        .unwrap();
    let cache = Arc::new(ResultCache::new(64 << 20).admitting_all());
    let unlimited = QueryOptions {
        result_cache: Some(Arc::clone(&cache)),
        ..QueryOptions::default()
    };
    let rows = match evaluate_query(&snapshot, &query, &unlimited).unwrap() {
        nrese_sparql::QueryResults::Solutions(solutions) => solutions.count(),
        _ => unreachable!("a SELECT"),
    };
    assert_eq!(rows, 27_000);
    // The whole answer's part, as columns of ids (the cache may hold its join's part too).
    let part = 27_000 * 3 * 8;
    assert!(cache.stats().bytes >= part, "{:?}", cache.stats());
    let small = QueryOptions {
        result_cache: Some(Arc::clone(&cache)),
        resources: nrese_sparql::Resources {
            memory: nrese_exec::Envelope::new(Some(128 << 10), None),
            ..Default::default()
        },
        ..QueryOptions::default()
    };
    let hits = cache.stats().hits;
    heap::start("hit");
    let before = heap::live();
    let outcome = evaluate_query(&snapshot, &query, &small).map(|results| match results {
        nrese_sparql::QueryResults::Solutions(mut solutions) => solutions.find_map(Result::err),
        _ => None,
    });
    let peak = heap::peak().saturating_sub(before);
    heap::finish();
    let error = match outcome {
        Err(error) => error,
        Ok(Some(error)) => error,
        Ok(None) => panic!("the hit fit a budget of 128 KiB"),
    };
    assert!(
        matches!(error, nrese_sparql::QueryEvaluationError::MemoryLimit(_)),
        "{error}"
    );
    assert!(cache.stats().hits > hits, "{:?}", cache.stats());
    assert!(
        peak < part / 2,
        "the hit's heap peaked {peak} bytes above the start for a part of {part}"
    );
}

#[test]
fn the_result_cache_holds_no_more_heap_than_its_budget() {
    let _serial = HEAP.lock().unwrap_or_else(|p| p.into_inner());
    let engine = engine();
    let snapshot = engine.snapshot();
    let queries: Vec<_> = queries()
        .iter()
        .map(|text| SparqlParser::new().parse_query(text).unwrap())
        .collect();
    for budget in [64 << 10, 1 << 20, 16 << 20] {
        let cache = Arc::new(ResultCache::new(budget).admitting_all());
        {
            let options = QueryOptions {
                result_cache: Some(Arc::clone(&cache)),
                ..QueryOptions::default()
            };
            for _ in 0..2 {
                for query in &queries {
                    let results = evaluate_query(&snapshot, query, &options).unwrap();
                    if let nrese_sparql::QueryResults::Solutions(solutions) = results {
                        assert!(solutions.count() > 0);
                    }
                    // The answer's bytes too, as the store keeps them.
                    let format = "text/tab-separated-values";
                    if let CachedOutput::Miss(slot) =
                        cached_output(&snapshot, query, &options, format, "")
                    {
                        let mut written = Vec::new();
                        let tsv = nrese_sparql::ResultsFormat::Tsv;
                        write_results(&snapshot, query, &options, tsv, None, &mut written)
                            .unwrap()
                            .unwrap();
                        slot.offer(written, std::time::Duration::from_millis(1));
                    }
                }
            }
        }
        let stats = cache.stats();
        assert!(stats.stored > 0 && stats.hits > 0, "{stats:?}");
        assert!(stats.answers > 0, "{stats:?}");
        assert!(stats.bytes <= budget, "{stats:?}");
        let with = heap::live();
        drop(cache);
        let held = with - heap::live();
        assert!(
            held <= budget,
            "the cache held {held} bytes of heap over a budget of {budget}: {stats:?}"
        );
        // 1 to 2 % above what it holds on 6 October.
        assert!(
            held * 10 >= stats.bytes * 9,
            "the cache counts {} bytes and holds {held}: {stats:?}",
            stats.bytes
        );
        eprintln!(
            "budget {budget}: holds {held} bytes, counts {}, {stats:?}",
            stats.bytes
        );
    }
}

/// Equal caller contexts on output hits must not grow the cache past its hard budget
/// (docs/design/performance.md §0): four valid answers under a 144-KiB budget, each
/// distinguished by a 32-KiB caller context reconstructed independently for the hit.
#[test]
fn equal_context_output_hits_stay_within_the_cache_budget() {
    let _serial = HEAP.lock().unwrap_or_else(|p| p.into_inner());
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query("SELECT ?x WHERE { VALUES ?x { <http://example.org/value> } }")
        .unwrap();
    let budget = 144 << 10;
    let format = "text/tab-separated-values";
    for rehit in [false, true] {
        let cache = Arc::new(ResultCache::new(budget).admitting_all());
        {
            // Write with no cache so the fixture offers only whole-answer entries.
            let mut answer = Vec::new();
            write_results(
                &snapshot,
                &query,
                &QueryOptions::default(),
                nrese_sparql::ResultsFormat::Tsv,
                None,
                &mut answer,
            )
            .unwrap()
            .unwrap();
            assert_eq!(answer, b"?x\n<http://example.org/value>\n");
            let options = QueryOptions {
                result_cache: Some(Arc::clone(&cache)),
                ..QueryOptions::default()
            };
            for marker in *b"abcd" {
                let context = String::from_utf8(vec![marker; 32 << 10]).unwrap();
                let CachedOutput::Miss(slot) =
                    cached_output(&snapshot, &query, &options, format, &context)
                else {
                    panic!("each distinct context must start with a miss");
                };
                slot.offer(answer.clone(), std::time::Duration::from_millis(1));
            }
            let admitted = cache.stats();
            assert_eq!(
                (admitted.entries, admitted.answers, admitted.stored),
                (4, 4, 4)
            );
            if rehit {
                for marker in *b"abcd" {
                    let context = String::from_utf8(vec![marker; 32 << 10]).unwrap();
                    let CachedOutput::Hit(bytes) =
                        cached_output(&snapshot, &query, &options, format, &context)
                    else {
                        panic!("all four admitted answers must be hits");
                    };
                    assert_eq!(bytes.as_ref(), answer.as_slice());
                }
            }
        }
        let stats = cache.stats();
        assert_eq!((stats.entries, stats.answers, stats.stored), (4, 4, 4));
        assert_eq!(stats.hits, if rehit { 4 } else { 0 });
        assert!(stats.bytes <= budget, "{stats:?}");
        assert_eq!(Arc::strong_count(&cache), 1, "caller cache aliases dropped");
        let with = heap::live();
        drop(cache);
        let held = with - heap::live();
        eprintln!(
            "CACHE_KEY_BUDGET rehit={rehit} budget={budget} held={held} stats_bytes={} entries={} answers={} stored={} hits={}",
            stats.bytes, stats.entries, stats.answers, stats.stored, stats.hits
        );
        assert!(
            held <= budget,
            "equal-context output cache exceeded hard budget: rehit={rehit} held={held} budget={budget}: {stats:?}"
        );
    }
}
