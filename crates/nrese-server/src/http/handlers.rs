use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};

use crate::error::ApiError;
use crate::http::admin_dataset;
use crate::http::ai;
use crate::http::console;
use crate::http::graph_store;
use crate::http::guard;
use crate::http::metrics;
use crate::http::operator_api;
use crate::http::operator_diagnostics;
use crate::http::operator_ui;
use crate::http::requests::{QueryRequest, accept_header_value, extract_query, extract_update};
use crate::http::responses::{StatusResponse, build_ready_response, build_version_response};
use crate::http::service_description::build_service_description;
use crate::http::sparql;
use crate::http::tell;
use crate::state::AppState;

pub async fn healthz() -> Json<StatusResponse> {
    Json(StatusResponse { status: "ok" })
}

pub async fn root_redirect() -> Redirect {
    Redirect::temporary("/console")
}

pub async fn console_ui(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    console::index()
}

pub async fn operator_ui(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<&'static str>, ApiError> {
    guard::enforce_operator_read(&state, &headers).await?;
    Ok(operator_ui::page())
}

pub async fn ai_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    ai::status(State(state), headers).await
}

pub async fn ai_query_suggestions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<crate::ai::QuerySuggestionRequest>,
) -> Result<Response, ApiError> {
    ai::query_suggestions(State(state), headers, Json(request)).await
}

pub async fn operator_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &headers).await?;
    Ok(operator_api::capabilities(state))
}

pub async fn operator_dataset_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &headers).await?;
    operator_api::dataset_summary(state)
}

pub async fn operator_extended_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &headers).await?;
    operator_api::extended_health(state)
}

pub async fn operator_runtime_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &headers).await?;
    operator_diagnostics::runtime(state)
}

pub async fn operator_reasoning_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &headers).await?;
    Ok(operator_diagnostics::reasoning(state))
}

pub async fn admin_backup_dataset(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &headers).await?;
    admin_dataset::backup(state).await
}

pub async fn admin_restore_dataset(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &headers).await?;
    admin_dataset::restore(state, headers, body).await
}

pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match build_ready_response(&state) {
        Ok(response) if state.is_ready() => (StatusCode::OK, Json(response)).into_response(),
        Ok(response) => (StatusCode::SERVICE_UNAVAILABLE, Json(response)).into_response(),
        Err(error) => error.into_response(),
    }
}

pub async fn dataset_info(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_service_description_read(&state, &headers).await?;
    Ok((StatusCode::OK, Json(build_ready_response(&state)?)).into_response())
}

pub async fn version(State(state): State<AppState>) -> Response {
    (StatusCode::OK, Json(build_version_response(&state))).into_response()
}

pub async fn metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_metrics_read(&state, &headers).await?;
    metrics::render(&state)
}

pub async fn service_description(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_service_description_read(&state, &headers).await?;
    let ttl = build_service_description(&state);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/turtle; charset=utf-8")],
        ttl,
    )
        .into_response())
}

pub async fn query_get(
    State(state): State<AppState>,
    Query(request): Query<QueryRequest>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    sparql::execute_query(state, request.query, accept_header_value(&headers)).await
}

pub async fn query_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    guard::enforce_query_read(&state, &headers).await?;
    let query = extract_query(headers.get(header::CONTENT_TYPE), &body)?;
    sparql::execute_query(state, query, accept_header_value(&headers)).await
}

pub async fn update_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_update_write(&state, &headers).await?;
    let update = extract_update(headers.get(header::CONTENT_TYPE), &body)?;
    sparql::execute_update(state, update).await
}

pub async fn tell_post(
    State(state): State<AppState>,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    guard::enforce_tell_write(&state, &headers).await?;
    tell::execute_tell(state, raw_query, headers, body).await
}

pub async fn graph_get(
    State(state): State<AppState>,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    graph_store::get_graph(state, raw_query, headers).await
}

pub async fn graph_head(
    State(state): State<AppState>,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    graph_store::head_graph(state, raw_query, headers).await
}

pub async fn graph_put(
    State(state): State<AppState>,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    graph_store::put_graph(state, raw_query, headers, body).await
}

pub async fn graph_post(
    State(state): State<AppState>,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    graph_store::post_graph(state, raw_query, headers, body).await
}

pub async fn graph_delete(
    State(state): State<AppState>,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    graph_store::delete_graph(state, raw_query, headers).await
}
