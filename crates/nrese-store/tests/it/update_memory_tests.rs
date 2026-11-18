//! Clearing a graph through the mutation pipeline, by SPARQL Update or the Graph Store
//! Protocol, draws the statements it retracts, held in the request's pending changes, from the
//! memory the requests share (`total_query_memory_bytes`) before it allocates (D16-039): a
//! clear larger than that memory is refused with the memory error, and the store is as it was.
//! With memory to spare, the same clear succeeds (guard rows).
//!
//! The size comes from the data: `N` distinct statements take at least `N` encoded quads of 32
//! bytes in any set that holds them, ten times the shared memory here.

use crate::support::{Writes, load_past_the_pipeline};
use nrese_store::{GraphTarget, MutationError, StoreConfig, StoreService};

const N: usize = 40_000;
const SHARED: usize = 64 << 10;
const _: () = assert!(N * 32 >= 10 * SHARED);

/// A store of `N` statements in `<urn:a>`, whose requests share `shared` bytes.
fn store(shared: usize) -> StoreService {
    let mut config = StoreConfig::in_memory();
    config.total_query_memory_bytes = shared;
    let store = StoreService::new(config).unwrap();
    let source: String = (0..N)
        .map(|i| format!("<urn:s{i}> <urn:p> {i} . "))
        .collect();
    load_past_the_pipeline(&store, &format!("<urn:a> {{ {source} }}"));
    assert_eq!(quads(&store), N, "the source is loaded");
    store
}

/// The store's statements, counted without a query, which would draw on the same small memory.
fn quads(store: &StoreService) -> usize {
    store.stats().unwrap().quad_count
}

fn graph_a() -> GraphTarget {
    GraphTarget::NamedGraph("urn:a".to_owned())
}

fn refused_for_memory<T: std::fmt::Debug>(result: &Result<T, MutationError>) -> bool {
    matches!(
        result,
        Err(MutationError::Store { source, .. }) if source.is_memory_limit()
    )
}

#[test]
fn a_clear_larger_than_the_shared_query_memory_is_refused() {
    let store = store(SHARED);
    let revision = store.current_revision();
    let result = store.execute_update_str("CLEAR GRAPH <urn:a>");
    assert!(
        refused_for_memory(&result),
        "a clear past the shared memory is refused: {result:?}"
    );
    assert_eq!(store.current_revision(), revision, "nothing is committed");
    assert_eq!(quads(&store), N, "no statement is removed");
}

#[test]
fn a_graph_store_delete_larger_than_the_shared_query_memory_is_refused() {
    let store = store(SHARED);
    let revision = store.current_revision();
    let result = store.execute_graph_delete(&graph_a());
    assert!(
        refused_for_memory(&result),
        "a graph delete past the shared memory is refused: {result:?}"
    );
    assert_eq!(store.current_revision(), revision, "nothing is committed");
    assert_eq!(quads(&store), N, "no statement is removed");
}

/// Guard: with memory to spare, both clears succeed and remove every statement.
#[test]
fn guard_clears_within_the_shared_query_memory_succeed() {
    let store = store(256 << 20);
    store.execute_update_str("CLEAR GRAPH <urn:a>").unwrap();
    assert_eq!(quads(&store), 0, "CLEAR removes the graph");
    let store = self::store(256 << 20);
    let report = store.execute_graph_delete(&graph_a()).unwrap();
    assert!(report.modified, "the graph delete changes the store");
    assert_eq!(quads(&store), 0, "DELETE removes the graph");
}
