use serde::Serialize;

use crate::error::ApiError;
use crate::http::ai;
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
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub reasoning_mode: &'static str,
    pub reasoning_profile: &'static str,
    pub reasoning_preset: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct VersionResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub rust_edition: &'static str,
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub durable_storage_available: bool,
    pub reasoning_mode: &'static str,
    pub reasoning_profile: &'static str,
    pub reasoning_preset: Option<&'static str>,
    pub available_reasoning_presets: &'static [&'static str],
    pub graph_store_enabled: bool,
    pub sparql_update_enabled: bool,
    pub tell_enabled: bool,
    pub federated_service_enabled: bool,
    pub user_console_path: &'static str,
    pub ai_query_suggestions_enabled: bool,
    pub ai_provider: &'static str,
}

#[derive(Debug, Serialize)]
pub struct OperatorCapabilitiesResponse {
    pub operator_ui_path: &'static str,
    pub user_console_path: &'static str,
    pub reasoning_diagnostics_endpoint: &'static str,
    pub query_endpoint: &'static str,
    pub update_endpoint: &'static str,
    pub tell_endpoint: &'static str,
    pub ai_status_endpoint: &'static str,
    pub ai_query_suggestions_endpoint: &'static str,
    pub graph_store_endpoint: &'static str,
    pub admin_backup_endpoint: &'static str,
    pub admin_restore_endpoint: &'static str,
    pub service_description_endpoint: &'static str,
    pub version_endpoint: &'static str,
    pub health_endpoint: &'static str,
    pub readiness_endpoint: &'static str,
    pub store_mode: &'static str,
    pub durability: &'static str,
    pub durable_storage_available: bool,
    pub reasoning_preset: Option<&'static str>,
    pub available_reasoning_presets: &'static [&'static str],
    pub graph_store_enabled: bool,
    pub sparql_update_enabled: bool,
    pub tell_enabled: bool,
    pub federated_service_enabled: bool,
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
        store_mode: state.store_mode_name(),
        durability: state.durability_name(),
        reasoning_mode: state.reasoner_mode_name(),
        reasoning_profile: state.reasoner_profile_name(),
        reasoning_preset: active_reasoning_preset(state),
    })
}

pub fn build_version_response(state: &AppState) -> VersionResponse {
    let ai_status = ai::ai_status(state);
    VersionResponse {
        service: "nrese-server",
        version: env!("CARGO_PKG_VERSION"),
        rust_edition: "2024",
        store_mode: state.store_mode_name(),
        durability: state.durability_name(),
        durable_storage_available: durable_storage_available(),
        reasoning_mode: state.reasoner_mode_name(),
        reasoning_profile: state.reasoner_profile_name(),
        reasoning_preset: active_reasoning_preset(state),
        available_reasoning_presets: nrese_reasoner::RulesMvpPreset::available(),
        graph_store_enabled: true,
        sparql_update_enabled: true,
        tell_enabled: true,
        federated_service_enabled: false,
        user_console_path: "/console",
        ai_query_suggestions_enabled: ai_status.enabled,
        ai_provider: ai_status.provider,
    }
}

pub fn build_operator_capabilities_response(state: &AppState) -> OperatorCapabilitiesResponse {
    let ai_status = ai::ai_status(state);
    OperatorCapabilitiesResponse {
        operator_ui_path: "/ops",
        user_console_path: "/console",
        reasoning_diagnostics_endpoint: "/ops/api/diagnostics/reasoning",
        query_endpoint: "/dataset/query",
        update_endpoint: "/dataset/update",
        tell_endpoint: "/dataset/tell",
        ai_status_endpoint: "/api/ai/status",
        ai_query_suggestions_endpoint: "/api/ai/query-suggestions",
        graph_store_endpoint: "/dataset/data",
        admin_backup_endpoint: "/ops/api/admin/dataset/backup",
        admin_restore_endpoint: "/ops/api/admin/dataset/restore",
        service_description_endpoint: "/dataset/service-description",
        version_endpoint: "/version",
        health_endpoint: "/healthz",
        readiness_endpoint: "/readyz",
        store_mode: state.store_mode_name(),
        durability: state.durability_name(),
        durable_storage_available: durable_storage_available(),
        reasoning_preset: active_reasoning_preset(state),
        available_reasoning_presets: nrese_reasoner::RulesMvpPreset::available(),
        graph_store_enabled: true,
        sparql_update_enabled: true,
        tell_enabled: true,
        federated_service_enabled: false,
        ai_query_suggestions_enabled: ai_status.enabled,
        ai_provider: ai_status.provider,
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

const fn durable_storage_available() -> bool {
    cfg!(feature = "durable-storage")
}

fn active_reasoning_preset(state: &AppState) -> Option<&'static str> {
    if state.reasoner_mode_name() != "rules-mvp" {
        return None;
    }

    Some(state.reasoner().rules_mvp_preset().as_str())
}
