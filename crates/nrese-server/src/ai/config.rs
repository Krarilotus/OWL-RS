use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub enabled: bool,
    pub provider: AiProviderConfig,
    pub model: String,
    pub request_timeout: Duration,
    pub max_suggestions: usize,
    pub system_prompt: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: AiProviderConfig::Disabled,
            model: "gemini-2.5-flash".to_owned(),
            request_timeout: Duration::from_secs(20),
            max_suggestions: 4,
            system_prompt: "You generate concise SPARQL query ideas for an RDF and OWL knowledge graph. Return strict JSON only. Prefer safe read queries. Use prefixes only when you can justify them from the prompt or context. If uncertain, use full IRIs.".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AiProviderConfig {
    Disabled,
    Gemini(GeminiConfig),
    OpenRouter(OpenRouterConfig),
}

impl AiProviderConfig {
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Gemini(_) => "gemini",
            Self::OpenRouter(_) => "openrouter",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub api_key: String,
    pub api_base: String,
}

#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    pub api_key: String,
    pub api_base: String,
    pub site_url: Option<String>,
    pub app_name: Option<String>,
}
