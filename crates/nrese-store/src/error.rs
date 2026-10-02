use std::path::PathBuf;

use nrese_engine::EngineError;
use nrese_rdf_io::RdfParseError;
use nrese_sparql::{QueryEvaluationError, UpdateError};
use nrese_sparql_syntax::SparqlSyntaxError;
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
    /// A bulk load was stopped ([`crate::LoadProgress::cancel`]); nothing of it was
    /// committed.
    #[error("the load was cancelled; nothing of it was committed")]
    LoadCancelled,
    /// Graph-level access control refused the request; nothing of it is applied.
    #[error(transparent)]
    Forbidden(#[from] Refusal),
}

/// Why graph-level access control refused a request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    /// The request would change `graph`, which its requester may not write.
    #[error(
        "the request would change {}, which the requester may not write",
        nrese_sparql::graph_label(.0)
    )]
    Write(nrese_rdf::GraphName),
    /// The operation reads every graph, and the requester may read only some.
    #[error("{0} reads every graph, and the requester may read only some")]
    ReadAll(String),
    /// The operation changes every graph, and the requester may change only some.
    #[error("{0} changes every graph, and the requester may change only some")]
    WriteAll(String),
}

impl StoreError {
    /// The query needed more memory than its limit
    /// ([`SparqlQueryRequest::memory_limit`](crate::SparqlQueryRequest::memory_limit)).
    pub fn is_memory_limit(&self) -> bool {
        matches!(
            self,
            Self::SparqlEvaluation(QueryEvaluationError::MemoryLimit(_))
        )
    }

    /// The query needed memory that other running queries hold: the server's budget for
    /// all queries ([`StoreConfig::total_query_memory_bytes`](crate::StoreConfig)) is
    /// used up. The same query may succeed later.
    pub fn is_server_memory_limit(&self) -> bool {
        matches!(
            self,
            Self::SparqlEvaluation(QueryEvaluationError::MemoryLimit(exceeded))
                if exceeded.shared
        )
    }

    /// True if the request is at fault (syntax, invalid IRIs or payloads, unsupported query
    /// features) rather than the store. Transports map these to client errors. A cancelled
    /// evaluation is neither: the transport decides what cancelled it.
    pub fn is_request_error(&self) -> bool {
        match self {
            Self::SparqlSyntax(_)
            | Self::SparqlUpdate(_)
            | Self::InvalidGraphIri(_)
            | Self::RdfParse(_)
            | Self::ShaclShapes(_)
            | Self::Forbidden(_)
            | Self::FileParse { .. } => true,
            Self::SparqlEvaluation(error) => !matches!(
                error,
                QueryEvaluationError::Dataset(_)
                    | QueryEvaluationError::MemoryLimit(_)
                    | QueryEvaluationError::Unexpected(_)
                    | QueryEvaluationError::Cancelled
            ),
            Self::Configuration(_)
            | Self::Io(_)
            | Self::Engine(_)
            | Self::OntologyFileNotFound { .. }
            | Self::MaterialisationCancelled
            | Self::LoadCancelled => false,
        }
    }
}
