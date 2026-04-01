#![allow(dead_code)]

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_server::ai::AiSuggestionService;
use nrese_store::{StoreConfig, StoreService};

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
