use std::fs;
use std::path::PathBuf;

use nrese_core::DatasetSnapshot;
use nrese_store::{
    GraphReadRequest, GraphTarget, GraphWriteRequest, QueryResultKind, SparqlQueryRequest,
    SparqlUpdateRequest, StoreConfig, StoreError, StoreMode, StoreService,
};
use tempfile::tempdir;

fn test_ontology() -> &'static str {
    "@prefix ex: <http://example.com/> .
ex:s ex:p ex:o .
"
}

fn minimal_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ontologies/minimal_services.ttl")
}

fn new_in_memory_service() -> Result<StoreService, Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let config = StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    };

    Ok(StoreService::new(config)?)
}

#[test]
fn preload_ontology_and_query_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let ontology_path = temp.path().join("test_ontology.ttl");
    fs::write(&ontology_path, test_ontology())?;

    let config = StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused-in-memory"),
        preload_ontology: true,
        ontology_path: Some(ontology_path),
        ontology_fallbacks: Vec::new(),
    };

    let service = StoreService::new(config)?;
    let stats = service.stats()?;
    assert_eq!(stats.quad_count, 1);
    assert_eq!(stats.named_graph_count, 0);
    assert!(!stats.is_empty);
    assert!(service.preloaded_ontology_path().is_some());

    let select = service.execute_query_str("SELECT ?s WHERE { ?s ?p ?o }")?;
    assert_eq!(select.kind, QueryResultKind::Solutions);
    assert_eq!(select.media_type, "application/sparql-results+json");
    let select_text = String::from_utf8(select.payload)?;
    assert!(select_text.contains("http://example.com/s"));

    let ask = service.execute_query_str("ASK WHERE { <http://example.com/s> ?p ?o }")?;
    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert_eq!(ask.media_type, "application/sparql-results+json");
    let ask_text = String::from_utf8(ask.payload)?;
    assert!(ask_text.contains("true"));

    let construct = service.execute_query(&SparqlQueryRequest::new(
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
    ))?;
    assert_eq!(construct.kind, QueryResultKind::Graph);
    assert_eq!(construct.media_type, "application/n-triples");
    let graph_text = String::from_utf8(construct.payload)?;
    assert!(graph_text.contains("<http://example.com/s>"));

    Ok(())
}

#[test]
fn preload_minimal_ttl_fixture_supports_real_query_surface()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let config = StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused-in-memory"),
        preload_ontology: true,
        ontology_path: Some(minimal_fixture_path()),
        ontology_fallbacks: Vec::new(),
    };

    let service = StoreService::new(config)?;
    let stats = service.stats()?;
    assert_eq!(stats.quad_count, 8);

    let ask = service.execute_query_str(
        "ASK WHERE { <http://example.com/alice> <http://example.com/likes> <http://example.com/spec> }",
    )?;
    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    let construct = service.execute_query_str("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }")?;
    assert_eq!(construct.kind, QueryResultKind::Graph);
    let graph_text = String::from_utf8(construct.payload)?;
    assert!(graph_text.contains("http://example.com/alice"));

    Ok(())
}

#[test]
fn ontology_preload_requires_existing_file() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let missing_path: PathBuf = temp.path().join("missing.ttl");

    let config = StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(missing_path),
        ontology_fallbacks: Vec::new(),
    };

    match StoreService::new(config) {
        Ok(_) => Err("expected ontology preload to fail".into()),
        Err(StoreError::OntologyFileNotFound { .. }) => Ok(()),
        Err(other) => Err(format!("unexpected error: {other}").into()),
    }
}

#[test]
fn sparql_update_inserts_data() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let ontology_path = temp.path().join("test_ontology.ttl");
    fs::write(&ontology_path, test_ontology())?;

    let config = StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(ontology_path),
        ontology_fallbacks: Vec::new(),
    };

    let service = StoreService::new(config)?;
    let report = service.execute_update(&SparqlUpdateRequest::new(
        "INSERT DATA { <http://example.com/a> <http://example.com/p> <http://example.com/b> }",
    ))?;
    assert!(report.applied);
    assert_eq!(report.revision, 2);
    assert_eq!(service.current_revision(), 2);

    let ask = service.execute_query_str(
        "ASK WHERE { <http://example.com/a> <http://example.com/p> <http://example.com/b> }",
    )?;
    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}

