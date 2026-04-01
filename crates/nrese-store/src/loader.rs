use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;

use oxigraph::io::RdfParser;
use oxigraph::store::Store;
use tracing::info;

use crate::config::StoreConfig;
use crate::error::{StoreError, StoreResult};
use crate::query::GraphResultFormat;

pub fn preload_ontology(store: &Store, config: &StoreConfig) -> StoreResult<Option<PathBuf>> {
    if !config.preload_ontology {
        return Ok(None);
    }

    let ontology_path = resolve_ontology_path(config)?;
    let file = File::open(&ontology_path)?;
    let reader = BufReader::new(file);
    let parser = RdfParser::from_format(infer_ontology_format(&ontology_path)?.into_oxigraph());

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

fn resolve_ontology_path(config: &StoreConfig) -> StoreResult<PathBuf> {
    let candidates = config.ontology_candidates();
    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    Err(StoreError::OntologyFileNotFound {
        configured: config.ontology_path.clone(),
        searched: candidates,
    })
}
