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
mod replication_env;
pub mod settings;
mod source;
mod store_env;
pub mod units;

pub use cli::{
    CliCommand, CliConfig, ConvertCommand, LoadCommand, PrintQueryCommand, QueryCommand,
};

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
    pub replication: crate::replication::ReplicationConfig,
}

impl ServerConfig {
    /// Installs the process-wide safety fallback at startup, before opening stores.
    /// Parsing configuration and constructing repositories have no such side effect.
    /// Embedded hosts may instead explicitly use `nrese_exec::memory::set_process_limit`.
    pub fn install_process_memory_policy(&self) {
        nrese_exec::memory::set_process_limit(self.store.process_memory_bytes);
    }

    /// The effective settings, one `key = value` per line, for `check-config`. Credentials
    /// are never printed: authentication and AI show their mode and provider only.
    pub fn summary(&self) -> String {
        let limits = &self.policy.limits;
        let timeouts = &self.policy.timeouts;
        let rates = &self.policy.rate_limits;
        let store = &self.store;
        // 0 switches a memory budget off.
        let size = |bytes: usize| match bytes {
            0 => "unlimited".to_owned(),
            bytes => units::format_size(bytes as u64),
        };
        let lines = [
            ("server.bind_address", self.bind_address.to_string()),
            (
                "server.deployment_posture",
                self.deployment_posture.as_str().to_owned(),
            ),
            ("store.mode", format!("{:?}", store.mode)),
            (
                "replication.mode",
                self.replication.mode.as_str().to_owned(),
            ),
            ("store.data_dir", store.data_dir.display().to_string()),
            ("store.verify_on_open", store.verify_on_open.to_string()),
            ("store.map_checkpoints", store.map_checkpoints.to_string()),
            ("store.wal_archive", store.wal_archive.to_string()),
            (
                "store.index_encoding",
                store.index_encoding.as_str().to_owned(),
            ),
            ("store.vocabulary", store.vocabulary.as_str().to_owned()),
            (
                "store.ontology_path",
                store
                    .ontology_path
                    .as_ref()
                    .map_or("(none)".to_owned(), |p| p.display().to_string()),
            ),
            (
                "store.default_graph",
                if store.union_default_graph {
                    "union"
                } else {
                    "default"
                }
                .to_owned(),
            ),
            (
                "federation.allow",
                if store.federation.enabled() {
                    store.federation.allow.join(", ")
                } else {
                    "(none: SERVICE is off)".to_owned()
                },
            ),
            ("shacl.shapes_graph", store.shapes_graph.clone()),
            ("shacl.gate", format!("{:?}", store.shacl_gate)),
            ("reasoner.mode", self.reasoner.mode().as_str().to_owned()),
            (
                "reasoner.rules",
                self.reasoner
                    .rules
                    .as_ref()
                    .map_or_else(|| "(none)".to_owned(), |rules| rules.name().to_owned()),
            ),
            (
                "reasoner.unnamed_classes",
                if store.hide_unnamed_classes {
                    "skip"
                } else {
                    "derive"
                }
                .to_owned(),
            ),
            (
                "reasoner.ql_rewriting",
                store.ql_rewriting.name().to_owned(),
            ),
            ("reasoner.support_sets", store.support_sets.to_string()),
            (
                "reasoner.semantics",
                nrese_reasoner::ReasonerService::new(self.reasoner.clone())
                    .semantics()
                    .unwrap_or_else(|| "(none)".to_owned()),
            ),
            ("policy.auth.mode", self.policy.auth.mode_name().to_owned()),
            ("budgets.query_memory", size(limits.max_query_memory_bytes)),
            (
                "budgets.total_query_memory",
                size(store.total_query_memory_bytes),
            ),
            (
                "budgets.bulk_load_memory",
                size(store.bulk_load_memory_bytes as usize),
            ),
            ("budgets.query_timeout", format!("{:?}", timeouts.query)),
            ("budgets.update_timeout", format!("{:?}", timeouts.update)),
            (
                "budgets.graph_read_timeout",
                format!("{:?}", timeouts.graph_read),
            ),
            (
                "budgets.graph_write_timeout",
                format!("{:?}", timeouts.graph_write),
            ),
            ("budgets.query_text", size(limits.max_query_bytes)),
            ("budgets.update_size", size(limits.max_update_bytes)),
            ("budgets.upload_size", size(limits.max_rdf_upload_bytes)),
            ("budgets.result_cache", size(store.query_cache_bytes)),
            ("policy.rate_limits.window", format!("{:?}", rates.window)),
            (
                "policy.rate_limits.read_requests_per_window",
                rates.read_requests_per_window.to_string(),
            ),
            (
                "policy.rate_limits.write_requests_per_window",
                rates.write_requests_per_window.to_string(),
            ),
            (
                "policy.rate_limits.admin_requests_per_window",
                rates.admin_requests_per_window.to_string(),
            ),
            ("ai.enabled", self.ai.enabled.to_string()),
            ("ai.provider", self.ai.provider.provider_name().to_owned()),
            ("ai.model", self.ai.model.clone()),
        ];
        lines
            .iter()
            .map(|(key, value)| format!("{key} = {value}\n"))
            .collect()
    }

