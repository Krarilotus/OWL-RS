//! The result cache of a repository (`/api/v1/repositories/{id}/cache`): its counts, the
//! pinned queries, pinning and unpinning, clearing. The cache itself is the SPARQL
//! layer's (`nrese_sparql::cache`); these routes translate to the store's operations.

use axum::Json;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ApiError;
use crate::http::guard;
use crate::http::repository::Repository;

/// A pinned query.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct PinView {
    name: String,
    /// The query, as given.
    query: String,
    /// The revision its result is held for; absent if it isn't held now (the store
    /// changed: the result is pinned again the next time the query runs).
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<u64>,
    rows: usize,
    /// What it takes of the cache's budget.
    bytes: usize,
}

impl From<nrese_sparql::PinnedResult> for PinView {
    fn from(pin: nrese_sparql::PinnedResult) -> Self {
        Self {
            name: pin.name,
            query: pin.query,
            revision: pin.revision,
            rows: pin.rows,
            bytes: pin.bytes,
        }
    }
}

/// The result cache's counts and pins.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct CacheView {
    /// The budget in bytes (`budgets.result_cache`); 0: the cache is off.
    capacity: usize,
    bytes: usize,
    pinned_bytes: usize,
    entries: usize,
    /// Entries holding a whole answer's serialised bytes, in one format.
    answers: usize,
    /// Query parts answered from the cache.
    hits: u64,
    /// Query parts computed that could have been cached.
    misses: u64,
    /// Query parts taken from another query computing them at the same time.
    shared: u64,
    stored: u64,
    /// Parts not kept: cheaper to recompute than to keep, too large, or worth less than
    /// what they would have evicted.
    rejected: u64,
    evicted: u64,
    pins: Vec<PinView>,
}

#[utoipa::path(get, path = "/api/v1/repositories/{id}/cache", tag = "sparql",
    params(("id" = String, Path, description = "The repository's id")),
    responses((status = 200, description = "The result cache's counts and pinned queries", body = CacheView)))]
/// The result cache: its budget and use, hits and misses since the start, the pinned
/// queries (operators).
pub async fn cache_get(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    let store = state.store();
    let stats = store.query_cache_stats();
    Ok(Json(CacheView {
        capacity: stats.capacity,
        bytes: stats.bytes,
        pinned_bytes: stats.pinned_bytes,
        entries: stats.entries,
        answers: stats.answers,
        hits: stats.hits,
        misses: stats.misses,
        shared: stats.shared,
        stored: stats.stored,
        rejected: stats.rejected,
        evicted: stats.evicted,
        pins: store
            .pinned_queries()
            .into_iter()
            .map(PinView::from)
            .collect(),
    })
    .into_response())
}

#[utoipa::path(delete, path = "/api/v1/repositories/{id}/cache", tag = "sparql",
    params(("id" = String, Path, description = "The repository's id")),
    responses((status = 204, description = "Every result that isn't pinned dropped")))]
/// Drops every cached result that isn't pinned (administrators).
pub async fn cache_delete(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<StatusCode, ApiError> {
    guard::enforce_admin_write(&state, &authenticated).await?;
    state.store().clear_query_cache();
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/api/v1/repositories/{id}/cache/pins/{name}", tag = "sparql",
    params(("id" = String, Path, description = "The repository's id"), ("name" = String, Path, description = "The pin's name")),
    request_body(content = String, content_type = "application/sparql-query", description = "The query"),
    responses((status = 200, description = "Run and pinned: its result is kept while the store doesn't change, and pinned again when it runs after a change", body = PinView),
        (status = 400, description = "Not a query, a query that can't be cached (RAND, NOW, UUID, STRUUID, BNODE, SERVICE), a result larger than the budget leaves beside the other pins, or the cache is off", body = crate::http::openapi::Problem)))]
/// Runs the query in the body with the requester's access and pins its result under
/// `name`, replacing what `name` pinned before (administrators).
pub async fn pin_put(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    Path((_, name)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &authenticated).await?;
    let access = guard::query_access(&state, &authenticated).await?;
    let query = String::from_utf8(body.to_vec())
        .map_err(|_| ApiError::bad_request("the query is not UTF-8"))?;
    let policy = state.policy().clone();
    policy.enforce_query_bytes(query.len())?;
    let request =
        nrese_store::SparqlQueryRequest::new(query, nrese_store::ReadScope::of(access.read));
    let store = state.store();
    let cancellation = nrese_store::CancellationToken::new();
    let token = cancellation.clone();
    let pinning = tokio::task::spawn_blocking(move || store.pin_query(&name, &request, &token));
    let pinned = match tokio::time::timeout(policy.timeouts.query, pinning).await {
        Ok(joined) => joined.map_err(|error| ApiError::internal(error.to_string()))?,
        Err(_) => {
            cancellation.cancel();
            return Err(ApiError::timeout(
                "pinning ran the query past the policy timeout",
            ));
        }
    };
    let pinned = pinned.map_err(|error| super::sparql::map_query_error(&policy, error))?;
    Ok(Json(PinView::from(pinned)).into_response())
}

#[utoipa::path(delete, path = "/api/v1/repositories/{id}/cache/pins/{name}", tag = "sparql",
    params(("id" = String, Path, description = "The repository's id"), ("name" = String, Path, description = "The pin's name")),
    responses((status = 204, description = "Unpinned; the result stays cached like any other"),
        (status = 404, description = "Nothing is pinned under the name", body = crate::http::openapi::Problem)))]
/// Unpins `name`'s result (administrators).
pub async fn pin_delete(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    Path((_, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    guard::enforce_admin_write(&state, &authenticated).await?;
    match state.store().unpin_query(&name) {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found(format!(
            "nothing is pinned as '{name}'"
        ))),
    }
}
