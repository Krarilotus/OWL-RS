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
    /// The shapes graph is ill-formed; one message per problem.
    #[error("ill-formed SHACL shapes: {}", .0.join("; "))]
    ShaclShapes(Vec<String>),
    #[error("RDF parse error in {}: {source}", path.display())]
    FileParse {
        path: PathBuf,
        source: RdfParseError,
    },
    #[error("configured ontology file does not exist: {}", path.display())]
    OntologyFileNotFound { path: PathBuf },
    /// A materialisation was stopped; the previous inferred stack stands.
    #[error("materialisation cancelled")]
    MaterialisationCancelled,
}

impl StoreError {
    /// True if the request is at fault (syntax, invalid IRIs or payloads, unsupported query
    /// features) rather than the store. Transports map these to client errors. A cancelled
    /// evaluation is neither: the transport decides what cancelled it.
    /// The query needed more memory than its limit
    /// ([`SparqlQueryRequest::memory_limit`](crate::SparqlQueryRequest::memory_limit)).
    pub fn is_memory_limit(&self) -> bool {
        matches!(
            self,
            Self::SparqlEvaluation(QueryEvaluationError::Dataset(error))
                if error.downcast_ref::<nrese_sparql::BudgetExceeded>().is_some()
        )
    }

    /// The query needed memory that other running queries hold: the server's budget for
    /// all queries ([`StoreConfig::total_query_memory_bytes`](crate::StoreConfig)) is
    /// used up. The same query may succeed later.
    pub fn is_server_memory_limit(&self) -> bool {
        matches!(
            self,
            Self::SparqlEvaluation(QueryEvaluationError::Dataset(error))
                if error
                    .downcast_ref::<nrese_sparql::BudgetExceeded>()
                    .is_some_and(|exceeded| exceeded.shared)
        )
    }

    pub fn is_request_error(&self) -> bool {
        match self {
            Self::SparqlSyntax(_)
            | Self::SparqlUpdate(_)
            | Self::InvalidGraphIri(_)
            | Self::RdfParse(_)
            | Self::ShaclShapes(_)
            | Self::FileParse { .. } => true,
            Self::SparqlEvaluation(error) => !matches!(
                error,
                QueryEvaluationError::Dataset(_)
                    | QueryEvaluationError::Unexpected(_)
                    | QueryEvaluationError::Cancelled
            ),
            Self::Configuration(_)
            | Self::Io(_)
            | Self::Engine(_)
            | Self::OntologyFileNotFound { .. }
            | Self::MaterialisationCancelled => false,
        }
    }
}
