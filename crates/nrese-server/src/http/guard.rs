use axum::http::HeaderMap;

use crate::error::ApiError;
use crate::policy::PolicyAction;
use crate::state::AppState;

pub async fn enforce(
    state: &AppState,
    action: PolicyAction,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    state.enforce_policy_action(action, headers).await
}

pub async fn enforce_operator_read(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if !state.runtime_posture().operator_surface_enabled {
        return Err(ApiError::not_found("operator UI is disabled by policy"));
    }
    enforce(state, PolicyAction::OperatorRead, headers).await
}

pub async fn enforce_query_read(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    enforce(state, PolicyAction::QueryRead, headers).await
}

pub async fn enforce_update_write(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    enforce(state, PolicyAction::UpdateWrite, headers).await
}

pub async fn enforce_tell_write(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    enforce(state, PolicyAction::TellWrite, headers).await
}

pub async fn enforce_admin_write(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    enforce(state, PolicyAction::AdminWrite, headers).await
}

pub async fn enforce_service_description_read(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    enforce(state, PolicyAction::ServiceDescriptionRead, headers).await
}

pub async fn enforce_metrics_read(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if !state.runtime_posture().metrics_enabled {
        return Err(ApiError::not_found(
            "metrics endpoint is disabled by policy",
        ));
    }
    enforce(state, PolicyAction::MetricsRead, headers).await
}
