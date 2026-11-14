mod client;
mod config;
mod gemini;
mod json;
mod openrouter;
mod prompt;
mod service;
mod types;

pub use config::{
    AiConfig, AiProviderConfig, DEFAULT_SYSTEM_PROMPT, GeminiConfig, OpenRouterConfig,
};
pub use service::AiSuggestionService;
pub(crate) use types::SuggestionContext;
pub use types::{AiStatusResponse, QuerySuggestionRequest, QuerySuggestionResponse};
