use axum::Json;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
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
use crate::http::repository::Repository;
use crate::http::requests::{
    SparqlOperation, accept_header_value, operation_from_post, query_from_post, query_from_url,
    update_from_post,
};
use crate::http::responses::{StatusResponse, build_ready_response, build_version_response};
use crate::http::service_description::build_service_description;
use crate::http::shacl;
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
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Html<String>, ApiError> {
    guard::enforce_query_read(&state, &authenticated).await?;
    console::index()
}

/// The console's scripts, styles and runtime configuration: static files, served without
/// the access check of the page that loads them.
pub async fn console_file(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    console::file(&path)
}

pub async fn operator_ui(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Html<&'static str>, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    Ok(operator_ui::page())
}

pub async fn ai_status(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    ai::status(authenticated, State(state)).await
}

pub async fn ai_query_suggestions(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
    Json(request): Json<crate::ai::QuerySuggestionRequest>,
) -> Result<Response, ApiError> {
    ai::query_suggestions(authenticated, State(state), Json(request)).await
}

pub async fn operator_capabilities(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    Ok(operator_api::capabilities(state))
}

pub async fn operator_dataset_summary(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    operator_api::dataset_summary(state)
}

pub async fn operator_extended_health(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    operator_api::extended_health(state)
}

pub async fn operator_runtime_diagnostics(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    operator_diagnostics::runtime(state)
}

pub async fn operator_reasoning_diagnostics(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<Response, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    Ok(operator_diagnostics::reasoning(state))
}

pub async fn admin_backup_dataset(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &authenticated).await?;
    admin_dataset::backup(state).await
}

/// An image backup (`?repository=` another repository than the default).
pub async fn admin_image_backup(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &authenticated).await?;
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(raw_query.as_deref().unwrap_or_default())
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let repository = pairs
        .iter()
        .find(|(key, _)| key == "repository")
        .map_or(crate::repositories::DEFAULT_REPOSITORY, |(_, value)| {
            value.as_str()
        });
    admin_dataset::image_backup(state, repository).await
}

/// An image backup of the repository the path names, into `backups/` of the data
/// directory.
pub async fn repository_image_backup(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &authenticated).await?;
    let id = state.repository_id().to_owned();
    admin_dataset::image_backup(state, &id).await
}

pub async fn admin_restore_dataset(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    guard::enforce_admin_write(&state, &authenticated).await?;
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
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<Response, ApiError> {
    guard::enforce_service_description_read(&state, &authenticated).await?;
    Ok((StatusCode::OK, Json(build_ready_response(&state)?)).into_response())
}

pub async fn version(State(state): State<AppState>) -> Response {
    (StatusCode::OK, Json(build_version_response(&state))).into_response()
}

pub async fn metrics(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    guard::enforce_metrics_read(&state, &authenticated).await?;
    metrics::render(&state)
}

pub async fn service_description(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
) -> Result<Response, ApiError> {
    guard::enforce_service_description_read(&state, &authenticated).await?;
    let ttl = build_service_description(&state);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/turtle; charset=utf-8")],
        ttl,
    )
        .into_response())
}

pub async fn query_get(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let access = guard::query_access(&state, &authenticated).await?;
    let mut operation = query_from_url(raw_query.as_deref())?;
    operation.restrict(&access);
    sparql::execute_query(state, operation, accept_header_value(&headers)).await
}

pub async fn query_post(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let access = guard::query_access(&state, &authenticated).await?;
    let mut operation = query_from_post(
        raw_query.as_deref(),
        headers.get(header::CONTENT_TYPE),
        &body,
    )?;
    operation.restrict(&access);
    sparql::execute_query(state, operation, accept_header_value(&headers)).await
}

pub async fn update_post(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let access = guard::update_access(&state, &authenticated).await?;
    let operation = update_from_post(
        raw_query.as_deref(),
        headers.get(header::CONTENT_TYPE),
        &body,
    )?;
    sparql::execute_update(state, operation, access.requester()).await
}

