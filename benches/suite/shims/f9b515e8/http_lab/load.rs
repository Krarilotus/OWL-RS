//! http_lab's bulk load on main (f9b515e8), copied over crates/nrese-server/examples/http_lab/load.rs
//! when a comparison builds main: main's BulkLoadRequest has `skip_errors` where later revisions
//! have `invalid`. Loads `files` into the default graph, adding to what is there; a statement
//! with a syntax error stops the load (`skip_errors: false`); http_lab's generated data has none.

pub(crate) fn bulk_load(
    store: &nrese_store::StoreService,
    files: Vec<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    store.bulk_load(&nrese_store::BulkLoadRequest {
        files,
        replace: false,
        graph: nrese_store::GraphTarget::DefaultGraph,
        skip_errors: false,
    })?;
    Ok(())
}
