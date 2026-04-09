use serde::Serialize;

use crate::error::ApiError;
use crate::runtime_posture::{
    ADMIN_BACKUP_ENDPOINT, ADMIN_RESTORE_ENDPOINT, AI_QUERY_SUGGESTIONS_ENDPOINT,
    AI_STATUS_ENDPOINT, GRAPH_STORE_ENDPOINT, HEALTH_ENDPOINT, QUERY_ENDPOINT, READINESS_ENDPOINT,
    SERVICE_DESCRIPTION_ENDPOINT, TELL_ENDPOINT, UPDATE_ENDPOINT, USER_CONSOLE_PATH,
    VERSION_ENDPOINT,
};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ReadyResponse {
    pub status: &'static str,
    pub revision: u64,
    pub quad_count: u64,
    pub named_graph_count: usize,
    pub ontology_path: Option<String>,
    pub deployment_posture: &'static str,
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub reasoning_mode: &'static str,
    pub reasoning_profile: &'static str,
    pub reasoning_read_model: &'static str,
    pub reasoning_semantic_tier: &'static str,
}

#[derive(Debug, Serialize)]
pub struct VersionResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub rust_edition: &'static str,
    pub deployment_posture: &'static str,
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub durable_storage_available: bool,
    pub reasoning_mode: &'static str,
    pub reasoning_profile: &'static str,
    pub reasoning_read_model: &'static str,
    pub reasoning_semantic_tier: &'static str,
    pub graph_store_enabled: bool,
    pub graph_write_enabled: bool,
    pub sparql_update_enabled: bool,
    pub tell_enabled: bool,
    pub federated_service_enabled: bool,
    pub admin_surface_enabled: bool,
    pub operator_surface_enabled: bool,
    pub metrics_enabled: bool,
    pub user_console_path: &'static str,
    pub ai_query_suggestions_enabled: bool,
    pub ai_provider: &'static str,
}

