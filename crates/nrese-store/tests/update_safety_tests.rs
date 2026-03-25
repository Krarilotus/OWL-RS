use nrese_store::{SparqlUpdateRequest, StoreConfig, StoreService};
use tempfile::tempdir;

fn new_in_memory_service() -> Result<StoreService, Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    Ok(StoreService::new(StoreConfig {
        data_dir: temp.path().join("unused"),
        ..StoreConfig::default()
    })?)
}

#[test]
fn failed_live_update_does_not_change_revision_or_dataset() -> Result<(), Box<dyn std::error::Error>>
{
    let service = new_in_memory_service()?;
    service.execute_update_str(
        "INSERT DATA { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;
    let revision_before = service.current_revision();

    let result = service.execute_update(&SparqlUpdateRequest::new("INSERT DATA {"));
    assert!(result.is_err());
    assert_eq!(service.current_revision(), revision_before);

    let ask = service.execute_query_str(
        "ASK WHERE { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
    )?;
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}

#[test]
fn failed_preview_update_does_not_change_revision_or_dataset()
-> Result<(), Box<dyn std::error::Error>> {
    let service = new_in_memory_service()?;
    service.execute_update_str(
        "INSERT DATA { <http://example.com/staged-s> <http://example.com/p> <http://example.com/staged-o> }",
    )?;
    let revision_before = service.current_revision();

    let result = service.preview_update(&SparqlUpdateRequest::new("INSERT DATA {"));
    assert!(result.is_err());
    assert_eq!(service.current_revision(), revision_before);

    let ask = service.execute_query_str(
        "ASK WHERE { <http://example.com/staged-s> <http://example.com/p> <http://example.com/staged-o> }",
    )?;
    assert!(String::from_utf8(ask.payload)?.contains("true"));

    Ok(())
}
