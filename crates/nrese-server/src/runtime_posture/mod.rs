use nrese_reasoner::ReasoningMode;
use nrese_store::StoreMode;

use crate::auth::AuthConfig;
use crate::policy::{PolicyConfig, SparqlParseErrorProfile};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeploymentPosture {
    #[default]
    OpenWorkbench,
    ReadOnlyDemo,
    InternalAuthenticated,
    ReplacementGrade,
}

impl DeploymentPosture {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenWorkbench => "open-workbench",
            Self::ReadOnlyDemo => "read-only-demo",
            Self::InternalAuthenticated => "internal-authenticated",
            Self::ReplacementGrade => "replacement-grade",
        }
    }

    pub const fn write_surfaces_enabled(self) -> bool {
        !matches!(self, Self::ReadOnlyDemo)
    }

    pub const fn admin_surface_enabled(self) -> bool {
        !matches!(self, Self::ReadOnlyDemo)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePosture {
    pub deployment_posture: &'static str,
    pub reasoning_profile: &'static str,
    pub reasoning_read_model: &'static str,
    pub reasoning_semantic_tier: &'static str,
    pub query_enabled: bool,
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

impl RuntimePosture {
    pub fn from_state(state: &AppState) -> Self {
        let ai_status = state.ai().status();
        let deployment_posture = state.deployment_posture();
        let graph_write_enabled = deployment_posture.write_surfaces_enabled();
        let admin_surface_enabled = deployment_posture.admin_surface_enabled();

        Self {
            deployment_posture: deployment_posture.as_str(),
            reasoning_profile: state.reasoner_profile_name(),
            reasoning_read_model: state.reasoner_read_model_name(),
            reasoning_semantic_tier: state.reasoner().semantic_tier(),
            query_enabled: true,
            graph_store_enabled: true,
            graph_write_enabled,
            sparql_update_enabled: graph_write_enabled,
            tell_enabled: graph_write_enabled,
            federated_service_enabled: false,
            admin_surface_enabled,
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

pub fn validate_configuration(
    deployment_posture: DeploymentPosture,
    store_mode: StoreMode,
    reasoning_mode: ReasoningMode,
    policy: &PolicyConfig,
) -> Result<(), &'static str> {
    match deployment_posture {
        DeploymentPosture::OpenWorkbench | DeploymentPosture::ReadOnlyDemo => Ok(()),
        DeploymentPosture::InternalAuthenticated => {
            validate_authenticated_posture(policy)?;
            Ok(())
        }
        DeploymentPosture::ReplacementGrade => {
            validate_authenticated_posture(policy)?;
            if store_mode != StoreMode::OnDisk {
                return Err("replacement-grade posture requires on-disk durable storage");
            }
            if reasoning_mode == ReasoningMode::OwlDlTarget {
                return Err(
                    "replacement-grade posture cannot use owl-dl-target while it remains scaffolded",
                );
            }
            if policy.sparql_parse_error_profile != SparqlParseErrorProfile::ProblemJson {
                return Err("replacement-grade posture requires problem-json SPARQL parse errors");
            }
            Ok(())
        }
    }
}

fn validate_authenticated_posture(policy: &PolicyConfig) -> Result<(), &'static str> {
    if matches!(policy.auth, AuthConfig::None) {
        return Err("authenticated deployment posture requires auth mode other than none");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{StoreConfig, StoreMode, StoreService};

    use crate::ai::AiSuggestionService;
    use crate::auth::AuthConfig;
    use crate::policy::PolicyConfig;
    use crate::state::AppState;

    use super::{
        DeploymentPosture, METRICS_ENDPOINT, OPERATOR_UI_PATH, RuntimePosture,
        validate_configuration,
    };

    fn test_state(policy: PolicyConfig, deployment_posture: DeploymentPosture) -> AppState {
        let store = StoreService::new(StoreConfig::default()).expect("store");
        let reasoner = ReasonerService::new(ReasonerConfig::default());
        AppState::new(
            store,
            reasoner,
            policy,
            AiSuggestionService::disabled(),
            deployment_posture,
        )
    }

    #[test]
    fn posture_reflects_disabled_operator_surface() {
        let posture = RuntimePosture::from_state(&test_state(
            PolicyConfig {
                expose_operator_ui: false,
                ..PolicyConfig::default()
            },
            DeploymentPosture::OpenWorkbench,
        ));

        assert!(!posture.operator_surface_enabled);
        assert_eq!(posture.operator_ui_path(), None);
    }

    #[test]
    fn posture_reflects_disabled_metrics() {
        let posture = RuntimePosture::from_state(&test_state(
            PolicyConfig {
                expose_metrics: false,
                ..PolicyConfig::default()
            },
            DeploymentPosture::OpenWorkbench,
        ));

        assert!(!posture.metrics_enabled);
        assert_eq!(posture.metrics_path(), None);
    }

    #[test]
    fn posture_exposes_enabled_paths() {
        let posture = RuntimePosture::from_state(&test_state(
            PolicyConfig::default(),
            DeploymentPosture::OpenWorkbench,
        ));

        assert_eq!(posture.operator_ui_path(), Some(OPERATOR_UI_PATH));
        assert_eq!(posture.metrics_path(), Some(METRICS_ENDPOINT));
    }

    #[test]
    fn read_only_demo_disables_write_surfaces() {
        let posture = RuntimePosture::from_state(&test_state(
            PolicyConfig::default(),
            DeploymentPosture::ReadOnlyDemo,
        ));

        assert!(posture.graph_store_enabled);
        assert!(!posture.graph_write_enabled);
        assert!(!posture.sparql_update_enabled);
        assert!(!posture.tell_enabled);
        assert!(!posture.admin_surface_enabled);
    }

    #[test]
    fn replacement_grade_requires_durable_store() {
        let result = validate_configuration(
            DeploymentPosture::ReplacementGrade,
            StoreMode::InMemory,
            ReasoningMode::Disabled,
            &PolicyConfig {
                auth: AuthConfig::BearerStatic(crate::auth::StaticBearerConfig {
                    read_token: Some("read".to_owned()),
                    admin_token: "admin".to_owned(),
                }),
                ..PolicyConfig::default()
            },
        );

        assert_eq!(
            result,
            Err("replacement-grade posture requires on-disk durable storage")
        );
    }

    #[test]
    fn internal_authenticated_requires_auth() {
        let result = validate_configuration(
            DeploymentPosture::InternalAuthenticated,
            StoreMode::OnDisk,
            ReasoningMode::RulesMvp,
            &PolicyConfig::default(),
        );

        assert_eq!(
            result,
            Err("authenticated deployment posture requires auth mode other than none")
        );
    }
}
