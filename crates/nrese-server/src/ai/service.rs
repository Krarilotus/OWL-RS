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
        // No redirects: a provider's key travels in headers (Gemini's `x-goog-api-key` is
        // one reqwest keeps across origins), and a hop elsewhere must not take it along.
        let http = Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
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
    use std::sync::{Arc, Mutex};

    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;

    use super::AiSuggestionService;
    use crate::ai::config::{AiConfig, AiProviderConfig, GeminiConfig};
    use crate::ai::types::SuggestionContext;

    #[test]
    fn disabled_status_reports_no_model() {
        let service = AiSuggestionService::disabled();
        let status = service.status();

        assert!(!status.enabled);
        assert_eq!(status.provider, "disabled");
        assert!(status.model.is_none());
    }

    /// A provider that redirects elsewhere doesn't get its API key sent there: the client
    /// follows no redirect.
    #[tokio::test]
    async fn a_redirect_does_not_take_the_api_key_elsewhere() {
        let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = format!("http://{}/", elsewhere.local_addr().unwrap());
        let record = Arc::clone(&seen);
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(move |headers: HeaderMap| {
                let record = Arc::clone(&record);
                async move {
                    let key = headers.get("x-goog-api-key").map(|key| format!("{key:?}"));
                    record.lock().unwrap().push(key);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            });
            axum::serve(elsewhere, app).await.unwrap();
        });
        let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_base = format!("http://{}", provider.local_addr().unwrap());
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(move || {
                let target = target.clone();
                async move {
                    (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, target)]).into_response()
                }
            });
            axum::serve(provider, app).await.unwrap();
        });
        let service = AiSuggestionService::new(AiConfig {
            enabled: true,
            provider: AiProviderConfig::Gemini(GeminiConfig {
                api_key: "secret-key".to_owned(),
                api_base,
            }),
            ..AiConfig::default()
        })
        .unwrap();
        let outcome = service
            .suggest(SuggestionContext {
                locale: "en".to_owned(),
                prompt: "classes".to_owned(),
                current_query: None,
                quad_count: 0,
                named_graph_count: 0,
                reasoning_mode: "disabled",
                reasoning_profile: "none",
                reasoning_read_model: "asserted",
            })
            .await;
        assert!(outcome.is_err());
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
    }
}
