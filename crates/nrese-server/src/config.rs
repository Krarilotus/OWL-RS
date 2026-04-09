use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::ai::AiConfig;
use anyhow::{Context, Result};
use nrese_reasoner::ReasonerConfig;
use nrese_store::StoreConfig;

use crate::policy::PolicyConfig;
use crate::runtime_posture::{DeploymentPosture, validate_configuration};

mod ai_env;
mod auth_env;
mod cli;
mod env_names;
mod env_values;
mod file_config;
mod policy_env;
mod reasoner_env;
mod source;
mod store_env;
#[cfg(test)]
mod test_support;

pub use cli::CliConfig;

use ai_env::parse_ai_config;
use env_names as names;
use file_config::load_file_source;
use policy_env::parse_policy_config;
use reasoner_env::parse_reasoner_config;
use source::{ConfigSource, KeyValueSource, LayeredSource, ProcessEnv};
use store_env::parse_store_config;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_address: SocketAddr,
    pub deployment_posture: DeploymentPosture,
    pub store: StoreConfig,
    pub reasoner: ReasonerConfig,
    pub policy: PolicyConfig,
    pub ai: AiConfig,
}

impl ServerConfig {
    pub fn from_env() -> Result<Self> {
        Self::load(None)
    }

    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let env_source = ProcessEnv;
        let file_source = match resolve_config_path(config_path, &env_source) {
            Some(path) => load_file_source(&path)?,
            None => KeyValueSource::default(),
        };
        let source = LayeredSource::new(file_source, env_source);
        Self::from_source(&source)
    }

    fn from_source(source: &dyn ConfigSource) -> Result<Self> {
        let bind_address_raw = source
            .get(names::BIND_ADDR)
            .unwrap_or_else(|| "127.0.0.1:8080".to_owned());
        let bind_address = bind_address_raw
            .parse()
            .context("failed to parse bind address")?;
        let deployment_posture =
            parse_deployment_posture(source.get(names::DEPLOYMENT_POSTURE).as_deref())?;
        let store = parse_store_config(source);
        let reasoner = parse_reasoner_config(source)?;
        let policy = parse_policy_config(source)?;
        let ai = parse_ai_config(source)?;

        validate_configuration(deployment_posture, store.mode, reasoner.mode(), &policy)
            .map_err(anyhow::Error::msg)?;

        Ok(Self {
            bind_address,
            deployment_posture,
            store,
            reasoner,
            policy,
            ai,
        })
    }
}

fn parse_deployment_posture(input: Option<&str>) -> Result<DeploymentPosture> {
    let Some(raw) = input.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(DeploymentPosture::OpenWorkbench);
    };

    match raw.to_ascii_lowercase().as_str() {
        "open-workbench" | "open_workbench" | "development" | "dev" => {
            Ok(DeploymentPosture::OpenWorkbench)
        }
        "read-only-demo" | "read_only_demo" | "readonlydemo" | "demo" => {
            Ok(DeploymentPosture::ReadOnlyDemo)
        }
        "internal-authenticated" | "internal_authenticated" | "internal" => {
            Ok(DeploymentPosture::InternalAuthenticated)
        }
        "replacement-grade" | "replacement_grade" | "replacement" => {
            Ok(DeploymentPosture::ReplacementGrade)
        }
        unknown => anyhow::bail!(
            "unsupported value '{unknown}' in {}",
            names::DEPLOYMENT_POSTURE
        ),
    }
}

