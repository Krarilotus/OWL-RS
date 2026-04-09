use crate::state::AppState;

pub const USER_CONSOLE_PATH: &str = "/console";
pub const OPERATOR_UI_PATH: &str = "/ops";
pub const REASONING_DIAGNOSTICS_PATH: &str = "/ops/api/diagnostics/reasoning";
pub const QUERY_ENDPOINT: &str = "/dataset/query";
pub const UPDATE_ENDPOINT: &str = "/dataset/update";
pub const TELL_ENDPOINT: &str = "/dataset/tell";
pub const GRAPH_STORE_ENDPOINT: &str = "/dataset/data";
pub const ADMIN_BACKUP_ENDPOINT: &str = "/ops/api/admin/dataset/backup";
pub const ADMIN_RESTORE_ENDPOINT: &str = "/ops/api/admin/dataset/restore";
pub const AI_STATUS_ENDPOINT: &str = "/api/ai/status";
pub const AI_QUERY_SUGGESTIONS_ENDPOINT: &str = "/api/ai/query-suggestions";
pub const SERVICE_DESCRIPTION_ENDPOINT: &str = "/dataset/service-description";
pub const VERSION_ENDPOINT: &str = "/version";
pub const HEALTH_ENDPOINT: &str = "/healthz";
pub const READINESS_ENDPOINT: &str = "/readyz";
pub const METRICS_ENDPOINT: &str = "/metrics";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePosture {
    pub reasoning_preset: Option<&'static str>,
    pub available_reasoning_presets: &'static [&'static str],
    pub query_enabled: bool,
    pub graph_store_enabled: bool,
    pub sparql_update_enabled: bool,
    pub tell_enabled: bool,
    pub federated_service_enabled: bool,
    pub operator_surface_enabled: bool,
    pub metrics_enabled: bool,
    pub ai_query_suggestions_enabled: bool,
    pub ai_provider: &'static str,
}

impl RuntimePosture {
    pub fn from_state(state: &AppState) -> Self {
        let ai_status = state.ai().status();
        Self {
            reasoning_preset: active_reasoning_preset(state),
            available_reasoning_presets: nrese_reasoner::RulesMvpPreset::available(),
            query_enabled: true,
            graph_store_enabled: true,
            sparql_update_enabled: true,
            tell_enabled: true,
            federated_service_enabled: false,
            operator_surface_enabled: state.policy().expose_operator_ui,
            metrics_enabled: state.policy().expose_metrics,
            ai_query_suggestions_enabled: ai_status.enabled,
            ai_provider: ai_status.provider,
        }
    }

    pub fn operator_ui_path(&self) -> Option<&'static str> {
        self.operator_surface_enabled.then_some(OPERATOR_UI_PATH)
    }

    pub fn reasoning_diagnostics_path(&self) -> Option<&'static str> {
        self.operator_surface_enabled
            .then_some(REASONING_DIAGNOSTICS_PATH)
    }

    pub fn metrics_path(&self) -> Option<&'static str> {
        self.metrics_enabled.then_some(METRICS_ENDPOINT)
    }
}

fn active_reasoning_preset(state: &AppState) -> Option<&'static str> {
    if state.reasoner_mode_name() != "rules-mvp" {
        return None;
    }

    Some(state.reasoner().rules_mvp_preset().as_str())
}

#[cfg(test)]
mod tests {
    use nrese_reasoner::{ReasonerConfig, ReasonerService};
    use nrese_store::{StoreConfig, StoreService};

    use crate::ai::AiSuggestionService;
    use crate::policy::PolicyConfig;
    use crate::state::AppState;

    use super::{METRICS_ENDPOINT, OPERATOR_UI_PATH, RuntimePosture};

    fn test_state(policy: PolicyConfig) -> AppState {
        let store = StoreService::new(StoreConfig::default()).expect("store");
        let reasoner = ReasonerService::new(ReasonerConfig::default());
        AppState::new(store, reasoner, policy, AiSuggestionService::disabled())
    }

    #[test]
    fn posture_reflects_disabled_operator_surface() {
        let posture = RuntimePosture::from_state(&test_state(PolicyConfig {
            expose_operator_ui: false,
            ..PolicyConfig::default()
        }));

        assert!(!posture.operator_surface_enabled);
        assert_eq!(posture.operator_ui_path(), None);
    }

    #[test]
    fn posture_reflects_disabled_metrics() {
        let posture = RuntimePosture::from_state(&test_state(PolicyConfig {
            expose_metrics: false,
            ..PolicyConfig::default()
        }));

        assert!(!posture.metrics_enabled);
        assert_eq!(posture.metrics_path(), None);
    }

    #[test]
    fn posture_exposes_enabled_paths() {
        let posture = RuntimePosture::from_state(&test_state(PolicyConfig::default()));

        assert_eq!(posture.operator_ui_path(), Some(OPERATOR_UI_PATH));
        assert_eq!(posture.metrics_path(), Some(METRICS_ENDPOINT));
    }
}
