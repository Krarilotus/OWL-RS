use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::ai::client::AiProviderClient;
use crate::ai::config::GeminiConfig;
use crate::ai::json::parse_suggestion_json;
use crate::ai::prompt::build_query_suggestion_prompt;
use crate::ai::types::{QuerySuggestionResponse, SuggestionContext};

pub struct GeminiClient {
    http: Client,
    config: GeminiConfig,
}

impl GeminiClient {
    pub fn new(http: Client, config: GeminiConfig) -> Self {
        Self { http, config }
    }
}

#[async_trait]
impl AiProviderClient for GeminiClient {
    async fn suggest(
        &self,
        context: &SuggestionContext,
        system_prompt: &str,
        model: &str,
        max_suggestions: usize,
    ) -> Result<QuerySuggestionResponse> {
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.config.api_base.trim_end_matches('/'),
            model
        );
        let request = GeminiGenerateContentRequest {
            system_instruction: GeminiContent {
                parts: vec![GeminiPart {
                    text: system_prompt.to_owned(),
                }],
            },
            contents: vec![GeminiContent {
                parts: vec![GeminiPart {
                    text: build_query_suggestion_prompt(context, max_suggestions),
                }],
            }],
            generation_config: GeminiGenerationConfig {
                response_mime_type: "application/json".to_owned(),
            },
        };

        let response = self
            .http
            .post(url)
            .header("x-goog-api-key", &self.config.api_key)
            .json(&request)
            .send()
            .await
            .context("gemini request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("gemini request failed with status {status}: {body}");
        }

        let payload: GeminiGenerateContentResponse = response
            .json()
            .await
            .context("failed to parse gemini response")?;
        let raw_json = payload
            .candidates
            .first()
            .and_then(|candidate| candidate.content.parts.first())
            .map(|part| part.text.as_str())
            .ok_or_else(|| anyhow!("gemini response did not contain candidate text"))?;

        parse_suggestion_json("gemini", model, raw_json)
    }
}
#[derive(Debug, Serialize)]
struct GeminiGenerateContentRequest {
    system_instruction: GeminiContent,
    contents: Vec<GeminiContent>,
    generation_config: GeminiGenerationConfig,
}

#[derive(Debug, Serialize)]
struct GeminiGenerationConfig {
    response_mime_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiPart {
    text: String,
}

#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: GeminiContent,
}

#[cfg(test)]
mod tests {
    use super::parse_suggestion_json;

    #[test]
    fn parses_gemini_json_suggestion_payload() {
        let response = parse_suggestion_json(
            "gemini",
            "gemini-2.5-flash",
            r#"{"suggestions":[{"title":"Find classes","explanation":"Lists common rdf:type values.","sparql":"SELECT ?type (COUNT(*) AS ?count) WHERE { ?s a ?type } GROUP BY ?type ORDER BY DESC(?count) LIMIT 10"}]}"#,
        )
        .expect("response");

        assert_eq!(response.provider, "gemini");
        assert_eq!(response.suggestions.len(), 1);
    }
}
