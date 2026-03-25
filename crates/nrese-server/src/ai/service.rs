use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::Client;

use crate::ai::client::AiProviderClient;
use crate::ai::config::{AiConfig, AiProviderConfig};
use crate::ai::gemini::GeminiClient;
use crate::ai::openrouter::OpenRouterClient;
use crate::ai::types::{AiStatusResponse, QuerySuggestionResponse, SuggestionContext};
use crate::error::ApiError;

#[derive(Clone)]
pub struct AiSuggestionService {
    config: AiConfig,
    backend: Option<Arc<dyn AiProviderClient>>,
}

impl AiSuggestionService {
    pub fn new(config: AiConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(config.request_timeout)
            .build()
            .context("failed to build AI HTTP client")?;

        let backend: Option<Arc<dyn AiProviderClient>> = match &config.provider {
            AiProviderConfig::Disabled => None,
            AiProviderConfig::Gemini(provider) => {
                Some(Arc::new(GeminiClient::new(http.clone(), provider.clone())))
            }
            AiProviderConfig::OpenRouter(provider) => {
                Some(Arc::new(OpenRouterClient::new(http, provider.clone())))
            }
        };

        Ok(Self { config, backend })
    }

    pub fn disabled() -> Self {
        Self {
            config: AiConfig::default(),
            backend: None,
        }
    }

    pub fn status(&self) -> AiStatusResponse {
        AiStatusResponse {
            enabled: self.config.enabled && self.backend.is_some(),
            provider: self.config.provider.provider_name(),
            model: if self.config.enabled {
                Some(self.config.model.clone())
            } else {
                None
            },
        }
    }

    pub async fn suggest(
        &self,
        context: SuggestionContext,
    ) -> Result<QuerySuggestionResponse, ApiError> {
        if !self.config.enabled {
            return Err(ApiError::not_found(
                "AI query suggestions are disabled in the current runtime configuration",
            ));
        }
        let backend = self.backend.as_ref().ok_or_else(|| {
            ApiError::unavailable(
                "AI query suggestions are enabled but no provider backend is available",
            )
        })?;
        backend
            .suggest(
                &context,
                &self.config.system_prompt,
                &self.config.model,
                self.config.max_suggestions,
            )
            .await
            .map_err(|error| ApiError::unavailable(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::AiSuggestionService;

    #[test]
    fn disabled_status_reports_no_model() {
        let service = AiSuggestionService::disabled();
        let status = service.status();

        assert!(!status.enabled);
        assert_eq!(status.provider, "disabled");
        assert!(status.model.is_none());
    }
}
