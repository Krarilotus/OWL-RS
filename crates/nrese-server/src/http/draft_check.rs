//! Draft checks over HTTP: the Datamodel Workflow's backend-agnostic store-check
//! protocol (`dmw-store-check/1`, see `docs/integration/datamodel-workflow.md`).
//!
//! `GET /api/v1/draft-check/capabilities` declares what this server checks;
//! `POST /api/v1/draft-check` runs one SHACL, query or reasoning check over the inputs in
//! the request, in a throwaway in-memory store ([`nrese_store::run_draft_check`]). No
//! repository is read or written, so the permission asked is the one to query.
//!
//! Every check ends in a reply that echoes the request's identity (request hash, scope,
//! build, semantics, profile, input hashes, limits), so the caller can bind the answer to
//! what it asked. Only a malformed request is an HTTP error; a check that cannot run says
//! so in its terminal status.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use nrese_store::{
    CancellationToken, DRAFT_CHECK_PROFILES, DRAFT_CHECK_PROTOCOL, DRAFT_CHECK_SEMANTICS,
    DraftInputs, DraftLimits, DraftOperation, DraftOutcome, DraftStatus, input_hash_mismatch,
    run_draft_check,
};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::http::guard;
use crate::state::AppState;

/// This server's build as draft-check evidence names it: the version, the semantics,
/// and the source revision when the build was given one (`NRESE_BUILD_REVISION`).
pub(crate) fn build_id() -> String {
    let base = format!(
        "nrese-server {} {DRAFT_CHECK_SEMANTICS}",
        env!("CARGO_PKG_VERSION")
    );
    match option_env!("NRESE_BUILD_REVISION") {
        Some(revision) if !revision.trim().is_empty() => format!("{base} {}", revision.trim()),
        _ => base,
    }
}

/// What this server's draft checks offer.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DraftCapabilities {
    /// `dmw-store-check/1`.
    protocol: &'static str,
    /// `shacl`, `query`, `reasoning`.
    operations: Vec<&'static str>,
    profiles: Vec<&'static str>,
    /// What the checks mean; a change of meaning gets a new identifier.
    semantics: Vec<&'static str>,
    build_id: String,
    /// The longest time limit a request may ask for, in seconds.
    max_seconds: f64,
}

/// One check: its identity, its limits and its pinned inputs (N-Triples; the query as
/// SPARQL text).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DraftCheckRequest {
    protocol: String,
    /// `shacl`, `query` or `reasoning`.
    #[schema(value_type = String)]
    operation: DraftOperation,
    request_sha256: String,
    /// The caller's scope, echoed unchanged.
    scope: serde_json::Value,
    build_id: String,
    semantics_id: String,
    profile: String,
    /// SHA-256 of the inputs: `@active_data`, `@active_schema`, `@active_shapes` and
    /// `@query` are verified; others are echoed.
    input_hashes: BTreeMap<String, String>,
    #[schema(value_type = Object)]
    limits: DraftLimits,
    #[schema(value_type = Object)]
    inputs: DraftInputs,
}

/// The check's terminal answer with the request's identity.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DraftCheckReply {
    #[schema(value_type = String)]
    operation: DraftOperation,
    request_sha256: String,
    scope: serde_json::Value,
    build_id: String,
    semantics_id: String,
    profile: String,
    input_hashes: BTreeMap<String, String>,
    #[schema(value_type = Object)]
    effective_limits: DraftLimits,
    #[serde(flatten)]
    #[schema(value_type = Object)]
    outcome: DraftOutcome,
}

#[utoipa::path(get, path = "/api/v1/draft-check/capabilities", tag = "draft-check",
    responses((status = 200, description = "What the server's draft checks offer", body = DraftCapabilities)))]
/// The operations, profiles and semantics of draft checks, and this build.
pub async fn capabilities(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &authenticated).await?;
    Ok(Json(DraftCapabilities {
        protocol: DRAFT_CHECK_PROTOCOL,
        operations: vec!["shacl", "query", "reasoning"],
        profiles: DRAFT_CHECK_PROFILES.to_vec(),
        semantics: vec![DRAFT_CHECK_SEMANTICS],
        build_id: build_id(),
        max_seconds: state.policy().timeouts.query.as_secs_f64(),
    })
    .into_response())
}

