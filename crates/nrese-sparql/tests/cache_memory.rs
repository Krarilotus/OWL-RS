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

/// Hits keep the cache within its budget (docs/design/performance.md §0): four answers
/// under 144 KiB, each told apart by a 32-KiB caller context that every lookup builds
/// anew, hold no more heap after they are hit than before.
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
        assert!(
            held <= budget,
            "hit {rehit}: the cache held {held} bytes of heap over a budget of {budget}: {stats:?}"
        );
        eprintln!(
            "hit {rehit}: holds {held} bytes, counts {}, {stats:?}",
            stats.bytes
        );
    }
}
