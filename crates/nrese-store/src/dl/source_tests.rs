//! Dictionary lifecycle V1's store guards for the DL source helpers
//! (`notes/2026-11-09-dict-v1-speculative-private-design.md` §4, items 5, 6 and 7).

use std::sync::Arc;

use nrese_exec::Budget;

use crate::dl::bounds::upper_facts;
use crate::{StoreConfig, StoreError, StoreService};

/// Item 5: the U1 a cold read keeps holds valid names until the next commit. Its red is the
/// mutant that gives U1's names a private vocabulary (design note §4, item 5's record): the
/// window is between the cold read and the next commit, and terms appended there (an aborted
/// writer's stay in the dictionary) take the next ids.
///
/// - Input: a store with an existential axiom written without the gates (no commit interned
///   U1's names); a cold read that keeps U1; a writer that interns two new terms and is dropped
///   (no commit); then the kept U1 and U1 afresh, at the same revision.
/// - Expected: the revision unchanged; the kept U1 equals U1 afresh, and neither names the
///   aborted writer's terms or an undecoded `#id`.
#[test]
fn the_kept_cold_upper_bound_survives_an_aborted_writers_terms() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .update_str_ungated(
            "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
                 owl:someValuesFrom :B ] . :a a :A . }",
        )
        .unwrap();
    let cold = upper_facts(&store, false).expect("the cold read keeps U1");
    let revision = store.current_revision();
    let mut aborted = store.engine().transaction();
    for name in ["urn:test:aborted1", "urn:test:aborted2"] {
        let node = nrese_rdf::NamedNode::new_unchecked(name);
        aborted.insert(
            nrese_rdf::Quad::new(
                node.clone(),
                node.clone(),
                node,
                nrese_rdf::GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    drop(aborted);
    assert_eq!(store.current_revision(), revision, "no commit");
    let kept = upper_facts(&store, false).expect("the kept U1");
    let afresh = upper_facts(&store, true).expect("U1 afresh");
    let wrong: Vec<&String> = cold
        .iter()
        .chain(&kept)
        .chain(&afresh)
        .flatten()
        .filter(|t| t.starts_with('#') || t.contains("aborted"))
        .collect();
    assert!(
        wrong.is_empty(),
        "undecoded or the aborted writer's: {wrong:?}"
    );
    assert_eq!(kept, cold, "the kept U1, read again");
    assert_eq!(kept, afresh, "the kept U1 is U1 afresh");
}

/// Item 6: the bounded-prefix precondition, checked where a DL request opens its private
/// transaction.
///
/// - Input: a published snapshot of another store's engine.
/// - Expected: in a debug build, opening the request's private transaction against it panics
///   with the precondition's message (the ids would be compared across dictionaries).
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "is a prefix of its vocabulary")]
fn a_private_transaction_refuses_another_engines_published_input() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let other = StoreService::new(StoreConfig::in_memory()).unwrap();
    let foreign = other.engine().snapshot();
    let _ = super::private_over(store.engine(), &foreign, Arc::new(Budget::unlimited()));
}

/// Item 6, its own engine: an earlier snapshot of the same engine is accepted.
#[test]
fn a_private_transaction_takes_its_engines_earlier_snapshot() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let earlier = store.engine().snapshot();
    let tx = super::private_over(store.engine(), &earlier, Arc::new(Budget::unlimited()));
    assert!(tx.extends(&earlier));
}

/// Item 7, at the error's owner: an engine memory refusal converts to the memory limit's
/// class, any other engine error to an engine error.
#[test]
fn an_engine_memory_refusal_is_the_store_memory_limit() {
    let exceeded = nrese_exec::BudgetExceeded {
        limit: 0,
        requested: 1,
        used: 0,
        shared: false,
        unread: false,
    };
    let refused = StoreError::from(nrese_engine::EngineError::MemoryLimit(exceeded));
    assert!(refused.is_memory_limit(), "{refused:?}");
    let other = StoreError::from(nrese_engine::EngineError::Corruption("x".to_owned()));
    assert!(
        matches!(other, StoreError::Engine(_)) && !other.is_memory_limit(),
        "{other:?}"
    );
}

/// Item 7: a private transaction's refusal is the request's memory limit.
///
/// - Input: a DL request's private transaction with no room; the fresh names resolved in it.
/// - Expected: `resolve` gives `Err`, classified as the memory limit (not an engine error, not
///   "missing on a replica"); a replica's missing term stays `Ok(None)`.
#[test]
fn a_refused_dl_name_is_the_memory_limit_not_a_missing_one() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let snapshot = store.engine().snapshot();
    let tx = super::private_over(store.engine(), &snapshot, Arc::new(Budget::new(0)));
    let iri = super::fresh_iri(0);
    let term: nrese_rdf::TermRef<'_> = nrese_rdf::NamedNodeRef::new_unchecked(&iri).into();
    let refused: StoreError = super::resolve(false, &tx, term).unwrap_err();
    assert!(refused.is_memory_limit(), "{refused:?}");
    assert!(matches!(super::resolve(true, &tx, term), Ok(None)));
}
