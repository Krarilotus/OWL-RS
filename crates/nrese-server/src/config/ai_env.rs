use std::time::Duration;

use anyhow::{Result, bail};

use crate::ai::{AiConfig, AiProviderConfig, GeminiConfig, OpenRouterConfig};

use super::env_names as names;
use super::env_values::{parse_bool, parse_u64, parse_usize};
use super::source::ConfigSource;

const DEFAULT_GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com";
const DEFAULT_OPENROUTER_API_BASE: &str = "https://openrouter.ai/api/v1";

pub(super) fn parse_ai_config(source: &dyn ConfigSource) -> Result<AiConfig> {
    let enabled = parse_bool(source, names::AI_ENABLED, false)?;
    let provider_name = source
        .get(names::AI_PROVIDER)
        .unwrap_or_else(|| "disabled".to_owned());
    let model = source
        .get(names::AI_MODEL)
        .unwrap_or_else(|| "gemini-2.5-flash".to_owned());
    let request_timeout = Duration::from_millis(parse_u64(source, names::AI_TIMEOUT_MS, 20_000)?);
    let max_suggestions = parse_usize(source, names::AI_MAX_SUGGESTIONS, 4)?;
    let system_prompt = source.get(names::AI_SYSTEM_PROMPT).unwrap_or_else(|| {
        "You generate concise SPARQL query ideas for an RDF and OWL knowledge graph. Return strict JSON only. Prefer safe read queries. Use prefixes only when you can justify them from the prompt or context. If uncertain, use full IRIs.".to_owned()
    });

    let provider = match provider_name.as_str() {
        "disabled" => AiProviderConfig::Disabled,
        "gemini" => {
            let api_key = source
                .get(names::AI_GOOGLE_API_KEY)
                .or_else(|| source.get("GOOGLE_API_KEY"))
                .ok_or_else(|| anyhow::anyhow!("missing Gemini API key"))?;
            let api_base = source
                .get(names::AI_GOOGLE_API_BASE)
                .unwrap_or_else(|| DEFAULT_GEMINI_API_BASE.to_owned());
            AiProviderConfig::Gemini(GeminiConfig { api_key, api_base })
        }
        "openrouter" => {
            let api_key = source
                .get(names::AI_OPENROUTER_API_KEY)
                .ok_or_else(|| anyhow::anyhow!("missing OpenRouter API key"))?;
            let api_base = source
                .get(names::AI_OPENROUTER_API_BASE)
                .unwrap_or_else(|| DEFAULT_OPENROUTER_API_BASE.to_owned());
            AiProviderConfig::OpenRouter(OpenRouterConfig {
                api_key,
                api_base,
                site_url: source.get(names::AI_OPENROUTER_SITE_URL),
                app_name: source.get(names::AI_OPENROUTER_APP_NAME),
            })
        }
        other => bail!("unsupported AI provider: {other}"),
    };

    Ok(AiConfig {
        enabled,
        provider: if enabled {
            provider
        } else {
            AiProviderConfig::Disabled
        },
        model,
        request_timeout,
        max_suggestions,
        system_prompt,
    })
}

#[cfg(test)]
mod tests {
    use crate::config::source::KeyValueSource;

    use super::*;

    #[test]
    fn parses_disabled_ai_config_by_default() {
        let source = KeyValueSource::default();

        let config = parse_ai_config(&source).expect("ai config");

        assert!(!config.enabled);
        assert!(matches!(config.provider, AiProviderConfig::Disabled));
    }

    #[test]
    fn parses_gemini_config_with_google_api_key_fallback() {
        let mut source = KeyValueSource::default();
        source.insert(names::AI_ENABLED, "true");
        source.insert(names::AI_PROVIDER, "gemini");
        source.insert("GOOGLE_API_KEY", "local-key");

        let config = parse_ai_config(&source).expect("ai config");

        match config.provider {
            AiProviderConfig::Gemini(provider) => assert_eq!(provider.api_key, "local-key"),
            other => panic!("expected gemini provider, got {other:?}"),
        }
    }
}
