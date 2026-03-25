use std::path::PathBuf;

use nrese_core::ReasonerEngine;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{StoreConfig, StoreMode, StoreService};
use tempfile::tempdir;

fn minimal_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ontologies/minimal_services.ttl")
}

#[test]
fn rules_mvp_runs_against_ttl_preloaded_store_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let store = StoreService::new(StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(minimal_fixture_path()),
        ontology_fallbacks: Vec::new(),
    })?;
    let snapshot = store.dataset_snapshot()?;
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));

    let plan = reasoner.plan(&snapshot)?;
    let output = reasoner.run(&snapshot, &plan)?;
    let derived = output.inferred.derived_triples;

    assert_eq!(
        output.report.status,
        nrese_core::ReasonerRunStatus::Completed
    );
    assert!(derived.iter().any(|(s, p, o)| {
        s == "http://example.com/alice"
            && p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
            && o == "http://example.com/Parent"
    }));
    assert!(derived.iter().any(|(s, p, o)| {
        s == "http://example.com/bob"
            && p == "http://example.com/friendOf"
            && o == "http://example.com/alice"
    }));
    assert!(derived.iter().any(|(s, p, o)| {
        s == "http://example.com/spec"
            && p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
            && o == "http://example.com/Document"
    }));

    Ok(())
}