#[derive(Debug, Serialize)]
pub struct OperatorCapabilitiesResponse {
    pub operator_ui_path: Option<&'static str>,
    pub user_console_path: &'static str,
    pub reasoning_diagnostics_endpoint: Option<&'static str>,
    pub query_endpoint: &'static str,
    pub update_endpoint: &'static str,
    pub tell_endpoint: &'static str,
    pub ai_status_endpoint: &'static str,
    pub ai_query_suggestions_endpoint: &'static str,
    pub graph_store_endpoint: &'static str,
    pub admin_backup_endpoint: Option<&'static str>,
    pub admin_restore_endpoint: Option<&'static str>,
    pub service_description_endpoint: &'static str,
    pub version_endpoint: &'static str,
    pub health_endpoint: &'static str,
    pub readiness_endpoint: &'static str,
    pub metrics_endpoint: Option<&'static str>,
    pub deployment_posture: &'static str,
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub durable_storage_available: bool,
    pub reasoning_profile: &'static str,
    pub reasoning_read_model: &'static str,
    pub reasoning_semantic_tier: &'static str,
    pub graph_store_enabled: bool,
    pub graph_write_enabled: bool,
    pub sparql_update_enabled: bool,
    pub tell_enabled: bool,
    pub federated_service_enabled: bool,
    pub admin_surface_enabled: bool,
    pub operator_surface_enabled: bool,
    pub metrics_enabled: bool,
    pub ai_query_suggestions_enabled: bool,
    pub ai_provider: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ExtendedHealthResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub status: &'static str,
    pub ready: bool,
    pub revision: u64,
    pub quad_count: u64,
    pub named_graph_count: usize,
    pub deployment_posture: &'static str,
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub reasoning_mode: &'static str,
    pub reasoning_profile: &'static str,
    pub ontology_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminRestoreResponse {
    pub status: &'static str,
    pub revision: u64,
    pub quad_count: u64,
    pub format: &'static str,
    pub checksum_sha256: String,
    pub replaced_existing: bool,
}

pub fn build_ready_response(state: &AppState) -> Result<ReadyResponse, ApiError> {
    let stats = state
        .store()
        .stats()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let posture = state.runtime_posture();

    Ok(ReadyResponse {
        status: if state.is_ready() {
            "ready"
        } else {
            "starting"
        },
        revision: state.store().current_revision(),
        quad_count: stats.quad_count as u64,
        named_graph_count: stats.named_graph_count,
        ontology_path: state
            .store()
            .preloaded_ontology_path()
            .map(|path| path.display().to_string()),
        deployment_posture: posture.deployment_posture,
        store_mode: state.store_mode_name(),
        durability: state.durability_name(),
        reasoning_mode: state.reasoner_mode_name(),
        reasoning_profile: posture.reasoning_profile,
        reasoning_read_model: posture.reasoning_read_model,
        reasoning_semantic_tier: posture.reasoning_semantic_tier,
    })
}

pub fn build_version_response(state: &AppState) -> VersionResponse {
    let posture = state.runtime_posture();
    VersionResponse {
        service: "nrese-server",
        version: env!("CARGO_PKG_VERSION"),
        rust_edition: "2024",
        deployment_posture: posture.deployment_posture,
        store_mode: state.store_mode_name(),
        durability: state.durability_name(),
        durable_storage_available: durable_storage_available(),
        reasoning_mode: state.reasoner_mode_name(),
        reasoning_profile: posture.reasoning_profile,
        reasoning_read_model: posture.reasoning_read_model,
        reasoning_semantic_tier: posture.reasoning_semantic_tier,
        graph_store_enabled: posture.graph_store_enabled,
        graph_write_enabled: posture.graph_write_enabled,
        sparql_update_enabled: posture.sparql_update_enabled,
        tell_enabled: posture.tell_enabled,
        federated_service_enabled: posture.federated_service_enabled,
        admin_surface_enabled: posture.admin_surface_enabled,
        operator_surface_enabled: posture.operator_surface_enabled,
        metrics_enabled: posture.metrics_enabled,
        user_console_path: USER_CONSOLE_PATH,
        ai_query_suggestions_enabled: posture.ai_query_suggestions_enabled,
        ai_provider: posture.ai_provider,
    }
}

pub fn build_operator_capabilities_response(state: &AppState) -> OperatorCapabilitiesResponse {
    let posture = state.runtime_posture();
    OperatorCapabilitiesResponse {
        operator_ui_path: posture.operator_ui_path(),
        user_console_path: USER_CONSOLE_PATH,
        reasoning_diagnostics_endpoint: posture.reasoning_diagnostics_path(),
        query_endpoint: QUERY_ENDPOINT,
        update_endpoint: UPDATE_ENDPOINT,
        tell_endpoint: TELL_ENDPOINT,
        ai_status_endpoint: AI_STATUS_ENDPOINT,
        ai_query_suggestions_endpoint: AI_QUERY_SUGGESTIONS_ENDPOINT,
        graph_store_endpoint: GRAPH_STORE_ENDPOINT,
        admin_backup_endpoint: posture
            .admin_surface_enabled
            .then_some(ADMIN_BACKUP_ENDPOINT),
        admin_restore_endpoint: posture
            .admin_surface_enabled
            .then_some(ADMIN_RESTORE_ENDPOINT),
        service_description_endpoint: SERVICE_DESCRIPTION_ENDPOINT,
        version_endpoint: VERSION_ENDPOINT,
        health_endpoint: HEALTH_ENDPOINT,
        readiness_endpoint: READINESS_ENDPOINT,
        metrics_endpoint: posture.metrics_path(),
        deployment_posture: posture.deployment_posture,
        store_mode: state.store_mode_name(),
        durability: state.durability_name(),
        durable_storage_available: durable_storage_available(),
        reasoning_profile: posture.reasoning_profile,
        reasoning_read_model: posture.reasoning_read_model,
        reasoning_semantic_tier: posture.reasoning_semantic_tier,
        graph_store_enabled: posture.graph_store_enabled,
        graph_write_enabled: posture.graph_write_enabled,
        sparql_update_enabled: posture.sparql_update_enabled,
        tell_enabled: posture.tell_enabled,
        federated_service_enabled: posture.federated_service_enabled,
        admin_surface_enabled: posture.admin_surface_enabled,
        operator_surface_enabled: posture.operator_surface_enabled,
        metrics_enabled: posture.metrics_enabled,
        ai_query_suggestions_enabled: posture.ai_query_suggestions_enabled,
        ai_provider: posture.ai_provider,
    }
}

pub fn build_extended_health_response(
    state: &AppState,
    ready: ReadyResponse,
) -> ExtendedHealthResponse {
    ExtendedHealthResponse {
        service: "nrese-server",
        version: env!("CARGO_PKG_VERSION"),
        status: ready.status,
        ready: state.is_ready(),
        revision: ready.revision,
        quad_count: ready.quad_count,
        named_graph_count: ready.named_graph_count,
        deployment_posture: ready.deployment_posture,
        store_mode: ready.store_mode,
        durability: ready.durability,
        reasoning_mode: ready.reasoning_mode,
        reasoning_profile: ready.reasoning_profile,
        ontology_path: ready.ontology_path,
    }
}

pub fn build_admin_restore_response(
    report: &nrese_store::DatasetRestoreReport,
) -> AdminRestoreResponse {
    AdminRestoreResponse {
        status: "restored",
        revision: report.revision,
        quad_count: report.quad_count,
        format: "n-quads",
        checksum_sha256: report.checksum_sha256.clone(),
        replaced_existing: report.replaced_existing,
    }
}

pub const fn durable_storage_available() -> bool {
    cfg!(feature = "durable-storage")
}
