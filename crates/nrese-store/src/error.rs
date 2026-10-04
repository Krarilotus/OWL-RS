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
    /// A long operation stopped because the process reached its memory limit
    /// ([`crate::StoreConfig::process_memory_bytes`]); nothing of it was applied.
    #[error(
        "stopped at the process's memory limit ({} MiB); nothing was applied",
        .limit / (1 << 20)
    )]
    ProcessMemoryLimit { limit: u64 },
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
    /// The evaluation error, of a query or of an update's pattern: both are classified
    /// alike (the review of 3 October 2026, A6).
    fn evaluation(&self) -> Option<&QueryEvaluationError> {
        match self {
            Self::SparqlEvaluation(error)
            | Self::SparqlUpdate(nrese_sparql::UpdateError::Evaluation(error)) => Some(error),
            _ => None,
        }
    }

    /// The query needed more memory than its limit
    /// ([`SparqlQueryRequest::memory_limit`](crate::SparqlQueryRequest::memory_limit)).
    pub fn is_memory_limit(&self) -> bool {
        matches!(
            self.evaluation(),
            Some(QueryEvaluationError::MemoryLimit(_))
        )
    }

    /// The query needed memory that other running queries hold: the server's budget for
    /// all queries ([`StoreConfig::total_query_memory_bytes`](crate::StoreConfig)) is
    /// used up. The same query may succeed later.
    pub fn is_server_memory_limit(&self) -> bool {
        matches!(
            self.evaluation(),
            Some(QueryEvaluationError::MemoryLimit(exceeded)) if exceeded.shared
        )
    }

    /// True if the request is at fault (syntax, invalid IRIs or payloads, unsupported query
    /// features) rather than the store. Transports map these to client errors. A cancelled
    /// evaluation is neither: the transport decides what cancelled it.
    pub fn is_request_error(&self) -> bool {
        if let Some(error) = self.evaluation() {
            return !matches!(
                error,
                QueryEvaluationError::Dataset(_)
                    | QueryEvaluationError::MemoryLimit(_)
                    | QueryEvaluationError::Unexpected(_)
                    | QueryEvaluationError::Cancelled
            );
        }
        match self {
            Self::SparqlUpdate(nrese_sparql::UpdateError::Cancelled) => false,
            Self::SparqlSyntax(_)
            | Self::SparqlUpdate(_)
            | Self::InvalidGraphIri(_)
            | Self::RdfParse(_)
            | Self::ShaclShapes(_)
            | Self::Forbidden(_)
            | Self::FileParse { .. } => true,
            Self::SparqlEvaluation(_) => unreachable!("classified above"),
            Self::Configuration(_)
            | Self::Io(_)
            | Self::Engine(_)
            | Self::OntologyFileNotFound { .. }
            | Self::MaterialisationCancelled
            | Self::ProcessMemoryLimit { .. }
            | Self::LoadCancelled => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use nrese_sparql::UpdateError;

    use super::*;

    #[test]
    fn an_update_s_evaluation_error_is_classified_as_a_query_s() {
        let errors: [fn() -> QueryEvaluationError; 3] = [
            || QueryEvaluationError::Unexpected("probe".into()),
            || QueryEvaluationError::Cancelled,
            || QueryEvaluationError::Unsupported("probe".into()),
        ];
        for error in errors {
            let query = StoreError::SparqlEvaluation(error());
            let update = StoreError::SparqlUpdate(UpdateError::Evaluation(error()));
            assert_eq!(
                query.is_request_error(),
                update.is_request_error(),
                "{query}"
            );
            assert_eq!(query.is_memory_limit(), update.is_memory_limit());
        }
        assert!(!StoreError::SparqlUpdate(UpdateError::Cancelled).is_request_error());
    }
}
