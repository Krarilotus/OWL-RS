use std::path::PathBuf;

use nrese_engine::EngineError;
use nrese_sparql::{QueryEvaluationError, UpdateError};
use oxrdfio::RdfParseError;
use spargebra::SparqlSyntaxError;
use thiserror::Error;

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("invalid store configuration: {0}")]
    Configuration(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage engine error: {0}")]
    Engine(#[from] EngineError),
    #[error("SPARQL syntax error: {0}")]
    SparqlSyntax(#[from] SparqlSyntaxError),
    #[error("SPARQL evaluation error: {0}")]
    SparqlEvaluation(#[from] QueryEvaluationError),
    #[error("SPARQL update error: {0}")]
    SparqlUpdate(#[from] UpdateError),
    #[error("invalid graph IRI: {0}")]
    InvalidGraphIri(String),
    #[error("RDF parse error: {0}")]
    RdfParse(#[from] RdfParseError),
    #[error("RDF parse error in {}: {source}", path.display())]
    FileParse {
        path: PathBuf,
        source: RdfParseError,
    },
    #[error("configured ontology file does not exist: {}", path.display())]
    OntologyFileNotFound { path: PathBuf },
}
