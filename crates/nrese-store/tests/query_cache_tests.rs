//! The query result cache: repeated queries are answered from it until the next commit,
//! volatile queries never are, and a zero budget turns it off.

use nrese_store::{StoreConfig, StoreService};

const QUERY: &str = "SELECT ?o WHERE { <http://example.com/s> <http://example.com/p> ?o }";

fn store(cache_bytes: usize) -> StoreService {
    let config = StoreConfig {
        query_cache_bytes: cache_bytes,
        ..StoreConfig::in_memory()
    };
    let store = StoreService::new(config).unwrap();
    store
        .execute_update_str("INSERT DATA { <http://example.com/s> <http://example.com/p> 1 }")
        .unwrap();
    store
}

fn answer(store: &StoreService, query: &str) -> String {
    String::from_utf8(store.execute_query_str(query).unwrap().payload).unwrap()
}

#[test]
fn repeated_queries_hit_until_the_next_commit() {
    let store = store(1 << 20);
    let first = answer(&store, QUERY);
    assert_eq!(answer(&store, QUERY), first);
    // Aggregates too: parsing names their internal variables randomly, so the key is the
    // query text, not the parsed form.
    let count = "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }";
    let counted = answer(&store, count);
    assert_eq!(answer(&store, count), counted);
    let stats = store.query_cache_stats();
    assert_eq!((stats.hits, stats.misses, stats.entries), (2, 2, 2));

    store
        .execute_update_str("INSERT DATA { <http://example.com/s> <http://example.com/p> 2 }")
        .unwrap();
    let after = answer(&store, QUERY);
    assert_ne!(after, first, "a commit starts a new revision");
    assert!(after.contains('2'));
    assert_eq!(store.query_cache_stats().misses, 3);
}

#[test]
fn volatile_queries_and_a_zero_budget_are_not_cached() {
    let store = store(1 << 20);
    answer(&store, "SELECT (RAND() AS ?r) WHERE {}");
    answer(&store, "SELECT (RAND() AS ?r) WHERE {}");
    assert_eq!(store.query_cache_stats().entries, 0);

    let off = self::store(0);
    answer(&off, QUERY);
    answer(&off, QUERY);
    let stats = off.query_cache_stats();
    assert_eq!((stats.hits, stats.entries), (0, 0));
}
