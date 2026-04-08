#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Method, Request};
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_server::ai::AiSuggestionService;
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

use nrese_server::policy::PolicyConfig;
use nrese_server::{AppState, build_app};

pub fn test_app() -> Result<axum::Router, Box<dyn std::error::Error>> {
    test_app_with_settings(PolicyConfig::default(), ReasonerConfig::default())
}

pub fn test_app_with_policy(
    policy: PolicyConfig,
) -> Result<axum::Router, Box<dyn std::error::Error>> {
    test_app_with_settings(policy, ReasonerConfig::default())
}

pub fn test_app_with_settings(
    policy: PolicyConfig,
    reasoner_config: ReasonerConfig,
) -> Result<axum::Router, Box<dyn std::error::Error>> {
    let store = StoreService::new(StoreConfig::default())?;
    let reasoner = ReasonerService::new(reasoner_config);
    let state = AppState::new(store, reasoner, policy, AiSuggestionService::disabled());
    state.mark_ready();
    Ok(build_app(state))
}

pub fn test_app_with_store_config(
    store_config: StoreConfig,
    policy: PolicyConfig,
    reasoner_config: ReasonerConfig,
) -> Result<axum::Router, Box<dyn std::error::Error>> {
    let store = StoreService::new(store_config)?;
    let reasoner = ReasonerService::new(reasoner_config);
    let state = AppState::new(store, reasoner, policy, AiSuggestionService::disabled());
    state.mark_ready();
    Ok(build_app(state))
}

pub fn minimal_fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ontologies/minimal_services.ttl")
}

pub fn catalog_fixture_path(filename: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benches/nrese-bench-harness/fixtures/catalog-cache")
        .join(filename)
}

pub async fn body_text(
    response: axum::response::Response,
) -> Result<String, Box<dyn std::error::Error>> {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(String::from_utf8(body.to_vec())?)
}

pub async fn readyz_text(app: axum::Router) -> Result<String, Box<dyn std::error::Error>> {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    body_text(response).await
}

pub async fn query_text(
    app: axum::Router,
    query: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .body(Body::from(query.to_owned()))?,
        )
        .await?;
    body_text(response).await
}