#[test]
fn graph_store_named_graph_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let config = StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    };
    let service = StoreService::new(config)?;
    let named_graph = GraphTarget::NamedGraph("http://example.com/graph/one".to_owned());

    let write_report = service.execute_graph_write(&GraphWriteRequest {
        target: named_graph.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:s ex:p ex:o .".to_vec(),
        replace: true,
    })?;
    assert!(write_report.modified);
    assert!(write_report.created);
    assert_eq!(write_report.revision, 1);

    let read = service.execute_graph_read(&GraphReadRequest {
        target: named_graph.clone(),
        format: nrese_store::GraphResultFormat::NTriples,
    })?;
    let read_body = String::from_utf8(read.payload)?;
    assert!(read_body.contains("http://example.com/s"));

    let delete_report = service.execute_graph_delete(&named_graph)?;
    assert!(delete_report.modified);
    assert_eq!(delete_report.revision, 2);
    let read_after_delete = service.execute_graph_read(&GraphReadRequest {
        target: named_graph,
        format: nrese_store::GraphResultFormat::NTriples,
    })?;
    assert!(
        String::from_utf8(read_after_delete.payload)?
            .trim()
            .is_empty()
    );

    Ok(())
}

#[test]
fn revision_increments_monotonically_across_mutations() -> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    assert_eq!(service.current_revision(), 0);

    let update_report = service.execute_update(&SparqlUpdateRequest::new(
        "INSERT DATA { <http://example.com/a> <http://example.com/p> <http://example.com/b> }",
    ))?;
    assert_eq!(update_report.revision, 1);
    assert_eq!(service.current_revision(), 1);

    let write_report = service.execute_graph_write(&GraphWriteRequest {
        target: GraphTarget::DefaultGraph,
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:s ex:p ex:o .".to_vec(),
        replace: true,
    })?;
    assert_eq!(write_report.revision, 2);
    assert_eq!(service.current_revision(), 2);

    let delete_report = service.execute_graph_delete(&GraphTarget::DefaultGraph)?;
    assert_eq!(delete_report.revision, 3);
    assert_eq!(service.current_revision(), 3);

    Ok(())
}

#[test]
fn graph_write_replace_true_replaces_only_target_graph() -> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    let named_graph = GraphTarget::NamedGraph("http://example.com/graph/replace".to_owned());

    service.execute_graph_write(&GraphWriteRequest {
        target: named_graph.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:old ex:p ex:one .".to_vec(),
        replace: true,
    })?;
    service.execute_graph_write(&GraphWriteRequest {
        target: named_graph.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:new ex:p ex:two .".to_vec(),
        replace: true,
    })?;
    service.execute_graph_write(&GraphWriteRequest {
        target: GraphTarget::DefaultGraph,
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:default ex:p ex:v .".to_vec(),
        replace: true,
    })?;

    let named_read = service.execute_graph_read(&GraphReadRequest {
        target: named_graph,
        format: nrese_store::GraphResultFormat::NTriples,
    })?;
    let named_text = String::from_utf8(named_read.payload)?;
    assert!(named_text.contains("http://example.com/new"));
    assert!(!named_text.contains("http://example.com/old"));
    assert!(!named_text.contains("http://example.com/default"));

    Ok(())
}

#[test]
fn graph_write_reports_existing_named_graph_replacements_as_not_created()
-> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    let named_graph = GraphTarget::NamedGraph("http://example.com/graph/existing".to_owned());

    let first = service.execute_graph_write(&GraphWriteRequest {
        target: named_graph.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:first ex:p ex:one .".to_vec(),
        replace: true,
    })?;
    let second = service.execute_graph_write(&GraphWriteRequest {
        target: named_graph,
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:second ex:p ex:two .".to_vec(),
        replace: true,
    })?;

    assert!(first.created);
    assert!(!second.created);

    Ok(())
}

#[test]
fn graph_write_accepts_relative_iris_when_base_iri_is_provided()
-> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;

    service.execute_graph_write(&GraphWriteRequest {
        target: GraphTarget::DefaultGraph,
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: Some("https://www.w3.org/ns/prov.ttl".to_owned()),
        payload: b"@prefix : <#> . :generated <http://www.w3.org/2002/07/owl#inverseOf> :wasGeneratedBy ."
            .to_vec(),
        replace: true,
    })?;

    let ask = service.execute_query_str(
        "PREFIX owl: <http://www.w3.org/2002/07/owl#>
         ASK WHERE {
           <https://www.w3.org/ns/prov.ttl#generated> owl:inverseOf <https://www.w3.org/ns/prov.ttl#wasGeneratedBy>
         }",
    )?;
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}

#[test]
fn graph_write_replace_false_appends_to_existing_graph() -> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    let named_graph = GraphTarget::NamedGraph("http://example.com/graph/append".to_owned());

    service.execute_graph_write(&GraphWriteRequest {
        target: named_graph.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:a ex:p ex:one .".to_vec(),
        replace: true,
    })?;
    service.execute_graph_write(&GraphWriteRequest {
        target: named_graph.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:b ex:p ex:two .".to_vec(),
        replace: false,
    })?;

    let read = service.execute_graph_read(&GraphReadRequest {
        target: named_graph,
        format: nrese_store::GraphResultFormat::NTriples,
    })?;
    let text = String::from_utf8(read.payload)?;
    assert!(text.contains("http://example.com/a"));
    assert!(text.contains("http://example.com/b"));

    Ok(())
}

