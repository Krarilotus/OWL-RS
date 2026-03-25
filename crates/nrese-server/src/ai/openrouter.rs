use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::ai::client::AiProviderClient;
use crate::ai::config::OpenRouterConfig;
use crate::ai::json::parse_suggestion_json;
use crate::ai::prompt::build_query_suggestion_prompt;
use crate::ai::types::{QuerySuggestionResponse, SuggestionContext};

pub struct OpenRouterClient {
    http: Client,
    config: OpenRouterConfig,
}

impl OpenRouterClient {
    pub fn new(http: Client, config: OpenRouterConfig) -> Self {
        Self { http, config }
    }
}

#[async_trait]
impl AiProviderClient for OpenRouterClient {
    async fn suggest(
        &self,
        context: &SuggestionContext,
        system_prompt: &str,
        model: &str,
        max_suggestions: usize,
    ) -> Result<QuerySuggestionResponse> {
        let url = format!(
            "{}/chat/completions",
            self.config.api_base.trim_end_matches('/')
        );
        let request = OpenRouterChatRequest {
            model: model.to_owned(),
            response_format: OpenRouterResponseFormat {
                r#type: "json_object".to_owned(),
            },
            messages: vec![
                OpenRouterMessage {
                    role: "system".to_owned(),
                    content: system_prompt.to_owned(),
                },
                OpenRouterMessage {
                    role: "user".to_owned(),
                    content: build_query_suggestion_prompt(context, max_suggestions),
                },
            ],
        };

        let mut request = self
            .http
            .post(url)
            .bearer_auth(&self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&request);
        if let Some(site_url) = &self.config.site_url {
            request = request.header("HTTP-Referer", site_url);
        }
        if let Some(app_name) = &self.config.app_name {
            request = request.header("X-Title", app_name);
        }

        let response = request.send().await.context("openrouter request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("openrouter request failed with status {status}: {body}");
        }

        let payload: OpenRouterChatResponse = response
            .json()
            .await
            .context("failed to parse openrouter response")?;
        let raw_json = payload
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .ok_or_else(|| anyhow!("openrouter response did not contain message content"))?;

        parse_suggestion_json("openrouter", model, raw_json)
    }
}

#[derive(Debug, Serialize)]
struct OpenRouterChatRequest {
    model: String,
    response_format: OpenRouterResponseFormat,
    messages: Vec<OpenRouterMessage>,
}

#[derive(Debug, Serialize)]
struct OpenRouterResponseFormat {
    r#type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenRouterMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterChatResponse {
    choices: Vec<OpenRouterChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterChoice {
    message: OpenRouterMessageContent,
}

#[derive(Debug, Deserialize)]
struct OpenRouterMessageContent {
    content: Option<String>,
}
