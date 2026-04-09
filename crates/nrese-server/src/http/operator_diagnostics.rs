use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ApiError;
use crate::http::reasoning_view;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct RuntimeDiagnosticsResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub deployment_posture: &'static str,
    pub ready: bool,
    pub revision: u64,
    pub quad_count: u64,
    pub named_graph_count: usize,
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub reasoning_mode: &'static str,
    pub reasoning_profile: &'static str,
    pub reasoning_read_model: &'static str,
    pub graph_write_enabled: bool,
    pub admin_surface_enabled: bool,
    pub operator_ui_enabled: bool,
    pub metrics_enabled: bool,
}

pub fn runtime(state: AppState) -> Result<Response, ApiError> {
    let stats = state
        .store()
        .stats()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let posture = state.runtime_posture();

    Ok((
        StatusCode::OK,
        Json(RuntimeDiagnosticsResponse {
            service: "nrese-server",
            version: env!("CARGO_PKG_VERSION"),
            deployment_posture: posture.deployment_posture,
            ready: state.is_ready(),
            revision: state.store().current_revision(),
            quad_count: stats.quad_count as u64,
            named_graph_count: stats.named_graph_count,
            store_mode: state.store_mode_name(),
            durability: state.durability_name(),
            reasoning_mode: state.reasoner_mode_name(),
            reasoning_profile: state.reasoner_profile_name(),
            reasoning_read_model: state.reasoner_read_model_name(),
            graph_write_enabled: posture.graph_write_enabled,
            admin_surface_enabled: posture.admin_surface_enabled,
            operator_ui_enabled: posture.operator_surface_enabled,
            metrics_enabled: posture.metrics_enabled,
        }),
    )
        .into_response())
}

pub fn reasoning(state: AppState) -> Response {
    let last_run = state.last_reasoning_run();
    let capabilities = state
        .reasoner()
        .capabilities()
        .iter()
        .map(reasoning_view::capability_view)
        .collect();

    (
        StatusCode::OK,
        Json(reasoning_view::ReasoningDiagnosticsResponse {
            revision: state.store().current_revision(),
            mode: state.reasoner_mode_name(),
            profile: state.reasoner_profile_name(),
            read_model: state.reasoner_read_model_name(),
            capabilities,
            configured_policy: reasoning_view::configured_reasoning_policy(&state.reasoner()),
            last_run: last_run.as_ref().map(reasoning_view::last_run_view),
            reject_diagnostics: reasoning_view::reject_diagnostics_baseline(last_run.as_ref()),
        }),
    )
        .into_response()
}
