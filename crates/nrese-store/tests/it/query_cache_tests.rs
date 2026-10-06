//! The result cache as the store runs it: repeated queries are answered from their parts
//! until the next commit, every user gets the answer of their own access, concurrent
//! requests compute a query once, pinned results stay, and a zero budget turns it off.

use std::sync::Arc;

use nrese_sparql::GraphAccess;
use nrese_store::{CancellationToken, ReadScope, SparqlQueryRequest, StoreConfig, StoreService};

const EX: &str = "http://example.com/";

/// A store of `n` people, each knowing another and living in one of ten cities: enough
/// rows that a query over them takes longer to compute than its result takes to copy, so
/// the cache admits it.
fn store(cache_bytes: usize, n: usize) -> StoreService {
    let store = StoreService::new(StoreConfig {
        query_cache_bytes: cache_bytes,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    let mut data = String::new();
    for i in 0..n {
        data.push_str(&format!(
            "<{EX}p{i}> <{EX}knows> <{EX}p{}> . <{EX}p{i}> <{EX}livesIn> <{EX}c{}> .\n",
            (i * 7 + 1) % n,
            i % 10
        ));
    }
    store
        .execute_update_str(&format!("INSERT DATA {{ {data} }}"))
        .unwrap();
    store
}

/// People per city among those who know someone.
fn per_city() -> String {
    format!(
        "SELECT ?c (COUNT(*) AS ?n) WHERE {{ ?p <{EX}knows> ?q . ?q <{EX}livesIn> ?c }} GROUP BY ?c ORDER BY ?c"
    )
}

fn answer_as(store: &StoreService, scope: ReadScope, query: &str) -> String {
    let result = store
        .execute_query(&SparqlQueryRequest::new(query, scope))
        .unwrap();
    String::from_utf8(result.payload).unwrap()
}

fn answer(store: &StoreService, query: &str) -> String {
    answer_as(store, ReadScope::All, query)
}

#[test]
fn repeated_queries_hit_until_the_next_commit() {
    let store = store(1 << 20, 4_000);
    let first = answer(&store, &per_city());
    let stats = store.query_cache_stats();
    assert!(stats.stored >= 1 && stats.hits == 0, "{stats:?}");
    assert_eq!(answer(&store, &per_city()), first);
    // The whole query is a part: one hit answers it.
    assert_eq!(store.query_cache_stats().hits, 1);
    // Another query sharing the group: answered from it.
    let top = format!("SELECT ?c WHERE {{ {{ {} }} FILTER(?n > 0) }}", per_city());
    answer(&store, &top);
    assert_eq!(store.query_cache_stats().hits, 2);

    store
        .execute_update_str(&format!("INSERT DATA {{ <{EX}x> <{EX}knows> <{EX}p0> }}"))
        .unwrap();
    let after = answer(&store, &per_city());
    assert_ne!(after, first, "a commit starts a new revision");
    let stats = store.query_cache_stats();
    assert_eq!(stats.hits, 2, "nothing of the older revision is hit");
    assert!(stats.bytes <= stats.capacity);
}

#[test]
fn volatile_queries_and_a_zero_budget_are_not_cached() {
    let store = store(1 << 20, 100);
    // Decided on the parsed query: a space before the bracket or lower case doesn't hide
    // the call (the review of 3 October 2026, C2).
    for query in [
        "SELECT (RAND () AS ?r) WHERE {}",
        "SELECT ?v WHERE { BIND(struuid  () AS ?v) }",
    ] {
        assert_ne!(answer(&store, query), answer(&store, query), "{query}");
    }
    assert_eq!(store.query_cache_stats().hits, 0);

    let off = self::store(0, 4_000);
    answer(&off, &per_city());
    answer(&off, &per_city());
    let stats = off.query_cache_stats();
    assert_eq!((stats.hits, stats.entries, stats.capacity), (0, 0, 0));
}

#[test]
fn every_user_gets_the_answer_of_their_own_access() {
    let store = store(1 << 20, 10);
    store
        .execute_update_str(&format!(
            "INSERT DATA {{ GRAPH <{EX}g1> {{ <{EX}a> <{EX}p> 1 }} GRAPH <{EX}g2> {{ <{EX}b> <{EX}p> 2 }} }}"
        ))
        .unwrap();
    let reader = |graph: &str| {
        ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}{graph}")],
            ..GraphAccess::default()
        }))
    };
    let query = format!("SELECT ?s WHERE {{ GRAPH ?g {{ ?s <{EX}p> ?o }} }}");
    for _ in 0..2 {
        let one = answer_as(&store, reader("g1"), &query);
        let two = answer_as(&store, reader("g2"), &query);
        let all = answer(&store, &query);
        assert!(one.contains("/a") && !one.contains("/b"), "{one}");
        assert!(two.contains("/b") && !two.contains("/a"), "{two}");
        assert!(all.contains("/a") && all.contains("/b"), "{all}");
    }
}

