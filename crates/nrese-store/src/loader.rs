use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use nrese_engine::Engine;
use oxrdf::GraphName;
use tracing::info;

use crate::config::StoreConfig;
use crate::error::{StoreError, StoreResult};
use crate::query::GraphResultFormat;
use crate::rdf_io::{BlankNodes, file_base_iri, parse_graph};

/// Loads the configured ontology file into the default graph, if one is configured. A
/// configured but missing file is an error. Idempotent: the file's own blank-node labels
/// are kept, so loading it again after a restart adds nothing.
pub fn preload_ontology(engine: &Engine, config: &StoreConfig) -> StoreResult<Option<PathBuf>> {
    let Some(ontology_path) = config.ontology_path.clone() else {
        return Ok(None);
    };
    if !ontology_path.is_file() {
        return Err(StoreError::OntologyFileNotFound {
            path: ontology_path,
        });
    }
    let quads = parse_graph(
        infer_ontology_format(&ontology_path)?,
        Some(&file_base_iri(&ontology_path)?),
        BufReader::new(File::open(&ontology_path)?),
        GraphName::DefaultGraph,
        BlankNodes::AsWritten,
    )?;
    let mut tx = engine.transaction();
    for quad in &quads {
        tx.insert(quad.as_ref());
    }
    let summary = tx.commit()?;
    info!(
        ontology_path = %ontology_path.display(),
        added = summary.inserted,
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