#[test]
fn graph_delete_only_affects_target_named_graph() -> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    let graph_one = GraphTarget::NamedGraph("http://example.com/graph/one".to_owned());
    let graph_two = GraphTarget::NamedGraph("http://example.com/graph/two".to_owned());

    service.execute_graph_write(&GraphWriteRequest {
        target: graph_one.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:g1 ex:p ex:o .".to_vec(),
        replace: true,
    })?;
    service.execute_graph_write(&GraphWriteRequest {
        target: graph_two.clone(),
        format: nrese_store::GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:g2 ex:p ex:o .".to_vec(),
        replace: true,
    })?;

    service.execute_graph_delete(&graph_one)?;

    let one_after = service.execute_graph_read(&GraphReadRequest {
        target: graph_one,
        format: nrese_store::GraphResultFormat::NTriples,
    })?;
    let two_after = service.execute_graph_read(&GraphReadRequest {
        target: graph_two,
        format: nrese_store::GraphResultFormat::NTriples,
    })?;
    assert!(String::from_utf8(one_after.payload)?.trim().is_empty());
    assert!(String::from_utf8(two_after.payload)?.contains("http://example.com/g2"));

    Ok(())
}

#[test]
fn dataset_snapshot_captures_revision_and_supported_triples()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let config = StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    };
    let service = StoreService::new(config)?;
    service.execute_update(&SparqlUpdateRequest::new(
        "INSERT DATA {
            <http://example.com/s> <http://example.com/p> <http://example.com/o> .
            <http://example.com/s> <http://example.com/p2> \"literal\" .
        }",
    ))?;

    let snapshot = service.dataset_snapshot()?;
    let triples: Vec<_> = nrese_core::TripleSource::triples(&snapshot).collect();

    assert_eq!(snapshot.revision(), 1);
    assert_eq!(snapshot.asserted_triple_count(), 2);
    assert_eq!(snapshot.unsupported_triple_count(), 1);
    assert_eq!(triples.len(), 1);

    Ok(())
}

#[test]
fn dataset_snapshot_preserves_count_invariants_for_supported_and_unsupported_terms()
-> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    service.execute_update(&SparqlUpdateRequest::new(
        "INSERT DATA {
            <http://example.com/s1> <http://example.com/p1> <http://example.com/o1> .
            <http://example.com/s1> <http://example.com/p2> \"literal\" .
            _:b0 <http://example.com/p3> <http://example.com/o3> .
            <http://example.com/s1> <http://example.com/p4> _:b1 .
        }",
    ))?;

    let snapshot = service.dataset_snapshot()?;
    let triples: Vec<_> = nrese_core::TripleSource::triples(&snapshot).collect();

    assert_eq!(snapshot.revision(), service.current_revision());
    assert_eq!(snapshot.asserted_triple_count(), 4);
    assert_eq!(snapshot.unsupported_triple_count(), 3);
    assert_eq!(triples.len(), 1);
    assert_eq!(triples[0].subject.as_str(), "http://example.com/s1");
    assert_eq!(triples[0].predicate.as_str(), "http://example.com/p1");
    assert_eq!(triples[0].object.as_str(), "http://example.com/o1");

    Ok(())
}

#[cfg(not(feature = "durable-storage"))]
#[test]
fn on_disk_mode_requires_durable_storage_feature() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let config = StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir: temp.path().join("nrese-store-data"),
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    };

    match StoreService::new(config) {
        Ok(_) => Err("expected on-disk mode to fail without durable-storage feature".into()),
        Err(StoreError::DurableStorageFeatureDisabled) => Ok(()),
        Err(other) => Err(format!("unexpected error: {other}").into()),
    }
}

#[cfg(feature = "durable-storage")]
#[test]
fn on_disk_mode_persists_data_across_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let data_dir = temp.path().join("nrese-store-data");
    let config = StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir: data_dir.clone(),
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    };

    {
        let service = StoreService::new(config.clone())?;
        let report = service.execute_update(&SparqlUpdateRequest::new(
            "INSERT DATA { <http://example.com/persist/s> <http://example.com/persist/p> <http://example.com/persist/o> }",
        ))?;
        assert!(report.applied);
    }

    let reopened = StoreService::new(config)?;
    let ask = reopened.execute_query_str(
        "ASK WHERE { <http://example.com/persist/s> <http://example.com/persist/p> <http://example.com/persist/o> }",
    )?;
    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}
