//! Shared helpers for `nrese-store` integration tests. Each test binary uses a subset.
#![allow(dead_code)]

use std::path::PathBuf;

use nrese_core::ReasonerEngine;
use nrese_reasoner::{InferenceDelta, ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{SparqlUpdateRequest, StoreConfig, StoreService};

pub fn catalog_fixture_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benches/nrese-bench-harness/fixtures/catalog-cache")
        .join(filename)
}

pub fn in_memory_store_config() -> StoreConfig {
    StoreConfig::in_memory()
}

/// Loads an official catalog ontology, applies `update`, and runs `rules-mvp` over the result.
pub fn run_rules_mvp_catalog_fixture(
    filename: &str,
    update: &str,
) -> Result<InferenceDelta, Box<dyn std::error::Error>> {
    let store = StoreService::new(StoreConfig {
        ontology_path: Some(catalog_fixture_path(filename)),
        ..in_memory_store_config()
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
