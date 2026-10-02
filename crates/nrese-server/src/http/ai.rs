use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::ai::{AiStatusResponse, QuerySuggestionRequest, SuggestionContext};
use crate::error::ApiError;
use crate::state::AppState;

pub async fn status(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    crate::http::guard::enforce_query_read(&state, &authenticated).await?;
    Ok(Json(ai_status(&state)).into_response())
}

pub async fn query_suggestions(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
    Json(request): Json<QuerySuggestionRequest>,
) -> Result<Response, ApiError> {
    // The suggestions describe the whole dataset.
    crate::http::guard::enforce_whole_read(&state, &authenticated).await?;
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
            reasoning_read_model: state.reasoner_read_model_name(),
        })
        .await?;
    Ok(Json(response).into_response())
}

pub fn ai_status(state: &AppState) -> AiStatusResponse {
    state.ai().status()
}
