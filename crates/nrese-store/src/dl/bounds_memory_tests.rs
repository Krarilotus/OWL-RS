//! No process-global setters: each test uses an independent store watch.

use super::*;
use crate::StoreConfig;

fn store(limit: u64) -> StoreService {
    let store = StoreService::new(StoreConfig {
        process_memory_bytes: crate::ProcessMemory::Bytes(limit),
        ..StoreConfig::in_memory()
    })
    .unwrap();
    // An existential conclusion takes the U1 route, not the OWL 2 RL shortcut.
    store
        .update_str_ungated(
            "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] . :a a :A . }",
        )
        .unwrap();
    store
}

/// What the bounds keep at a revision is charged to their budget while kept (SYN-61): U1's
/// own stack among it, which holds each of U1's facts three times as 24-byte keys, so the
/// charge is at least that, whatever the views' runs add.
#[test]
fn the_upper_bounds_stack_is_charged_while_it_is_kept() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let mut data = String::from(
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] .",
    );
    for i in 0..2_000 {
        data.push_str(&format!(" :a{i} a :A ."));
    }
    data.push('}');
    store.update_str_ungated(&data).unwrap();
    let report = report(&store);
    assert_eq!(report.unavailable, None, "{report:?}");
    let keys = report.upper_facts * 3 * std::mem::size_of::<[u64; 3]>();
    let kept = store.dl().bounds.kept().used();
    assert!(
        kept >= keys,
        "{kept} bytes kept for {} facts of U1 ({keys} bytes of its stack's keys)",
        report.upper_facts
    );
}

/// U1's build reads the whole materialised view as its evaluation's input (SYN-61): that
/// scratch is charged to the bounds' budget while it is held, so the budget's peak is at least
/// the view's facts as 24-byte keys. The view is many values of one individual, so U1's own
/// facts (an individual's few, `owl:Thing` among them) stay far below it.
#[test]
fn the_upper_bounds_build_input_is_charged_while_it_is_held() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let mut data = String::from(
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] . :a a :A .",
    );
    for i in 0..20_000 {
        data.push_str(&format!(" :a :note \"n{i}\" ."));
    }
    data.push('}');
    store.update_str_ungated(&data).unwrap();
    let report = report(&store);
    assert_eq!(report.unavailable, None, "{report:?}");
    let view = store
        .engine()
        .snapshot()
        .count_in(ReadModel::Materialised, &nrese_engine::QuadPattern::all())
        .at_least();
    let input = usize::try_from(view).unwrap() * std::mem::size_of::<[u64; 3]>();
    let peak = store.dl().bounds.kept().peak();
    assert!(
        peak >= input,
        "peak {peak} bytes while reading {view} facts ({input} bytes) for {} facts of U1",
        report.upper_facts
    );
}

#[test]
fn embedded_low_memory_bounds_are_unavailable_on_every_build_path() {
    if !nrese_exec::memory::READS_MEMORY {
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
    assert!(!store.memory_stop().exceeded(), "a zero limit never stops");
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
    if !nrese_exec::memory::READS_MEMORY {
        return;
    }
    let store = store(1);
    let mut tx = store.engine().speculative();
    source::intern_vocabulary(&tx);
    let ontology = source::read_snapshot(tx.base());
    let normalised = nrese_owl::normalise(&ontology);
    // Seed the existing U1 without a watch or a charge, as if the process had been below its
    // budget at the prior revision. The next preparation must carry its own watch.
    let upper = Upper::build(
        &ontology,
        &normalised,
        tx.base(),
        &|t| Some(tx.intern(t)),
        nrese_reasoner::eval::NEVER,
        &Arc::new(nrese_exec::Budget::unlimited()),
    )
    .unwrap();
    *store.dl().bounds.state.lock().unwrap() = Some(State {
        revision: tx.base().revision(),
        upper: Ok(Arc::new(upper)),
        last: "read",
        compiled: store.dl().bounds.compiled(),
        taxonomy: None,
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
