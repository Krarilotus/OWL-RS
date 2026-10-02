//! Transport adapter for the store-owned mutation pipeline.
//!
//! Runs the pipeline on the blocking pool, enforces the policy timeout through a
//! [`MutationTicket`] and maps [`MutationError`] to HTTP. It contains no write semantics:
//! a timeout is only reported if the ticket guarantees the mutation was not committed.

use std::time::Duration;

use nrese_store::{
    MutationCommand, MutationCommitReport, MutationError, MutationKind, MutationTicket, StoreError,
};

use crate::error::ApiError;
use crate::policy::PolicyConfig;
use crate::state::AppState;

pub async fn run(
    state: &AppState,
    command: MutationCommand,
    timeout: Duration,
    timeout_message: &'static str,
) -> Result<MutationCommitReport, ApiError> {
    state.ensure_serving()?;
    let pipeline = state.pipeline();
    let policy = state.policy();
    let ticket = MutationTicket::new();
    let worker_ticket = ticket.clone();
    let mut task = tokio::task::spawn_blocking(move || pipeline.apply(command, &worker_ticket));

    let joined = match tokio::time::timeout(timeout, &mut task).await {
        Ok(joined) => joined,
        Err(_) if ticket.cancel() => return Err(ApiError::timeout(timeout_message)),
        // The commit already started; report its real outcome instead of a timeout.
        Err(_) => task.await,
    };
    joined
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(|error| map_error(&policy, error, timeout_message))
}

/// The HTTP meaning of a failed mutation: the request's fault (4xx) or the server's (5xx).
/// The store classifies its errors ([`StoreError::is_request_error`]); the transport only
/// maps the classes, so every mutation kind reports a storage failure the same way.
fn map_error(policy: &PolicyConfig, error: MutationError, timeout_message: &str) -> ApiError {
    match error {
        MutationError::Store { kind, source } => {
            if source.is_server_memory_limit() {
                ApiError::unavailable(format!(
                    "{source} (budgets.total_query_memory, NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES)"
                ))
            } else if source.is_memory_limit() {
                ApiError::payload_too_large(format!(
                    "{source} (budgets.query_memory, NRESE_MAX_QUERY_MEMORY_BYTES)"
                ))
            } else if !source.is_request_error() {
                ApiError::internal(format!("{} failed: {source}", kind_name(kind)))
            } else if matches!(source, StoreError::SparqlSyntax(_)) {
                policy.bad_request_for_sparql_parse_error(source.to_string())
            } else {
                ApiError::bad_request(source.to_string())
            }
        }
        MutationError::Rejected(reject) => {
            let reject = *reject;
            ApiError::reasoner_reject(reject.detail, reject.explanation, reject.attribution)
        }
        MutationError::Cancelled => ApiError::timeout(timeout_message.to_owned()),
        error @ (MutationError::Gate(_) | MutationError::Poisoned) => {
            ApiError::internal(error.to_string())
        }
    }
}

fn kind_name(kind: MutationKind) -> &'static str {
    match kind {
        MutationKind::Update => "update",
        MutationKind::Tell => "tell",
        MutationKind::GraphWrite => "graph write",
        MutationKind::GraphDelete => "graph delete",
        MutationKind::Restore => "restore",
        MutationKind::Statements => "statements",
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use nrese_store::{EngineError, MutationError, MutationKind, StoreError};

    use super::map_error;
    use crate::policy::PolicyConfig;

    fn status(error: MutationError) -> StatusCode {
        map_error(&PolicyConfig::default(), error, "timeout")
            .into_response()
            .status()
    }

    /// Every mutation kind: the request's faults are 4xx, storage faults 5xx, a
    /// cancellation a timeout.
    #[test]
    fn failures_keep_their_meaning_for_every_mutation_kind() {
        let kinds = [
            MutationKind::Update,
            MutationKind::Tell,
            MutationKind::GraphWrite,
            MutationKind::GraphDelete,
            MutationKind::Restore,
            MutationKind::Statements,
        ];
        for kind in kinds {
            let syntax = nrese_store::PreparedQuery::parse(&nrese_store::SparqlQueryRequest::new(
                "SELECT {",
            ))
            .unwrap_err();
            assert!(matches!(syntax, StoreError::SparqlSyntax(_)));
            let request = [syntax, StoreError::InvalidGraphIri("not an iri".to_owned())];
            for source in request {
                assert_eq!(
                    status(MutationError::Store { kind, source }),
                    StatusCode::BAD_REQUEST,
                    "{kind:?}"
                );
            }
            let engine = StoreError::Engine(EngineError::Corruption("injected".to_owned()));
            assert_eq!(
                status(MutationError::Store {
                    kind,
                    source: engine
                }),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{kind:?}"
            );
            let io = StoreError::Io(std::io::Error::other("disk full"));
            assert_eq!(
                status(MutationError::Store { kind, source: io }),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{kind:?}"
            );
        }
        assert_eq!(
            status(MutationError::Cancelled),
            StatusCode::REQUEST_TIMEOUT
        );
    }
}