/// `GET` on the combined SPARQL endpoint: a query, or without one the service description
/// (SPARQL 1.1 Service Description §2).
pub async fn sparql_get(
    authenticated: crate::auth::Authenticated,
    state: Repository,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let has_query = raw_query.as_deref().is_some_and(|raw| {
        serde_urlencoded::from_str::<Vec<(String, String)>>(raw)
            .is_ok_and(|pairs| pairs.iter().any(|(key, _)| key == "query"))
    });
    if has_query {
        query_get(authenticated, state, RawQuery(raw_query), headers).await
    } else {
        service_description(authenticated, state).await
    }
}

/// `POST` on the combined SPARQL endpoint: a query or an update, each under its own
/// access rule. Clients that are configured with one endpoint URL use this (RDF4J's
/// SPARQL repository as ResearchSpace sets it up; Fuseki- and Blazegraph-style setups).
pub async fn sparql_post(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let operation = operation_from_post(
        raw_query.as_deref(),
        headers.get(header::CONTENT_TYPE),
        &body,
    )?;
    match operation {
        SparqlOperation::Query(mut operation) => {
            operation.restrict(&guard::query_access(&state, &authenticated).await?);
            sparql::execute_query(state, operation, accept_header_value(&headers)).await
        }
        SparqlOperation::Update(operation) => {
            let access = guard::update_access(&state, &authenticated).await?;
            sparql::execute_update(state, operation, access.requester())
                .await
                .map(IntoResponse::into_response)
        }
    }
}

/// Resources whose labels' or local names' words begin with the words of `q`, best first
/// (`limit`, default 10; `infer=false` for asserted statements only).
pub async fn autocomplete(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ApiError> {
    let scope = guard::enforce_whole_read(&state, &authenticated).await?;
    state.ensure_serving()?;
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(raw_query.as_deref().unwrap_or_default())
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    let typed = value("q").unwrap_or_default().trim().to_owned();
    if typed.is_empty() {
        return Err(ApiError::bad_request("autocomplete needs a non-empty q"));
    }
    let limit = match value("limit") {
        None => 10,
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|&n| (1..=1000).contains(&n))
            .ok_or_else(|| ApiError::bad_request("limit must be between 1 and 1000"))?,
    };
    let infer = value("infer") != Some("false");
    let store = state.store();
    let suggestions =
        tokio::task::spawn_blocking(move || store.autocomplete(&scope, &typed, limit, infer))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
            .map_err(|error| ApiError::forbidden(error.to_string()))?;
    let suggestions: Vec<serde_json::Value> = suggestions
        .into_iter()
        .map(|s| serde_json::json!({"iri": s.iri, "label": s.label, "score": s.score}))
        .collect();
    Ok(Json(serde_json::json!({ "suggestions": suggestions })).into_response())
}

/// The OWL 2 EL class hierarchy of the asserted ontology.
pub async fn classification_get(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let scope = guard::enforce_whole_read(&state, &authenticated).await?;
    super::classification::classify(state, &headers, scope).await
}

/// Validates the data against the repository's shapes graph.
pub async fn shacl_get(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let scope = guard::enforce_whole_read(&state, &authenticated).await?;
    shacl::validate(state, raw_query, headers, None, scope).await
}

/// Validates the data against the shapes in the request body.
pub async fn shacl_post(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let scope = guard::enforce_whole_read(&state, &authenticated).await?;
    shacl::validate(state, raw_query, headers, Some(body), scope).await
}

pub async fn tell_post(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let access = guard::tell_access(&state, &authenticated).await?;
    tell::execute_tell(state, raw_query, headers, body, &access).await
}

pub async fn graph_get(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    graph_store::get_graph(state, authenticated, raw_query, headers).await
}

pub async fn graph_head(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    graph_store::head_graph(state, authenticated, raw_query, headers).await
}

pub async fn graph_put(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    graph_store::put_graph(state, authenticated, raw_query, headers, body).await
}

pub async fn graph_post(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    graph_store::post_graph(state, authenticated, raw_query, headers, body).await
}

pub async fn graph_delete(
    authenticated: crate::auth::Authenticated,
    Repository(state): Repository,
    raw_query: RawQuery,
) -> Result<StatusCode, ApiError> {
    graph_store::delete_graph(state, authenticated, raw_query).await
}
