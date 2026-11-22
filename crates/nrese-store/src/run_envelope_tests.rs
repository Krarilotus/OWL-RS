//! Spec: each run of a prepared query ([`StoreService::open_run`]) draws on a memory envelope
//! of its own, opened by the store that runs it from the query's memory limit within its
//! catalog's budget: its evaluation, and what its transport holds of its output. Runs of one
//! prepared query, at once or one after another, share neither a limit nor a refusal; a
//! query prepared by one catalog's store and run by another's charges the runner's budget.

use super::StoreService;
use crate::{CancellationToken, PreparedQuery, SparqlQueryRequest, StoreConfig};

/// A store without a result cache (every run evaluates), whose queries share a budget under
/// 256 MiB, where an envelope keeps no credit: the budget holds exactly what runs hold.
fn store() -> StoreService {
    StoreService::new(StoreConfig {
        total_query_memory_bytes: 128 << 20,
        query_cache_bytes: 0,
        ..StoreConfig::in_memory()
    })
    .unwrap()
}

/// 40,000 rows of two columns, which a run's evaluation holds at once.
fn query() -> String {
    let values: String = (1..=200).map(|n| format!("{n} ")).collect();
    format!("SELECT ?a ?b WHERE {{ VALUES ?a {{ {values}}} VALUES ?b {{ {values}}} }}")
}

fn prepared(store: &StoreService, limit: Option<usize>) -> PreparedQuery {
    let mut request = SparqlQueryRequest::all(query());
    request.memory_limit = limit;
    store.prepare_query(&request).unwrap()
}

/// Two runs of one prepared query opened at once (16 MiB each, which a run's evaluation fits
/// in), the first's transport holding all of its envelope but 64 KiB, far less than the
/// 40,000 rows need: the first's evaluation is refused, by its own limit; the second's is
/// answered within its own limit and charges only its own envelope.
#[test]
fn two_runs_of_one_prepared_query_opened_at_once_each_draw_on_their_own() {
    const LIMIT: usize = 16 << 20;
    const ROOM: usize = 64 << 10;
    let store = store();
    let prepared = prepared(&store, Some(LIMIT));
    let token = CancellationToken::new();
    let (first, second) = (store.open_run(&prepared), store.open_run(&prepared));
    let (held, own) = (first.memory().clone(), second.memory().clone());
    held.charge(LIMIT - ROOM).unwrap();
    let refused = first
        .run_reporting(&token, std::io::sink(), |_, _| None)
        .expect_err("the first run's evaluation fits beside what its transport holds");
    assert!(
        refused.is_memory_limit() && !refused.is_server_memory_limit(),
        "{refused}"
    );
    let answered = second.run_reporting(&token, std::io::sink(), |_, _| None);
    assert!(answered.is_ok(), "{answered:?}");
    assert!(
        own.peak() > 0,
        "the second run's evaluation charged another envelope"
    );
    assert_eq!(
        held.used(),
        LIMIT - ROOM,
        "the second run charged the first's envelope"
    );
}

/// A run after a refused one draws on an envelope of its own, with nothing of the earlier
/// run's refusal in it.
#[test]
fn a_retry_after_a_refusal_starts_unrefused() {
    let store = store();
    let prepared = prepared(&store, Some(1 << 20));
    let first = store.open_run(&prepared);
    assert!(first.memory().charge(2 << 20).is_err());
    assert!(first.memory().refused());
    let retry = store.open_run(&prepared);
    assert!(
        retry.memory() != first.memory(),
        "the retry draws on the refused run's envelope"
    );
    assert!(!retry.memory().refused());
    assert!(retry.memory().charge(1 << 19).is_ok());
}

/// A query one catalog's store prepared, run by another's, charges the runner's budget.
#[test]
fn a_run_charges_the_budget_of_the_store_that_runs_it() {
    let (preparer, runner) = (store(), store());
    let prepared = prepared(&preparer, None);
    let (before, others) = (
        runner.query_memory().unwrap().0,
        preparer.query_memory().unwrap().0,
    );
    let run = runner.open_run(&prepared);
    run.memory().charge(1 << 20).unwrap();
    assert_eq!(runner.query_memory().unwrap().0, before + (1 << 20));
    assert_eq!(preparer.query_memory().unwrap().0, others);
}
