//! The island split through the store's writes. `x owl:sameAs x` reads as a one-member
//! `SameIndividual`; a commit's check splits the ABox into islands, and that statement must
//! split with its member like any other.
//!
//! The rows run under explicit DL settings, those of the check that found the case: a 5 s
//! timeout and at most 100,000 nodes and 100,000 branch points. Before the fix, a release
//! build panicked under these settings, and a debug build panicked under the default settings
//! too.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use nrese_store::{DlConfig, MutationPipeline};

use super::{ask, pipeline_with, update};

const F: &str = "PREFIX f: <http://example.org/fuzz#> ";

fn pipeline() -> MutationPipeline {
    pipeline_with(DlConfig {
        timeout: Some(Duration::from_secs(5)),
        max_nodes: Some(100_000),
        max_branch_points: Some(100_000),
        ..DlConfig::default()
    })
}

/// Disjoint data properties with a value, then a self-`sameAs` of a name used nowhere else
/// as an individual (it is a class): the ABox has two islands when the second commit is
/// checked.
const FIRST: &str =
    "INSERT DATA { f:a3 f:d0 \"00\"^^xsd:integer . f:d0 owl:propertyDisjointWith f:d1 . }";
const SECOND: &str = "INSERT DATA { f:C2 owl:sameAs f:C2 . f:a0 a f:C2 . }";

#[test]
fn a_self_same_as_commits_beside_other_islands() {
    let dl = pipeline();
    update(&dl, &format!("{F}{FIRST}")).expect("the first commit");
    update(&dl, &format!("{F}{SECOND}")).expect("the self-sameAs commit");
    // The statement is in the data now: the next commit's check splits it again.
    update(&dl, &format!("{F}INSERT DATA {{ f:a1 a f:C3 . }}")).expect("the next commit");
    assert!(ask(
        &dl,
        "<http://example.org/fuzz#a0> a <http://example.org/fuzz#C2>"
    ));
}

/// A commit that failed leaves a store that still answers and takes the next write; the
/// self-`sameAs` commit must not fail at all.
#[test]
fn after_a_self_same_as_commit_the_store_answers_and_takes_the_next_write() {
    let dl = pipeline();
    update(&dl, &format!("{F}{FIRST}")).expect("the first commit");
    let second = catch_unwind(AssertUnwindSafe(|| update(&dl, &format!("{F}{SECOND}"))));
    assert!(
        ask(&dl, "<http://example.org/fuzz#a3> ?p ?v"),
        "after the self-sameAs commit: the store doesn't answer"
    );
    update(&dl, &format!("{F}INSERT DATA {{ f:a1 a f:C3 . }}"))
        .expect("after the self-sameAs commit: the next write is refused");
    assert!(ask(
        &dl,
        "<http://example.org/fuzz#a1> a <http://example.org/fuzz#C3>"
    ));
    match second {
        Ok(result) => {
            result.expect("the self-sameAs commit");
        }
        Err(_) => panic!("the self-sameAs commit panicked"),
    }
}