fn resolve_config_path(
    explicit_path: Option<&Path>,
    env_source: &dyn ConfigSource,
) -> Option<PathBuf> {
    explicit_path
        .map(Path::to_path_buf)
        .or_else(|| env_source.get(names::CONFIG_PATH).map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use nrese_reasoner::ReasoningMode;
    use tempfile::tempdir;

    use super::test_support::{EnvGuard, env_lock};
    use super::{CliConfig, DeploymentPosture, ServerConfig, env_names as names};

    #[test]
    fn server_config_loads_from_file() {
        let _lock = env_lock().lock().expect("env lock");
        let _guard = EnvGuard::set(&clean_runtime_env_overrides(&[]));
        let temp_dir = tempdir().expect("temp dir");
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[server]
bind_address = "0.0.0.0:9191"
deployment_posture = "open-workbench"

[store]
mode = "in-memory"
data_dir = "./runtime-data"

[reasoner]
mode = "rules-mvp"

[ai]
enabled = true
provider = "gemini"
model = "gemini-2.5-flash"

[ai.gemini]
api_key = "test-key"

[policy.exposure]
metrics = false

[policy]
sparql_parse_error_profile = "plain-text"

[auth]
mode = "none"
"#,
        )
        .expect("config file");

        let config = ServerConfig::load(Some(&path)).expect("server config");

        assert_eq!(config.bind_address.to_string(), "0.0.0.0:9191");
        assert_eq!(config.deployment_posture, DeploymentPosture::OpenWorkbench);
        assert_eq!(config.store.data_dir.to_string_lossy(), "./runtime-data");
        assert!(!config.policy.expose_metrics);
        assert_eq!(
            config.policy.sparql_parse_error_profile,
            crate::policy::SparqlParseErrorProfile::PlainText
        );
        assert!(config.ai.enabled);
    }

    #[test]
    fn env_overrides_file_values() {
        let _lock = env_lock().lock().expect("env lock");
        let _guard = EnvGuard::set(&clean_runtime_env_overrides(&[
            (names::BIND_ADDR, Some("127.0.0.1:9898")),
            (names::REASONING_MODE, Some("disabled")),
            (names::CONFIG_PATH, None),
        ]));
        let temp_dir = tempdir().expect("temp dir");
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[server]
bind_address = "0.0.0.0:9191"
deployment_posture = "open-workbench"

[reasoner]
mode = "rules-mvp"

[auth]
mode = "none"
"#,
        )
        .expect("config file");

        let config = ServerConfig::load(Some(&path)).expect("server config");

        assert_eq!(config.bind_address.to_string(), "127.0.0.1:9898");
        assert_eq!(config.reasoner.mode(), ReasoningMode::Disabled);
    }

    #[test]
    fn config_path_can_be_selected_from_env() {
        let _lock = env_lock().lock().expect("env lock");
        let temp_dir = tempdir().expect("temp dir");
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[server]
bind_address = "127.0.0.1:9393"
deployment_posture = "open-workbench"

[auth]
mode = "none"
"#,
        )
        .expect("config file");
        let _guard = EnvGuard::set(&clean_runtime_env_overrides(&[
            (names::CONFIG_PATH, Some(path.to_string_lossy().as_ref())),
            (names::AUTH_MODE, None),
        ]));

        let config = ServerConfig::load(None).expect("server config");

        assert_eq!(config.bind_address.to_string(), "127.0.0.1:9393");
    }

    #[test]
    fn cli_parser_rejects_unknown_flags() {
        assert!(CliConfig::from_args(["nrese-server".into(), "--verbose".into()]).is_err());
    }

    #[test]
    fn deployment_posture_rejects_unauthenticated_internal_mode() {
        let _lock = env_lock().lock().expect("env lock");
        let _guard = EnvGuard::set(&clean_runtime_env_overrides(&[
            (names::DEPLOYMENT_POSTURE, Some("internal-authenticated")),
            (names::AUTH_MODE, Some("none")),
        ]));

        assert!(ServerConfig::from_env().is_err());
    }

    fn clean_runtime_env_overrides<'a>(
        overrides: &'a [(&'static str, Option<&'a str>)],
    ) -> Vec<(&'static str, Option<&'a str>)> {
        let mut values = vec![
            (names::CONFIG_PATH, None),
            (names::BIND_ADDR, None),
            (names::DEPLOYMENT_POSTURE, None),
            (names::DATA_DIR, None),
            (names::STORE_MODE, None),
            (names::ONTOLOGY_PATH, None),
            (names::REASONING_MODE, None),
            (names::REASONER_READ_MODEL, None),
            (names::REASONER_RULES_MVP_PRESET, None),
            (names::REASONER_RULES_MVP_FEATURES, None),
            (names::MAX_QUERY_BYTES, None),
            (names::MAX_UPDATE_BYTES, None),
            (names::MAX_RDF_UPLOAD_BYTES, None),
            (names::RATE_LIMIT_WINDOW_SECS, None),
            (names::READ_REQUESTS_PER_WINDOW, None),
            (names::WRITE_REQUESTS_PER_WINDOW, None),
            (names::ADMIN_REQUESTS_PER_WINDOW, None),
            (names::QUERY_TIMEOUT_MS, None),
            (names::UPDATE_TIMEOUT_MS, None),
            (names::GRAPH_READ_TIMEOUT_MS, None),
            (names::GRAPH_WRITE_TIMEOUT_MS, None),
            (names::SPARQL_PARSE_ERROR_PROFILE, None),
            (names::ENABLE_OPERATOR_UI, None),
            (names::ENABLE_METRICS, None),
            (names::AI_ENABLED, None),
            (names::AI_PROVIDER, None),
            (names::AI_MODEL, None),
            (names::AI_TIMEOUT_MS, None),
            (names::AI_MAX_SUGGESTIONS, None),
            (names::AI_SYSTEM_PROMPT, None),
            (names::AI_GOOGLE_API_KEY, None),
            (names::AI_GOOGLE_API_BASE, None),
            (names::AI_OPENROUTER_API_KEY, None),
            (names::AI_OPENROUTER_API_BASE, None),
            (names::AI_OPENROUTER_SITE_URL, None),
            (names::AI_OPENROUTER_APP_NAME, None),
            ("GOOGLE_API_KEY", None),
            (names::AUTH_MODE, None),
            (names::AUTH_READ_TOKEN, None),
            (names::AUTH_ADMIN_TOKEN, None),
            (names::AUTH_JWT_SECRET, None),
            (names::AUTH_JWT_ISSUER, None),
            (names::AUTH_JWT_AUDIENCE, None),
            (names::AUTH_JWT_READ_ROLE, None),
            (names::AUTH_JWT_ADMIN_ROLE, None),
            (names::AUTH_JWT_LEEWAY_SECS, None),
            (names::AUTH_MTLS_SUBJECT_HEADER, None),
            (names::AUTH_MTLS_READ_SUBJECTS, None),
            (names::AUTH_MTLS_ADMIN_SUBJECTS, None),
            (names::AUTH_OIDC_INTROSPECTION_URL, None),
            (names::AUTH_OIDC_CLIENT_ID, None),
            (names::AUTH_OIDC_CLIENT_SECRET, None),
            (names::AUTH_OIDC_READ_ROLE, None),
            (names::AUTH_OIDC_ADMIN_ROLE, None),
            (names::AUTH_OIDC_TIMEOUT_MS, None),
        ];
        values.extend_from_slice(overrides);
        values
    }
}
