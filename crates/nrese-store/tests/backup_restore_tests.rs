use nrese_store::{
    DatasetBackupFormat, DatasetRestoreRequest, GraphReadRequest, GraphResultFormat, GraphTarget,
    GraphWriteRequest, QueryResultKind, SparqlUpdateRequest, StoreConfig, StoreMode, StoreService,
};
use tempfile::tempdir;

#[cfg(feature = "durable-storage")]
use std::path::PathBuf;

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

#[cfg(feature = "durable-storage")]
fn new_on_disk_service(data_dir: PathBuf) -> Result<StoreService, Box<dyn std::error::Error>> {
    Ok(StoreService::new(StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir,
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    })?)
}

#[test]
fn export_snapshot_serializes_default_and_named_graphs() -> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    service.execute_update_str(
        "INSERT DATA { <http://example.com/default-s> <http://example.com/p> <http://example.com/default-o> }",
    )?;
    service.execute_graph_write(&GraphWriteRequest {
        target: GraphTarget::NamedGraph("http://example.com/graph/backup".to_owned()),
        format: GraphResultFormat::Turtle,
        payload: b"@prefix ex: <http://example.com/> . ex:named-s ex:p ex:named-o .".to_vec(),
        replace: true,
    })?;

    let artifact = service.export_dataset(DatasetBackupFormat::NQuads)?;
    let body = String::from_utf8(artifact.payload.clone())?;

    assert_eq!(artifact.media_type, "application/n-quads");
    assert_eq!(artifact.source_revision, service.current_revision());
    assert!(body.contains("http://example.com/default-s"));
    assert!(body.contains("http://example.com/graph/backup"));
    assert_eq!(artifact.checksum_sha256.len(), 64);

    Ok(())
}

#[test]
fn restore_replaces_existing_dataset_and_resets_contents_to_backup()
-> Result<(), Box<dyn std::error::Error>> {
    let source = new_in_memory_service()?;
    source.execute_update_str(
        "INSERT DATA { <http://example.com/source-s> <http://example.com/p> <http://example.com/source-o> }",
    )?;
    source.execute_graph_write(&GraphWriteRequest {
        target: GraphTarget::NamedGraph("http://example.com/graph/source".to_owned()),
        format: GraphResultFormat::Turtle,
        payload: b"@prefix ex: <http://example.com/> . ex:source-named ex:p ex:value .".to_vec(),
        replace: true,
    })?;
    let artifact = source.export_dataset(DatasetBackupFormat::NQuads)?;

    let target = new_in_memory_service()?;
    target.execute_update_str(
        "INSERT DATA { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;

    let report = target.restore_dataset(&DatasetRestoreRequest {
        format: DatasetBackupFormat::NQuads,
        payload: artifact.payload,
    })?;

    assert!(report.replaced_existing);
    assert_eq!(report.revision, target.current_revision());

    let live_ask = target.execute_query_str(
        "ASK WHERE { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;
    let source_ask = target.execute_query_str(
        "ASK WHERE { <http://example.com/source-s> <http://example.com/p> <http://example.com/source-o> }",
    )?;

    assert!(String::from_utf8(live_ask.payload)?.contains("false"));
    assert!(String::from_utf8(source_ask.payload)?.contains("true"));

    let named_graph = target.execute_graph_read(&GraphReadRequest {
        target: GraphTarget::NamedGraph("http://example.com/graph/source".to_owned()),
        format: GraphResultFormat::NTriples,
    })?;
    assert!(String::from_utf8(named_graph.payload)?.contains("http://example.com/source-named"));

    Ok(())
}

#[test]
fn restore_increments_revision_once_for_successful_import() -> Result<(), Box<dyn std::error::Error>>
{
    let source = new_in_memory_service()?;
    source.execute_update_str(
        "INSERT DATA { <http://example.com/a> <http://example.com/p> <http://example.com/b> }",
    )?;
    let artifact = source.export_dataset(DatasetBackupFormat::NQuads)?;

    let target = new_in_memory_service()?;
    assert_eq!(target.current_revision(), 0);

    let report = target.restore_dataset(&DatasetRestoreRequest {
        format: DatasetBackupFormat::NQuads,
        payload: artifact.payload,
    })?;

    assert_eq!(report.revision, 1);
    assert_eq!(target.current_revision(), 1);

    Ok(())
}

#[test]
fn restore_rejects_invalid_dataset_payload_without_partial_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    service.execute_update_str(
        "INSERT DATA { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;
    let revision_before = service.current_revision();

    let result = service.restore_dataset(&DatasetRestoreRequest {
        format: DatasetBackupFormat::NQuads,
        payload: b"this is not n-quads".to_vec(),
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

#[test]
fn backup_restore_roundtrip_preserves_query_results() -> Result<(), Box<dyn std::error::Error>> {
    let source = new_in_memory_service()?;
    source.execute_update(&SparqlUpdateRequest::new(
        "INSERT DATA {
            <http://example.com/alice> <http://example.com/likes> <http://example.com/spec> .
            <http://example.com/bob> <http://example.com/likes> <http://example.com/spec> .
        }",
    ))?;
    let artifact = source.export_dataset(DatasetBackupFormat::NQuads)?;

    let restored = new_in_memory_service()?;
    restored.restore_dataset(&DatasetRestoreRequest {
        format: DatasetBackupFormat::NQuads,
        payload: artifact.payload,
    })?;

    let ask = restored.execute_query_str(
        "ASK WHERE { <http://example.com/alice> <http://example.com/likes> <http://example.com/spec> }",
    )?;
    let select = restored.execute_query_str(
        "SELECT ?s WHERE { ?s <http://example.com/likes> <http://example.com/spec> } ORDER BY ?s",
    )?;

    assert!(String::from_utf8(ask.payload)?.contains("true"));
    let select_text = String::from_utf8(select.payload)?;
    assert!(select_text.contains("http://example.com/alice"));
    assert!(select_text.contains("http://example.com/bob"));

    Ok(())
}

#[cfg(feature = "durable-storage")]
#[test]
fn on_disk_restore_then_reopen_preserves_restored_dataset() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let source = new_in_memory_service()?;
    source.execute_update_str(
        "INSERT DATA { <http://example.com/persist-s> <http://example.com/p> <http://example.com/persist-o> }",
    )?;
    let artifact = source.export_dataset(DatasetBackupFormat::NQuads)?;

    let data_dir = temp.path().join("restore-target");
    {
        let target = new_on_disk_service(data_dir.clone())?;
        target.restore_dataset(&DatasetRestoreRequest {
            format: DatasetBackupFormat::NQuads,
            payload: artifact.payload.clone(),
        })?;
    }

    let reopened = new_on_disk_service(data_dir)?;
    let ask = reopened.execute_query_str(
        "ASK WHERE { <http://example.com/persist-s> <http://example.com/p> <http://example.com/persist-o> }",
    )?;
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}
