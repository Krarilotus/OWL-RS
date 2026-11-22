//! Spec: each run of a prepared query draws on a memory envelope of its own, opened by the
//! store that runs it from the query's memory limit within its catalog's budget. Runs of one
//! prepared query, at once or one after another, share neither a limit nor a refusal; a
//! query prepared by one catalog's store and run by another's charges the runner's budget.

use super::StoreService;
use crate::{PreparedQuery, SparqlQueryRequest, StoreConfig};

/// A store whose queries share a budget under 256 MiB, where an envelope keeps no credit:
/// the budget holds exactly what runs hold.
fn store() -> StoreService {
    StoreService::new(StoreConfig {
        total_query_memory_bytes: 128 << 20,
        ..StoreConfig::in_memory()
    })
    .unwrap()
}

fn prepared(store: &StoreService, limit: Option<usize>) -> PreparedQuery {
    let mut request = SparqlQueryRequest::all("SELECT * WHERE { ?s ?p ?o }");
    request.memory_limit = limit;
    store.prepare_query(&request).unwrap()
}

/// Two runs of one prepared query at once, each holding 3/4 of its 1 MiB limit: the second
/// has its room, the first's charge not in its envelope.
#[test]
fn two_runs_of_one_prepared_query_at_once_have_a_limit_each() {
    let store = store();
    let prepared = prepared(&store, Some(1 << 20));
    let first = store.settings.query_envelope(&prepared);
    let second = store.settings.query_envelope(&prepared);
    first.charge(3 << 18).unwrap();
    assert!(
        second.charge(3 << 18).is_ok(),
        "the second run is charged with the first's memory"
    );
}

/// A run after a refused one draws on an envelope of its own, with nothing of the earlier
/// run's refusal in it.
#[test]
fn a_retry_after_a_refusal_starts_unrefused() {
    let store = store();
    let prepared = prepared(&store, Some(1 << 20));
    let first = store.settings.query_envelope(&prepared);
    assert!(first.charge(2 << 20).is_err());
    assert!(first.refused());
    let retry = store.settings.query_envelope(&prepared);
    assert!(
        retry != first,
        "the retry draws on the refused run's envelope"
    );
    assert!(!retry.refused());
    assert!(retry.charge(1 << 19).is_ok());
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
    let envelope = runner.settings.query_envelope(&prepared);
    envelope.charge(1 << 20).unwrap();
    assert_eq!(runner.query_memory().unwrap().0, before + (1 << 20));
    assert_eq!(preparer.query_memory().unwrap().0, others);
}
