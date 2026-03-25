use anyhow::Result;
use async_trait::async_trait;

use crate::ai::types::{QuerySuggestionResponse, SuggestionContext};

#[async_trait]
pub trait AiProviderClient: Send + Sync {
    async fn suggest(
        &self,
        context: &SuggestionContext,
        system_prompt: &str,
        model: &str,
        max_suggestions: usize,
    ) -> Result<QuerySuggestionResponse>;
}
