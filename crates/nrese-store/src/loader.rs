use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::store::Store;
use tracing::info;

use crate::config::StoreConfig;
use crate::error::{StoreError, StoreResult};

pub fn preload_ontology(store: &Store, config: &StoreConfig) -> StoreResult<Option<PathBuf>> {
    if !config.preload_ontology {
        return Ok(None);
    }

    let ontology_path = resolve_ontology_path(config)?;
    let file = File::open(&ontology_path)?;
    let reader = BufReader::new(file);
    let parser = RdfParser::from_format(RdfFormat::Turtle);

    store.load_from_reader(parser, reader)?;
    info!(
        ontology_path = %ontology_path.display(),
        "ontology preloaded into store"
    );

    Ok(Some(ontology_path))
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
