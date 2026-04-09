use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawFileConfig {
    #[serde(default)]
    pub server: RawServerConfig,
    #[serde(default)]
    pub store: RawStoreConfig,
    #[serde(default)]
    pub reasoner: RawReasonerConfig,
    #[serde(default)]
    pub policy: RawPolicyConfig,
    #[serde(default)]
    pub auth: RawAuthConfig,
    #[serde(default)]
    pub ai: RawAiConfig,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawServerConfig {
    #[serde(default, alias = "bind_addr")]
    pub bind_address: Option<String>,
    #[serde(default)]
    pub deployment_posture: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawStoreConfig {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub ontology_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawReasonerConfig {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub read_model: Option<String>,
    #[serde(default)]
    pub rules_mvp: RawRulesMvpConfig,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawRulesMvpConfig {
    #[serde(default, alias = "tier")]
    pub preset: Option<String>,
    #[serde(default)]
    pub features: Option<StringOrMany>,
}

#[derive(Debug, Default, Deserialize)]
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
pub(super) struct RawLimitsConfig {
    #[serde(default)]
    pub max_query_bytes: Option<usize>,
    #[serde(default)]
    pub max_update_bytes: Option<usize>,
    #[serde(default)]
    pub max_rdf_upload_bytes: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
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
pub(super) struct RawExposureConfig {
    #[serde(default)]
    pub operator_ui: Option<bool>,
    #[serde(default)]
    pub metrics: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
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
}

#[derive(Debug, Default, Deserialize)]
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
pub(super) struct RawAiGeminiConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
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
pub(super) struct RawBearerStaticConfig {
    #[serde(default)]
    pub read_token: Option<String>,
    #[serde(default)]
    pub admin_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
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
pub(super) struct RawMtlsConfig {
    #[serde(default)]
    pub subject_header: Option<String>,
    #[serde(default)]
    pub read_subjects: Option<StringOrMany>,
    #[serde(default)]
    pub admin_subjects: Option<StringOrMany>,
}

#[derive(Debug, Default, Deserialize)]
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
