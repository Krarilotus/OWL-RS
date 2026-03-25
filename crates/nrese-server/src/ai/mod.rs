mod client;
mod config;
mod gemini;
mod json;
mod openrouter;
mod prompt;
mod service;
mod types;

pub use config::{AiConfig, AiProviderConfig, GeminiConfig, OpenRouterConfig};
pub use service::AiSuggestionService;
pub(crate) use types::SuggestionContext;
pub use types::{AiStatusResponse, QuerySuggestionRequest, QuerySuggestionResponse};
