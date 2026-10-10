//! No process-global setters: each test uses an independent store watch.

use super::*;
use crate::StoreConfig;

fn store(limit: u64) -> StoreService {
    let store = StoreService::new(StoreConfig {
        process_memory_bytes: limit,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    // An existential conclusion takes the U1 route, not the OWL 2 RL shortcut.
    store
        .execute_update_str(
            "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] . :a a :A . }",
        )
        .unwrap();
    store
}

#[test]
fn embedded_low_memory_bounds_are_unavailable_on_every_build_path() {
    if nrese_exec::memory::process_bytes().is_none() {
        return;
    }
    let store = store(1);
    assert_eq!(
        super::super::classification::options(&store).max_memory,
        Some(1)
    );
    assert!(upper_facts(&store, true).is_none(), "fresh diagnostic U1");
    let report = report(&store);
    assert!(report.unavailable.as_deref().unwrap().contains("stopped"));
    assert!(upper_facts(&store, false).is_none(), "cached read U1");
    // A distinct store ensures prepare cannot just reuse an unavailable read result.
    let store = self::store(1);
    let tx = store.engine().speculative();
    let prepared = prepare(&store, &tx, nrese_reasoner::eval::NEVER);
    assert!(!prepared.proves_consistency);
    assert!(matches!(prepared.prepared, Prepared::Rebuilt(upper) if upper.is_err()));
}

#[test]
fn embedded_zero_memory_policy_leaves_bounds_enabled() {
    let store = store(0);
    assert!(store.memory_watch().is_none());
    assert_eq!(
        super::super::classification::options(&store).max_memory,
        Some(0)
    );
    let fresh = upper_facts(&store, true).expect("fresh U1 with disabled watch");
    assert!(!fresh.is_empty());
    assert!(report(&store).unavailable.is_none());
    assert_eq!(upper_facts(&store, false).unwrap(), fresh);
    let tx = store.engine().speculative();
    assert!(prepare(&store, &tx, nrese_reasoner::eval::NEVER).proves_consistency);
}

#[test]
fn embedded_low_memory_stops_maintenance_of_an_existing_upper_bound() {
    if nrese_exec::memory::process_bytes().is_none() {
        return;
    }
    let store = store(1);
    let mut tx = store.engine().speculative();
    source::intern_vocabulary(&tx);
    let ontology = source::read_snapshot(tx.base());
    let normalised = nrese_owl::normalise(&ontology);
    // Seed the existing U1 without a watch, as if the process had been below its
    // budget at the prior revision. The next preparation must carry its own watch.
    let upper = Upper::build(
        &ontology,
        &normalised,
        tx.base(),
        &|t| Some(tx.intern(t)),
        nrese_reasoner::eval::NEVER,
    )
    .unwrap();
    *store.dl().bounds.state.lock().unwrap() = Some(State {
        revision: tx.base().revision(),
        upper: Ok(upper),
        last: "read",
        taxonomy: std::sync::OnceLock::new(),
        rules: false,
    });
    let quad = nrese_rdf::Quad::new(
        nrese_rdf::NamedNode::new_unchecked("urn:test:b"),
        nrese_rdf::vocab::rdf::TYPE,
        nrese_rdf::NamedNode::new_unchecked("urn:test:A"),
        nrese_rdf::GraphName::DefaultGraph,
    );
    tx.insert(quad.as_ref());
    let prepared = prepare(&store, &tx, nrese_reasoner::eval::NEVER);
    assert!(!prepared.proves_consistency);
    assert!(matches!(prepared.prepared, Prepared::Rebuilt(upper) if upper.is_err()));
}
