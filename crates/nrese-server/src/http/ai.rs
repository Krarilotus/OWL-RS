use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use crate::ai::{AiStatusResponse, QuerySuggestionRequest, SuggestionContext};
use crate::error::ApiError;
use crate::policy::PolicyAction;
use crate::state::AppState;

pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    state
        .enforce_policy_action(PolicyAction::QueryRead, &headers)
        .await?;
    Ok(Json(ai_status(&state)).into_response())
}

pub async fn query_suggestions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<QuerySuggestionRequest>,
) -> Result<Response, ApiError> {
    state
        .enforce_policy_action(PolicyAction::QueryRead, &headers)
        .await?;
    let stats = state
        .store()
        .stats()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let response = state
        .ai()
        .suggest(SuggestionContext {
            locale: request.locale.unwrap_or_else(|| "en".to_owned()),
            prompt: request.prompt,
            current_query: request.current_query,
            quad_count: stats.quad_count as u64,
            named_graph_count: stats.named_graph_count,
            reasoning_mode: state.reasoner_mode_name(),
            reasoning_profile: state.reasoner_profile_name(),
        })
        .await?;
    Ok(Json(response).into_response())
}

pub fn ai_status(state: &AppState) -> AiStatusResponse {
    state.ai().status()
}
