//! http_lab's bulk load on this revision: the one call of the harness that differs between the
//! revisions it compares (main's copy is benches/suite/shims/f9b515e8/http_lab/load.rs). Loads
//! `files` into the default graph, adding to what is there. A statement with a syntax error
//! takes the store's own setting (`invalid: None`); http_lab's generated data has none.

pub(crate) fn bulk_load(
    store: &nrese_store::StoreService,
    files: Vec<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    store.bulk_load(&nrese_store::BulkLoadRequest {
        files,
        replace: false,
        graph: nrese_store::GraphTarget::DefaultGraph,
        invalid: None,
    })?;
    Ok(())
}
