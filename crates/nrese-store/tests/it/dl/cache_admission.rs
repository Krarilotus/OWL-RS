//! The result cache under `owl2-dl` (package 3.6's design note §4, the DL bound views): a
//! query whose answer the bounds decide evaluates the lower and the upper bound's views,
//! which the store derives from a published snapshot alone and keeps per revision. They
//! keep its publication (`Snapshot::with_inferred_derived_within`, package 3.0b; the
//! master's revision of F2 of the token design check), so the result cache admits them,
//! each under its own identity. Fail without 3.0b on 3.6's fix: pinning such a query
//! fails and its repeats are computed again.

use nrese_store::{CancellationToken, SparqlQueryRequest};

use super::queries::query;
use super::{PREFIXES, insert, pipeline};

/// `:i` is a `:D` through U1 and an exact service only (it is in the union of `:B` and
/// `:C`); `:j` is one in L (a `:B`): the bounds differ, so both are evaluated.
fn store_with_a_gap() -> nrese_store::MutationPipeline {
    let dl = pipeline();
    insert(
        &dl,
        ":B rdfs:subClassOf :D . :C rdfs:subClassOf :D . \
         :i a [ owl:unionOf ( :B :C ) ] . :j a :B .",
    )
    .expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(rows, ["i", "j"]);
    assert!(
        !status.paths.contains(&"closed-predicates"),
        "the bounds decide: {:?}",
        status.paths
    );
    dl
}

/// - Input: an `owl2-dl` store whose bounds differ for `?x a :D`; that query pinned as `d`.
/// - Expected: pinned, as any query of the store's is (the pin API and configured pins at
///   startup run this path).
#[test]
fn a_dl_query_the_bounds_decide_can_be_pinned() {
    let dl = store_with_a_gap();
    let request = SparqlQueryRequest::all(format!("{PREFIXES}SELECT ?x {{ ?x a :D }}"));
    let pinned = dl
        .store()
        .pin_query("d", &request, &CancellationToken::new());
    assert!(pinned.is_ok(), "{pinned:?}");
}

/// - Input: the same store; `?x a :D` asked again and again, nothing committed between.
/// - Expected: the same answer, both bounds' evaluations answered from the cache on a repeat
///   (a hit each, no part computed again).
#[test]
fn a_repeated_dl_query_the_bounds_decide_hits_both_bounds() {
    let dl = store_with_a_gap();
    let store = dl.store();
    let (first, _) = query(&dl, "SELECT ?x { ?x a :D }");
    let before = store.query_cache_stats();
    let (again, _) = query(&dl, "SELECT ?x { ?x a :D }");
    let after = store.query_cache_stats();
    assert_eq!(first, again);
    assert!(
        after.hits >= before.hits + 2,
        "both bounds from the cache: before {before:?}, after {after:?}"
    );
}
