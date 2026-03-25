use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct QuerySuggestionRequest {
    pub prompt: String,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub current_query: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuerySuggestion {
    pub title: String,
    pub explanation: String,
    pub sparql: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuerySuggestionResponse {
    pub provider: &'static str,
    pub model: String,
    pub suggestions: Vec<QuerySuggestion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiStatusResponse {
    pub enabled: bool,
    pub provider: &'static str,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SuggestionContext {
    pub locale: String,
    pub prompt: String,
    pub current_query: Option<String>,
    pub quad_count: u64,
    pub named_graph_count: usize,
    pub reasoning_mode: &'static str,
    pub reasoning_profile: &'static str,
}
