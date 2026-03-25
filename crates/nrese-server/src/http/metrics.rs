use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

pub fn render(state: &AppState) -> Result<Response, ApiError> {
    let ready = if state.is_ready() { 1 } else { 0 };
    let stats = state
        .store()
        .stats()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let body = format!(
        "# HELP nrese_ready Whether the server is ready.\n\
# TYPE nrese_ready gauge\n\
nrese_ready {ready}\n\
# HELP nrese_dataset_revision Current dataset revision.\n\
# TYPE nrese_dataset_revision gauge\n\
nrese_dataset_revision {}\n\
# HELP nrese_store_quads Number of quads in the active dataset.\n\
# TYPE nrese_store_quads gauge\n\
nrese_store_quads {}\n\
# HELP nrese_store_named_graphs Number of named graphs in the active dataset.\n\
# TYPE nrese_store_named_graphs gauge\n\
nrese_store_named_graphs {}\n\
# HELP nrese_reasoner_mode_info Active reasoner mode metadata.\n\
# TYPE nrese_reasoner_mode_info gauge\n\
nrese_reasoner_mode_info{{mode=\"{}\",profile=\"{}\"}} 1\n\
# HELP nrese_store_mode_info Active store mode metadata.\n\
# TYPE nrese_store_mode_info gauge\n\
nrese_store_mode_info{{mode=\"{}\",durability=\"{}\"}} 1\n",
        state.store().current_revision(),
        stats.quad_count,
        stats.named_graph_count,
        state.reasoner_mode_name(),
        state.reasoner_profile_name(),
        state.store_mode_name(),
        state.durability_name(),
    );

    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use crate::ai::AiSuggestionService;
    use axum::body::to_bytes;
    use nrese_reasoner::{ReasonerConfig, ReasonerService};
    use nrese_store::{StoreConfig, StoreService};

    use crate::policy::PolicyConfig;
    use crate::state::AppState;

    use super::render;

    #[tokio::test]
    async fn metrics_output_contains_stable_metric_names() {
        let store = StoreService::new(StoreConfig::default()).expect("store should initialize");
        let reasoner = ReasonerService::new(ReasonerConfig::default());
        let state = AppState::new(
            store,
            reasoner,
            PolicyConfig::default(),
            AiSuggestionService::disabled(),
        );
        state.mark_ready();

        let response = render(&state).expect("metrics should render");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let text = String::from_utf8(body.to_vec()).expect("metrics should be utf-8");

        assert!(text.contains("nrese_ready"));
        assert!(text.contains("nrese_dataset_revision"));
        assert!(text.contains("nrese_store_quads"));
        assert!(text.contains("nrese_store_mode_info"));
    }
}
