use std::path::PathBuf;

use crate::support::{assert_inferred_triple, inferred_statements};
use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{StoreConfig, StoreService};

fn minimal_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ontologies/minimal_services.ttl")
}

/// Reasoner v2 over a store preloaded from Turtle: the inferred stack holds the closure.
#[test]
fn owl2_rl_materialises_a_ttl_preloaded_store() -> Result<(), Box<dyn std::error::Error>> {
    let store = StoreService::new(StoreConfig::in_memory().with_ontology(minimal_fixture_path()))?;
    let report = store.rematerialise(Ruleset::Owl2Rl)?;
    assert_eq!(report.violations, 0);

    let inferred = inferred_statements(&store)?;
    assert_inferred_triple(
        &inferred,
        "http://example.com/alice",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://example.com/Parent",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/bob",
        "http://example.com/friendOf",
        "http://example.com/alice",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/spec",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://example.com/Document",
    );
    Ok(())
}
