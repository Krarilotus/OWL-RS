use std::path::PathBuf;

use oxigraph::io::RdfParseError;
use oxigraph::sparql::{QueryEvaluationError, SparqlSyntaxError, UpdateEvaluationError};
use oxigraph::store::{LoaderError, SerializerError, StorageError};
use thiserror::Error;

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("invalid store configuration: {0}")]
    Configuration(String),
    #[error("on-disk mode requires the `durable-storage` feature")]
    DurableStorageFeatureDisabled,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("loader error: {0}")]
    Loader(#[from] LoaderError),
    #[error("SPARQL syntax error: {0}")]
    SparqlSyntax(#[from] SparqlSyntaxError),
    #[error("SPARQL evaluation error: {0}")]
    SparqlEvaluation(#[from] QueryEvaluationError),
    #[error("SPARQL update evaluation error: {0}")]
    SparqlUpdateEvaluation(#[from] UpdateEvaluationError),
    #[error("invalid graph IRI: {0}")]
    InvalidGraphIri(String),
    #[error("RDF parse error: {0}")]
    RdfParse(#[from] RdfParseError),
    #[error("RDF serialization error: {0}")]
    RdfSerialize(#[from] SerializerError),
    #[error(
        "ontology preload is enabled but no ontology file was found. configured={configured:?}, searched={searched:?}"
    )]
    OntologyFileNotFound {
        configured: Option<PathBuf>,
        searched: Vec<PathBuf>,
    },
}
