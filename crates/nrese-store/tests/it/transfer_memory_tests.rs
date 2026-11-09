//! A transfer through the mutation pipeline draws what it reads and what it adds to the
//! request's pending changes from the memory the requests share (`total_query_memory_bytes`),
//! before it allocates (SYN-54 review r1, F1): a copy larger than that memory is refused with
//! the memory error, and the store is as it was.
//!
//! The size comes from the data: `N` distinct statements take at least `N` encoded quads of 32
//! bytes in any set that holds them, ten times the shared memory here.

use crate::support::{Writes, load_past_the_pipeline};
use nrese_store::{MutationError, StoreConfig, StoreService};

const N: usize = 40_000;
const SHARED: usize = 64 << 10;
const _: () = assert!(N * 32 >= 10 * SHARED);

#[test]
fn a_transfer_larger_than_the_shared_query_memory_is_refused() {
    let mut config = StoreConfig::in_memory();
    config.total_query_memory_bytes = SHARED;
    let store = StoreService::new(config).unwrap();
    let source: String = (0..N)
        .map(|i| format!("<urn:s{i}> <urn:p> {i} . "))
        .collect();
    load_past_the_pipeline(&store, &format!("<urn:a> {{ {source} }}"));
    // Counted without a query, which would draw on the same small memory.
    let revision = store.current_revision();
    assert_eq!(store.stats().unwrap().quad_count, N, "the source is loaded");
    let result = store.execute_update_str("ADD <urn:a> TO <urn:c>");
    let refused = matches!(
        &result,
        Err(MutationError::Store { source, .. }) if source.is_memory_limit()
    );
    assert!(
        refused,
        "a copy past the shared memory is refused: {result:?}"
    );
    assert_eq!(store.current_revision(), revision, "nothing is committed");
    assert_eq!(
        store.stats().unwrap().quad_count,
        N,
        "no statement is added"
    );
}
