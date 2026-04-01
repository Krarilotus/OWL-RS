use std::path::PathBuf;

use nrese_core::ReasonerEngine;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{SparqlUpdateRequest, StoreConfig, StoreMode, StoreService};
use tempfile::tempdir;

fn catalog_fixture_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benches/nrese-bench-harness/fixtures/catalog-cache")
        .join(filename)
}

#[test]
fn rules_mvp_infers_foaf_agent_from_official_foaf_fixture() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let store = StoreService::new(StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(catalog_fixture_path("foaf.rdf")),
        ontology_fallbacks: Vec::new(),
    })?;
    store.execute_update(&SparqlUpdateRequest::new(
        "PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         INSERT DATA { <http://example.com/alice> a foaf:Person . }",
    ))?;

    let snapshot = store.dataset_snapshot()?;
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let plan = reasoner.plan(&snapshot)?;
    let output = reasoner.run(&snapshot, &plan)?;

    assert_eq!(
        output.report.status,
        nrese_core::ReasonerRunStatus::Completed
    );
    assert!(output.inferred.derived_triples.iter().any(|(s, p, o)| {
        s == "http://example.com/alice"
            && p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
            && o == "http://xmlns.com/foaf/0.1/Agent"
    }));
    Ok(())
}

#[test]
fn rules_mvp_infers_time_inverse_and_transitive_property_closure_from_official_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let store = StoreService::new(StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(catalog_fixture_path("time.ttl")),
        ontology_fallbacks: Vec::new(),
    })?;
    store.execute_update(&SparqlUpdateRequest::new(
        "PREFIX time: <http://www.w3.org/2006/time#>
         INSERT DATA {
           <http://example.com/a> time:before <http://example.com/b> .
           <http://example.com/b> time:before <http://example.com/c> .
         }",
    ))?;
    let snapshot = store.dataset_snapshot()?;
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let plan = reasoner.plan(&snapshot)?;
    let output = reasoner.run(&snapshot, &plan)?;

    assert!(
        output.inferred.derived_triples.iter().any(|(s, p, o)| {
            s == "http://example.com/a"
                && p == "http://www.w3.org/2006/time#before"
                && o == "http://example.com/c"
        }),
        "expected transitive closure, got {:?}",
        output.inferred.derived_triples
    );
    assert!(
        output.inferred.derived_triples.iter().any(|(s, p, o)| {
            s == "http://example.com/b"
                && p == "http://www.w3.org/2006/time#after"
                && o == "http://example.com/a"
        }),
        "expected inverse-property inference, got {:?}",
        output.inferred.derived_triples
    );
    Ok(())
}
