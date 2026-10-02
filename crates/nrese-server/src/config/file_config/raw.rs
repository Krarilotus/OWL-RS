use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawFileConfig {
    #[serde(default)]
    pub server: RawServerConfig,
    #[serde(default)]
    pub store: RawStoreConfig,
    #[serde(default)]
    pub reasoner: RawReasonerConfig,
    #[serde(default)]
    pub shacl: RawShaclConfig,
    #[serde(default)]
    pub policy: RawPolicyConfig,
    #[serde(default)]
    pub budgets: RawBudgetsConfig,
    #[serde(default)]
    pub federation: RawFederationConfig,
    #[serde(default)]
    pub auth: RawAuthConfig,
    #[serde(default)]
    pub ai: RawAiConfig,
}

/// A number, or a number with a unit as text (`"4GiB"`, `"30s"`, `"50%"`).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum Amount {
    Number(u64),
    Text(String),
}

impl Amount {
    pub(super) fn into_text(self) -> String {
        match self {
            Self::Number(number) => number.to_string(),
            Self::Text(text) => text,
        }
    }
}

/// Every resource budget in one place. Each key has an older name in `[policy.limits]`,
/// `[policy.timeouts]` or `[store]`, which still works; setting both is an error.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawBudgetsConfig {
    #[serde(default)]
    pub query_memory: Option<Amount>,
    #[serde(default)]
    pub total_query_memory: Option<Amount>,
    #[serde(default)]
    pub bulk_load_memory: Option<Amount>,
    #[serde(default)]
    pub query_timeout: Option<Amount>,
    #[serde(default)]
    pub update_timeout: Option<Amount>,
    #[serde(default)]
    pub graph_read_timeout: Option<Amount>,
    #[serde(default)]
    pub graph_write_timeout: Option<Amount>,
    #[serde(default)]
    pub query_text: Option<Amount>,
    #[serde(default)]
    pub update_size: Option<Amount>,
    #[serde(default)]
    pub upload_size: Option<Amount>,
    #[serde(default)]
    pub result_cache: Option<Amount>,
}

/// `SERVICE`: the endpoints it may call, and per request how long and how many rows.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawFederationConfig {
    #[serde(default)]
    pub allow: Option<StringOrMany>,
    #[serde(default)]
    pub timeout: Option<Amount>,
    #[serde(default)]
    pub max_rows: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawShaclConfig {
    #[serde(default)]
    pub shapes_graph: Option<String>,
    #[serde(default)]
    pub gate: Option<String>,
    #[serde(default)]
    pub gate_severity: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawServerConfig {
    #[serde(default, alias = "bind_addr")]
    pub bind_address: Option<String>,
    #[serde(default)]
    pub deployment_posture: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawStoreConfig {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub ontology_path: Option<String>,
    #[serde(default)]
    pub query_cache_bytes: Option<usize>,
    #[serde(default)]
    pub default_graph: Option<String>,
    #[serde(default)]
    pub verify_on_open: Option<bool>,
    #[serde(default)]
    pub map_checkpoints: Option<bool>,
    #[serde(default)]
    pub wal_archive: Option<bool>,
    #[serde(default)]
    pub index_encoding: Option<String>,
    #[serde(default)]
    pub vocabulary: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawReasonerConfig {
    #[serde(default)]
    pub mode: Option<String>,
    /// A Notation3 rules file: user rules, added to the mode's ruleset (or the whole
    /// program in the `custom` mode).
    #[serde(default)]
    pub rules: Option<String>,
    #[serde(default)]
    pub unnamed_classes: Option<String>,
    #[serde(default)]
    pub equality: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPolicyConfig {
    #[serde(default)]
    pub limits: RawLimitsConfig,
    #[serde(default)]
    pub rate_limits: RawRateLimitConfig,
    #[serde(default)]
    pub timeouts: RawTimeoutConfig,
    #[serde(default)]
    pub sparql_parse_error_profile: Option<String>,
    #[serde(default)]
    pub exposure: RawExposureConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawLimitsConfig {
    #[serde(default)]
    pub max_query_bytes: Option<usize>,
    #[serde(default)]
    pub max_query_memory_bytes: Option<usize>,
    #[serde(default)]
    pub max_update_bytes: Option<usize>,
    #[serde(default)]
    pub max_rdf_upload_bytes: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRateLimitConfig {
    #[serde(default)]
    pub window_secs: Option<u64>,
    #[serde(default)]
    pub read_requests_per_window: Option<usize>,
    #[serde(default)]
    pub write_requests_per_window: Option<usize>,
    #[serde(default)]
    pub admin_requests_per_window: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawTimeoutConfig {
    #[serde(default)]
    pub query_ms: Option<u64>,
    #[serde(default)]
    pub update_ms: Option<u64>,
    #[serde(default)]
    pub graph_read_ms: Option<u64>,
    #[serde(default)]
    pub graph_write_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawExposureConfig {
    #[serde(default)]
    pub operator_ui: Option<bool>,
    #[serde(default)]
    pub metrics: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAuthConfig {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub bearer_static: RawBearerStaticConfig,
    #[serde(default)]
    pub bearer_jwt: RawBearerJwtConfig,
    #[serde(default)]
    pub mtls: RawMtlsConfig,
    #[serde(default)]
    pub oidc_introspection: RawOidcIntrospectionConfig,
    /// The graph access policy file ([`crate::access`]).
    #[serde(default)]
    pub access_policy: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAiConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_suggestions: Option<usize>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub gemini: RawAiGeminiConfig,
    #[serde(default)]
    pub openrouter: RawAiOpenRouterConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAiGeminiConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAiOpenRouterConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub site_url: Option<String>,
    #[serde(default)]
    pub app_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawBearerStaticConfig {
    #[serde(default)]
    pub read_token: Option<String>,
    #[serde(default)]
    pub admin_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawBearerJwtConfig {
    #[serde(default)]
    pub shared_secret: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub read_role: Option<String>,
    #[serde(default)]
    pub admin_role: Option<String>,
    #[serde(default)]
    pub leeway_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawMtlsConfig {
    #[serde(default)]
    pub subject_header: Option<String>,
    #[serde(default)]
    pub read_subjects: Option<StringOrMany>,
    #[serde(default)]
    pub admin_subjects: Option<StringOrMany>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawOidcIntrospectionConfig {
    #[serde(default)]
    pub introspection_url: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub read_role: Option<String>,
    #[serde(default)]
    pub admin_role: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum StringOrMany {
    One(String),
    Many(Vec<String>),
}

impl StringOrMany {
    pub(super) fn join(&self, delimiter: &str) -> String {
        match self {
            Self::One(value) => value.clone(),
            Self::Many(values) => values.join(delimiter),
        }
    }
}
