use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::ai::types::{QuerySuggestion, QuerySuggestionResponse};

pub fn parse_suggestion_json(
    provider: &'static str,
    model: &str,
    raw_json: &str,
) -> Result<QuerySuggestionResponse> {
    let parsed: SuggestionEnvelope = serde_json::from_str(raw_json)
        .with_context(|| format!("failed to parse {provider} suggestion json"))?;
    if parsed.suggestions.is_empty() {
        bail!("{provider} returned no suggestions");
    }

    Ok(QuerySuggestionResponse {
        provider,
        model: model.to_owned(),
        suggestions: parsed.suggestions,
    })
}

#[derive(Debug, Deserialize)]
struct SuggestionEnvelope {
    suggestions: Vec<QuerySuggestion>,
}
