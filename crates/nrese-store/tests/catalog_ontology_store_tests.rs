mod support;

use nrese_store::{
    GraphReadRequest, GraphResultFormat, GraphTarget, GraphWriteRequest, QueryResultKind,
    StoreConfig, StoreMode, StoreService,
};
use support::{catalog_fixture_path, in_memory_store_config};
use tempfile::tempdir;

#[test]
fn store_preload_accepts_official_foaf_rdf_xml_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let service = StoreService::new(StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(catalog_fixture_path("foaf.rdf")),
        ontology_fallbacks: Vec::new(),
    })?;

    let ask = service.execute_query_str(
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         ASK WHERE {
           <http://xmlns.com/foaf/0.1/Person> rdfs:subClassOf <http://xmlns.com/foaf/0.1/Agent>
         }",
    )?;

    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));
    Ok(())
}

#[test]
fn store_preload_accepts_official_prov_turtle_fixture_with_relative_base_iri()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let service = StoreService::new(StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: true,
        ontology_path: Some(catalog_fixture_path("prov.ttl")),
        ontology_fallbacks: Vec::new(),
    })?;

    let ask = service.execute_query_str(
        "PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX prov: <http://www.w3.org/ns/prov#>
         ASK WHERE {
           prov:generated owl:inverseOf prov:wasGeneratedBy
         }",
    )?;

    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));
    Ok(())
}

#[test]
fn store_graph_roundtrip_accepts_official_vcard_turtle_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let mut config = in_memory_store_config();
    config.data_dir = temp.path().join("unused");
    let service = StoreService::new(config)?;

    let payload = std::fs::read(catalog_fixture_path("vcard.ttl"))?;
    let target = GraphTarget::NamedGraph("http://example.com/catalog/vcard".to_owned());
    service.execute_graph_write(&GraphWriteRequest {
        target: target.clone(),
        format: GraphResultFormat::Turtle,
        payload,
        replace: true,
    })?;

    let graph = service.execute_graph_read(&GraphReadRequest {
        target,
        format: GraphResultFormat::NTriples,
    })?;
    let text = String::from_utf8(graph.payload)?;
    assert!(text.contains("http://www.w3.org/2006/vcard/ns#Individual"));
    Ok(())
}
