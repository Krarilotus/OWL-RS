use nrese_store::{
    GraphResultFormat, GraphTarget, GraphWriteRequest, QueryResultKind, SparqlUpdateRequest,
    StoreConfig, StoreMode, StoreService,
};
use tempfile::tempdir;

fn new_in_memory_service() -> Result<StoreService, Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    Ok(StoreService::new(StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: temp.path().join("unused"),
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    })?)
}

#[test]
fn failed_update_does_not_advance_revision_or_publish() -> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    service.execute_update(&SparqlUpdateRequest::new(
        "INSERT DATA { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    ))?;
    let revision_before = service.current_revision();

    let result = service.execute_update(&SparqlUpdateRequest::new("INSERT DATA {"));
    assert!(result.is_err());
    assert_eq!(service.current_revision(), revision_before);

    let ask = service.execute_query_str(
        "ASK WHERE { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;
    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}

#[test]
fn failed_graph_write_does_not_advance_revision_or_publish()
-> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    service.execute_update_str(
        "INSERT DATA { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;
    let revision_before = service.current_revision();

    let result = service.execute_graph_write(&GraphWriteRequest {
        target: GraphTarget::DefaultGraph,
        format: GraphResultFormat::Turtle,
        base_iri: None,
        payload: b"@prefix ex: <http://example.com/> . ex:broken ex:p ".to_vec(),
        replace: true,
    });

    assert!(result.is_err());
    assert_eq!(service.current_revision(), revision_before);

    let ask = service.execute_query_str(
        "ASK WHERE { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;
    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}
