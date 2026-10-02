use axum::Router;
use axum::routing::{get, post, put};

use crate::http::handlers;
use crate::http::rdf4j;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(handlers::root_redirect))
        .route("/console", get(handlers::console_ui))
        .route("/api/ai/status", get(handlers::ai_status))
        .route(
            "/api/ai/query-suggestions",
            post(handlers::ai_query_suggestions),
        )
        .route("/ops", get(handlers::operator_ui))
        .route("/ui", get(handlers::operator_ui))
        .route(
            "/ops/api/capabilities",
            get(handlers::operator_capabilities),
        )
        .route(
            "/ops/api/dataset/summary",
            get(handlers::operator_dataset_summary),
        )
        .route(
            "/ops/api/health/extended",
            get(handlers::operator_extended_health),
        )
        .route(
            "/ops/api/diagnostics/runtime",
            get(handlers::operator_runtime_diagnostics),
        )
        .route(
            "/ops/api/diagnostics/reasoning",
            get(handlers::operator_reasoning_diagnostics),
        )
        .route(
            "/ops/api/admin/dataset/backup",
            get(handlers::admin_backup_dataset),
        )
        .route(
            "/ops/api/admin/dataset/restore",
            post(handlers::admin_restore_dataset),
        )
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route("/version", get(handlers::version))
        .route("/metrics", get(handlers::metrics))
        .route(
            "/dataset/service-description",
            get(handlers::service_description),
        )
        .route("/dataset/info", get(handlers::dataset_info))
        .route("/dataset/autocomplete", get(handlers::autocomplete))
        .route(
            "/dataset/query",
            get(handlers::query_get).post(handlers::query_post),
        )
        .route("/dataset/update", post(handlers::update_post))
        // One URL for queries and updates, for clients configured with a single endpoint.
        .route(
            crate::runtime_posture::SPARQL_ENDPOINT,
            get(handlers::sparql_get).post(handlers::sparql_post),
        )
        .route(
            "/dataset",
            get(handlers::sparql_get).post(handlers::sparql_post),
        )
        .route("/dataset/tell", post(handlers::tell_post))
        .route(
            "/dataset/data",
            get(handlers::graph_get)
                .head(handlers::graph_head)
                .put(handlers::graph_put)
                .post(handlers::graph_post)
                .delete(handlers::graph_delete),
        )
        .route(
            crate::runtime_posture::SHACL_ENDPOINT,
            get(handlers::shacl_get).post(handlers::shacl_post),
        )
        .route("/dataset/classification", get(handlers::classification_get))
        .route("/console/{*path}", get(handlers::console_file))
        // The RDF4J REST protocol (`rdf4j.rs`).
        .route("/protocol", get(rdf4j::protocol))
        .route("/repositories", get(rdf4j::repositories))
        .route(
            "/repositories/{id}",
            get(rdf4j::query_get).post(rdf4j::query_post),
        )
        .route(
            "/repositories/{id}/statements",
            get(rdf4j::statements_get)
                .post(rdf4j::statements_post)
                .put(rdf4j::statements_put)
                .delete(rdf4j::statements_delete),
        )
        .route("/repositories/{id}/size", get(rdf4j::size))
        .route("/repositories/{id}/contexts", get(rdf4j::contexts_get))
        .route(
            "/repositories/{id}/namespaces",
            get(rdf4j::namespaces_get).delete(rdf4j::namespaces_delete),
        )
        .route(
            "/repositories/{id}/namespaces/{prefix}",
            get(rdf4j::namespace_get)
                .put(rdf4j::namespace_put)
                .delete(rdf4j::namespace_delete),
        )
        .route(
            "/repositories/{id}/rdf-graphs/service",
            get(rdf4j::graph_store_get)
                .put(rdf4j::graph_store_put)
                .post(rdf4j::graph_store_post)
                .delete(rdf4j::graph_store_delete),
        )
        .route(
            "/repositories/{id}/transactions",
            post(rdf4j::transaction_begin),
        )
        .route(
            "/repositories/{id}/transactions/{txid}",
            put(rdf4j::transaction_action)
                .post(rdf4j::transaction_action)
                .delete(rdf4j::transaction_rollback),
        )
        .with_state(state)
}
