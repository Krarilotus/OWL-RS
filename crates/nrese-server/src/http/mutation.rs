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

fn map_error(policy: &PolicyConfig, error: MutationError, timeout_message: &str) -> ApiError {
    match error {
        MutationError::Store { kind, source } => match kind {
            MutationKind::Update => policy.bad_request_for_sparql_parse_error(source.to_string()),
            MutationKind::Tell | MutationKind::GraphWrite | MutationKind::GraphDelete => {
                ApiError::bad_request(source.to_string())
            }
            MutationKind::Restore => match source {
                StoreError::RdfParse(_) => ApiError::bad_request(source.to_string()),
                other => ApiError::internal(other.to_string()),
            },
        },
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
