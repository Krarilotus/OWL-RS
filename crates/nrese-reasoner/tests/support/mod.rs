use std::path::PathBuf;

use nrese_core::ReasonerEngine;
use nrese_reasoner::{InferenceDelta, ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{SparqlUpdateRequest, StoreConfig, StoreMode, StoreService};
use tempfile::tempdir;

pub fn catalog_fixture_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benches/nrese-bench-harness/fixtures/catalog-cache")
        .join(filename)
}

pub fn run_rules_mvp_catalog_fixture(
    filename: &str,
    update: &str,
) -> Result<InferenceDelta, Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let store = StoreService::new(StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(catalog_fixture_path(filename)),
        ontology_fallbacks: Vec::new(),
    })?;
    store.execute_update(&SparqlUpdateRequest::new(update))?;

    let snapshot = store.dataset_snapshot()?;
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let plan = reasoner.plan(&snapshot)?;
    let output = reasoner.run(&snapshot, &plan)?;

    assert_eq!(
        output.report.status,
        nrese_core::ReasonerRunStatus::Completed
    );

    Ok(output.inferred)
}

pub fn assert_inferred_triple(
    inferred: &InferenceDelta,
    subject: &str,
    predicate: &str,
    object: &str,
) {
    assert!(
        inferred
            .derived_triples
            .iter()
            .any(|(s, p, o)| s == subject && p == predicate && o == object),
        "expected inferred triple ({subject}, {predicate}, {object}), got {:?}",
        inferred.derived_triples
    );
}
