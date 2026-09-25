use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;

use oxigraph::store::Store;
use tracing::info;

use crate::config::StoreConfig;
use crate::error::{StoreError, StoreResult};
use crate::query::GraphResultFormat;
use crate::rdf_io::{file_base_iri, parser_for_graph_format};

/// Loads the configured ontology file, if any. A configured but missing file is an error.
pub fn preload_ontology(store: &Store, config: &StoreConfig) -> StoreResult<Option<PathBuf>> {
    let Some(ontology_path) = config.ontology_path.clone() else {
        return Ok(None);
    };
    if !ontology_path.is_file() {
        return Err(StoreError::OntologyFileNotFound {
            path: ontology_path,
        });
    }
    let file = File::open(&ontology_path)?;
    let reader = BufReader::new(file);
    let ontology_format = infer_ontology_format(&ontology_path)?;
    let ontology_base_iri = file_base_iri(&ontology_path)?;
    let parser = parser_for_graph_format(ontology_format, Some(&ontology_base_iri))?;

    store.load_from_reader(parser, reader)?;
    info!(
        ontology_path = %ontology_path.display(),
        "ontology preloaded into store"
    );

    Ok(Some(ontology_path))
}

fn infer_ontology_format(path: &Path) -> StoreResult<GraphResultFormat> {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return Err(StoreError::Configuration(format!(
            "cannot infer ontology RDF format for {}",
            path.display()
        )));
    };

    GraphResultFormat::from_extension(extension).ok_or_else(|| {
        StoreError::Configuration(format!(
            "unsupported ontology RDF file extension '{}' for {}",
            extension,
            path.display()
        ))
    })
}
