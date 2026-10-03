use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

/// The process's resident memory, where the OS tells (Linux).
fn resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

pub fn render(state: &AppState) -> Result<Response, ApiError> {
    let ready = if state.is_ready() { 1 } else { 0 };
    let stats = state
        .store()
        .stats()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let cache = state.store().query_cache_stats();
    let mut body = format!(
        "# HELP nrese_ready Whether the server is ready.\n\
# TYPE nrese_ready gauge\n\
nrese_ready {ready}\n\
# HELP nrese_dataset_revision Current dataset revision.\n\
# TYPE nrese_dataset_revision gauge\n\
nrese_dataset_revision {}\n\
# HELP nrese_store_quads Number of quads in the active dataset.\n\
# TYPE nrese_store_quads gauge\n\
nrese_store_quads {}\n\
# HELP nrese_store_inferred Number of inferred statements (reasoner v2's inferred stack).\n\
# TYPE nrese_store_inferred gauge\n\
nrese_store_inferred {}\n\
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
        stats.inferred_count,
        stats.named_graph_count,
        state.reasoner_mode_name(),
        state.reasoner_profile_name(),
        state.store_mode_name(),
        state.durability_name(),
    );
    body.push_str(&format!(
        "# HELP nrese_query_cache_hits_total Queries answered from the result cache.
# TYPE nrese_query_cache_hits_total counter
nrese_query_cache_hits_total {}
# HELP nrese_query_cache_misses_total Cacheable queries evaluated.
# TYPE nrese_query_cache_misses_total counter
nrese_query_cache_misses_total {}
# HELP nrese_query_cache_bytes Bytes held by the query result cache.
# TYPE nrese_query_cache_bytes gauge
nrese_query_cache_bytes {}
",
        cache.hits, cache.misses, cache.bytes,
    ));
    if let Some((used, peak, limit)) = state.store().query_memory() {
        body.push_str(&format!(
            "# HELP nrese_query_memory_bytes Intermediate results the running queries hold.
# TYPE nrese_query_memory_bytes gauge
nrese_query_memory_bytes {used}
# HELP nrese_query_memory_peak_bytes The most the running queries held at once.
# TYPE nrese_query_memory_peak_bytes gauge
nrese_query_memory_peak_bytes {peak}
# HELP nrese_query_memory_limit_bytes The budget for all running queries together.
# TYPE nrese_query_memory_limit_bytes gauge
nrese_query_memory_limit_bytes {limit}
",
        ));
    }
    // Where the memory goes: the indexes and the dictionary, on the heap and mapped from
    // the checkpoint, and what the process holds resident.
    let engine = state.store().engine_stats();
    body.push_str(&format!(
        "# HELP nrese_index_runs Index runs of both stacks.
# TYPE nrese_index_runs gauge
nrese_index_runs {}
# HELP nrese_index_bytes Index data by where it lives: heap, or mapped from the checkpoint.
# TYPE nrese_index_bytes gauge
nrese_index_bytes{{place=\"heap\"}} {}
nrese_index_bytes{{place=\"mapped\"}} {}
# HELP nrese_wal_bytes_since_checkpoint Bytes logged since the last checkpoint: what a restart replays.
# TYPE nrese_wal_bytes_since_checkpoint gauge
nrese_wal_bytes_since_checkpoint {}
# HELP nrese_compactions_total Merges of index runs since start.
# TYPE nrese_compactions_total counter
nrese_compactions_total {}
# HELP nrese_checkpoints_total Checkpoints written since start (a bulk load writes one).
# TYPE nrese_checkpoints_total counter
nrese_checkpoints_total {}
# HELP nrese_dictionary_terms Terms in the dictionary.
# TYPE nrese_dictionary_terms gauge
nrese_dictionary_terms {}
# HELP nrese_dictionary_bytes Dictionary data: the terms' text (heap and mapped), the heap's index, and all that is mapped from the checkpoint.
# TYPE nrese_dictionary_bytes gauge
nrese_dictionary_bytes{{part=\"text\"}} {}
nrese_dictionary_bytes{{part=\"heap_index\"}} {}
nrese_dictionary_bytes{{part=\"mapped\"}} {}
",
        engine.runs,
        engine.index_bytes,
        engine.index_mapped_bytes,
        engine.wal_bytes_since_checkpoint,
        engine.compactions,
        engine.checkpoints,
        engine.dictionary.terms,
        engine.dictionary.arena_bytes,
        engine.dictionary.index_bytes,
        engine.dictionary.mapped_bytes,
    ));
    if let Some(resident) = resident_bytes() {
        body.push_str(&format!(
            "# HELP nrese_process_resident_bytes Memory the process holds resident (file-backed pages included).
# TYPE nrese_process_resident_bytes gauge
nrese_process_resident_bytes {resident}
"
        ));
    }
    let support = state.store().support_statistics();
    body.push_str(&format!(
        "# HELP nrese_vector_index_bytes The vector index: vectors and their graphs.
# TYPE nrese_vector_index_bytes gauge
nrese_vector_index_bytes {}
# HELP nrese_support_sets_total Support graph sets (inferred = \"supported\") computed afresh or updated for a commit.
# TYPE nrese_support_sets_total counter
nrese_support_sets_total{{how=\"computed\"}} {}
nrese_support_sets_total{{how=\"updated\"}} {}
# HELP nrese_support_views_total Readers' views built, all and from the reader's earlier one.
# TYPE nrese_support_views_total counter
nrese_support_views_total{{how=\"built\"}} {}
nrese_support_views_total{{how=\"patched\"}} {}
# HELP nrese_support_seconds_total Time spent on support graph sets and views.
# TYPE nrese_support_seconds_total counter
nrese_support_seconds_total{{phase=\"compute\"}} {}
nrese_support_seconds_total{{phase=\"update\"}} {}
nrese_support_seconds_total{{phase=\"view\"}} {}
",
        engine.dictionary.vector_bytes,
        support.computed,
        support.updated,
        support.views,
        support.patched,
        support.computing_us as f64 / 1e6,
        support.updating_us as f64 / 1e6,
        support.viewing_us as f64 / 1e6,
    ));
    state.request_metrics().render(&mut body);

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
            crate::runtime_posture::DeploymentPosture::OpenWorkbench,
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
        assert!(text.contains("nrese_store_inferred"));
        assert!(text.contains("nrese_store_mode_info"));
        assert!(text.contains("nrese_reasoner_mode_info"));
        assert!(text.contains("nrese_query_cache_hits_total"));
        assert!(text.contains("nrese_index_bytes{place=\"mapped\"}"));
        assert!(text.contains("nrese_dictionary_terms"));
        assert!(text.contains("nrese_http_responses_total{kind=\"query\",status=\"2xx\"} 0"));
    }
}