#[utoipa::path(post, path = "/api/v1/draft-check", tag = "draft-check",
    request_body = DraftCheckRequest,
    responses(
        (status = 200, description = "The check's terminal answer, whatever it is", body = DraftCheckReply),
        (status = 400, description = "Not a draft-check request", body = crate::http::openapi::Problem),
        (status = 413, description = "The request is larger than the upload limit", body = crate::http::openapi::Problem)))]
/// Runs one isolated draft check over the inputs in the request.
pub async fn check(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &authenticated).await?;
    let policy = state.policy().clone();
    policy.enforce_rdf_upload_bytes(body.len())?;
    let request: DraftCheckRequest = serde_json::from_slice(&body)
        .map_err(|error| ApiError::bad_request(format!("not a draft-check request: {error}")))?;
    if request.protocol != DRAFT_CHECK_PROTOCOL {
        return Err(ApiError::bad_request(format!(
            "unknown protocol '{}', expected '{DRAFT_CHECK_PROTOCOL}'",
            request.protocol
        )));
    }
    let server_max = policy.timeouts.query;
    let outcome = match refusal(&request, server_max) {
        Some(outcome) => outcome,
        None => {
            let seconds = Duration::from_secs_f64(request.limits.max_seconds).min(server_max);
            let memory = policy.limits.max_query_memory_bytes;
            run(&request, seconds, (memory > 0).then_some(memory)).await
        }
    };
    Ok(Json(DraftCheckReply {
        operation: request.operation,
        request_sha256: request.request_sha256,
        scope: request.scope,
        build_id: request.build_id,
        semantics_id: request.semantics_id,
        profile: request.profile,
        input_hashes: request.input_hashes,
        effective_limits: request.limits,
        outcome,
    })
    .into_response())
}

/// Why this server can't answer the request as asked, as a terminal outcome.
fn refusal(request: &DraftCheckRequest, server_max: Duration) -> Option<DraftOutcome> {
    let limits = &request.limits;
    if request.build_id != build_id() {
        return Some(DraftOutcome::refused(
            DraftStatus::Failed,
            "the request names another build; read the capabilities again",
        ));
    }
    if request.semantics_id != DRAFT_CHECK_SEMANTICS {
        return Some(DraftOutcome::refused(
            DraftStatus::Unsupported,
            format!("unknown semantics '{}'", request.semantics_id),
        ));
    }
    if !DRAFT_CHECK_PROFILES.contains(&request.profile.as_str()) {
        return Some(DraftOutcome::refused(
            DraftStatus::Unsupported,
            format!("unknown profile '{}'", request.profile),
        ));
    }
    if !limits.max_seconds.is_finite() || limits.max_seconds <= 0.0 {
        return Some(DraftOutcome::refused(
            DraftStatus::Failed,
            "max_seconds must be positive",
        ));
    }
    if limits.max_seconds > server_max.as_secs_f64() {
        // The reply echoes the asked limits as effective, so it can't silently lower them.
        return Some(DraftOutcome::refused(
            DraftStatus::Unsupported,
            format!(
                "max_seconds {} exceeds this server's {}",
                limits.max_seconds,
                server_max.as_secs_f64()
            ),
        ));
    }
    input_hash_mismatch(&request.inputs, &request.input_hashes)
        .map(|problem| DraftOutcome::refused(DraftStatus::Failed, problem))
}

/// The check in a blocking task, cancelled at the time limit.
async fn run(
    request: &DraftCheckRequest,
    seconds: Duration,
    memory: Option<usize>,
) -> DraftOutcome {
    let cancellation = CancellationToken::new();
    let (operation, inputs, limits) = (request.operation, request.inputs.clone(), request.limits);
    let token = cancellation.clone();
    let task = tokio::task::spawn_blocking(move || {
        run_draft_check(operation, &inputs, &limits, &token, memory)
    });
    match tokio::time::timeout(seconds, task).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            DraftOutcome::refused(DraftStatus::Failed, format!("the check stopped: {error}"))
        }
        Err(_) => {
            cancellation.cancel();
            DraftOutcome::refused(DraftStatus::Timeout, "the check exceeded its time limit")
        }
    }
}