    pub fn from_env() -> Result<Self> {
        Self::load(None)
    }

    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        Self::load_with(config_path, &[])
    }

    /// The configuration from the file, the environment and `overrides` (the command
    /// line's `--set key=value`), each over the one before ([`settings`]).
    pub fn load_with(config_path: Option<&Path>, overrides: &[(String, String)]) -> Result<Self> {
        Self::load_from(config_path, ProcessEnv, overrides)
    }

    /// [`Self::load_with`] with `env` as the environment.
    fn load_from(
        config_path: Option<&Path>,
        env: impl ConfigSource,
        overrides: &[(String, String)],
    ) -> Result<Self> {
        let file_source = match resolve_config_path(config_path, &env) {
            Some(path) => load_file_source(&path)?,
            None => KeyValueSource::default(),
        };
        let source = LayeredSource::new(
            LayeredSource::new(file_source, env),
            settings::from_overrides(overrides)?,
        );
        Self::from_source(&source)
    }

    /// The configuration from the file at `path` and `overrides` alone, whatever the
    /// process environment holds.
    #[cfg(test)]
    fn load_isolated(path: &Path, overrides: &[(String, String)]) -> Result<Self> {
        Self::load_from(Some(path), KeyValueSource::default(), overrides)
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
        let store = parse_store_config(source)?;
        let reasoner = parse_reasoner_config(source)?;
        let policy = parse_policy_config(source)?;
        let ai = parse_ai_config(source)?;
        let replication = replication_env::parse_replication_config(source)?;
        if replication.mode != crate::replication::ReplicationMode::Off
            && store.mode != nrese_store::StoreMode::OnDisk
        {
            anyhow::bail!("replication needs an on-disk store (store.mode = \"on-disk\")");
        }

        validate_configuration(deployment_posture, store.mode, &policy)
            .map_err(anyhow::Error::msg)?;

        Ok(Self {
            bind_address,
            deployment_posture,
            store,
            reasoner,
            policy,
            ai,
            replication,
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

    use super::source::KeyValueSource;
    use super::{CliConfig, DeploymentPosture, ServerConfig, env_names as names};

    /// An environment of `values` alone (the process environment untouched).
    fn env(values: &[(&str, &str)]) -> KeyValueSource {
        let mut source = KeyValueSource::default();
        for (key, value) in values {
            source.insert(*key, *value);
        }
        source
    }

    #[test]
    fn server_config_loads_from_file() {
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
mode = "owl2-rl"

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

        let config = ServerConfig::load_from(Some(&path), env(&[]), &[]).expect("server config");

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
        let environment = env(&[
            (names::BIND_ADDR, "127.0.0.1:9898"),
            (names::REASONING_MODE, "disabled"),
        ]);
        let temp_dir = tempdir().expect("temp dir");
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[server]
bind_address = "0.0.0.0:9191"
deployment_posture = "open-workbench"

[reasoner]
mode = "owl2-rl"

[auth]
mode = "none"
"#,
        )
        .expect("config file");

        let config = ServerConfig::load_from(Some(&path), environment, &[]).expect("server config");

        assert_eq!(config.bind_address.to_string(), "127.0.0.1:9898");
        assert_eq!(config.reasoner.mode(), ReasoningMode::Disabled);
    }

    #[test]
    fn config_path_can_be_selected_from_env() {
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
        let environment = env(&[(names::CONFIG_PATH, path.to_string_lossy().as_ref())]);

        let config = ServerConfig::load_from(None, environment, &[]).expect("server config");

        assert_eq!(config.bind_address.to_string(), "127.0.0.1:9393");
    }

    #[test]
    fn cli_parser_rejects_unknown_flags() {
        assert!(CliConfig::from_args(["nrese-server".into(), "--verbose".into()]).is_err());
    }

    #[test]
    fn deployment_posture_rejects_unauthenticated_internal_mode() {
        let environment = env(&[
            (names::DEPLOYMENT_POSTURE, "internal-authenticated"),
            (names::AUTH_MODE, "none"),
        ]);

        assert!(ServerConfig::from_source(&environment).is_err());
    }
}
