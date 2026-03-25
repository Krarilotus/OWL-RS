use axum::body::to_bytes;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{StoreConfig, StoreService};

use crate::ai::AiSuggestionService;
use crate::http::operator_diagnostics;
use crate::policy::PolicyConfig;
use crate::state::AppState;

#[tokio::test]
async fn reasoning_diagnostics_includes_reject_diagnostics_baseline() {
    let store = StoreService::new(StoreConfig::default()).expect("store should initialize");
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let state = AppState::new(
        store,
        reasoner,
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
    );
    let response = operator_diagnostics::reasoning(state);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let text = String::from_utf8(body.to_vec()).expect("body should be utf-8");

    assert!(text.contains("reject_diagnostics"));
    assert!(text.contains("hybrid-heuristic-plus-deep-justification"));
    assert!(text.contains("owl-equality-reasoning"));
}