#[test]
fn concurrent_requests_compute_a_query_once() {
    // How many parts one run computes.
    let alone = store(1 << 20, 4_000);
    answer(&alone, &per_city());
    let parts = alone.query_cache_stats().misses;

    let store = store(1 << 20, 4_000);
    let threads = 8;
    let start = std::sync::Barrier::new(threads);
    let answers: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    answer(&store, &per_city())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(answers.iter().all(|a| *a == answers[0]));
    let stats = store.query_cache_stats();
    // Computed once; the other seven waited for it or found it cached.
    assert_eq!(stats.misses, parts, "{stats:?}");
    assert_eq!(stats.hits + stats.shared, threads as u64 - 1, "{stats:?}");
}

#[test]
fn pinned_results_stay_and_are_pinned_again_after_a_commit() {
    // A budget of about four results of the per-city query's size.
    let store = store(64 << 10, 4_000);
    let token = CancellationToken::new();
    let pinned = store
        .pin_query("per-city", &SparqlQueryRequest::all(per_city()), &token)
        .unwrap();
    assert_eq!((pinned.name.as_str(), pinned.rows), ("per-city", 10));
    let revision = pinned.revision.expect("held");
    // Other results come and go; the pinned one stays.
    for i in 0..40 {
        answer(
            &store,
            &format!(
                "SELECT ?q (COUNT(*) AS ?n) WHERE {{ ?p <{EX}knows> ?q . ?p <{EX}livesIn> <{EX}c{}> }} GROUP BY ?q",
                i % 10
            ),
        );
    }
    let stats = store.query_cache_stats();
    assert!(
        stats.pinned_bytes > 0 && stats.bytes <= stats.capacity,
        "{stats:?}"
    );
    let hits = stats.hits;
    answer(&store, &per_city());
    assert_eq!(store.query_cache_stats().hits, hits + 1);
    // A commit: not held until the query runs again, then pinned at the new revision.
    store
        .execute_update_str(&format!("INSERT DATA {{ <{EX}x> <{EX}knows> <{EX}p0> }}"))
        .unwrap();
    assert_eq!(store.pinned_queries()[0].revision, None);
    answer(&store, &per_city());
    let again = store.pinned_queries()[0].revision.expect("pinned again");
    assert!(again > revision);
    // Queries that can't be cached can't be pinned.
    let volatile = store.pin_query(
        "rand",
        &SparqlQueryRequest::all("SELECT (RAND() AS ?r) WHERE {}"),
        &token,
    );
    assert!(volatile.is_err());
    assert!(store.unpin_query("per-city"));
    assert!(!store.unpin_query("per-city"));
    assert_eq!(store.query_cache_stats().pinned_bytes, 0);
}

#[test]
fn configured_queries_are_pinned_at_startup() {
    let dir = std::env::temp_dir().join(format!("nrese-pins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("everything.rq");
    std::fs::write(&path, "SELECT * WHERE { ?s ?p ?o }").unwrap();
    let store = StoreService::new(StoreConfig {
        pinned_queries: vec![path],
        ..StoreConfig::in_memory()
    })
    .unwrap();
    let pins = store.pinned_queries();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].name, "everything");
    std::fs::remove_dir_all(&dir).unwrap();
    // A file that isn't there is a startup error.
    let missing = StoreService::new(StoreConfig {
        pinned_queries: vec![dir.join("missing.rq")],
        ..StoreConfig::in_memory()
    });
    assert!(missing.is_err());
}
