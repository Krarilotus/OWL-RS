use axum::Router;
use axum::routing::{get, get_service, post};
use tower_http::services::ServeDir;

use crate::http::console;
use crate::http::handlers;
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
        .route(
            "/dataset/query",
            get(handlers::query_get).post(handlers::query_post),
        )
        .route("/dataset/update", post(handlers::update_post))
        .route("/dataset/tell", post(handlers::tell_post))
        .route(
            "/dataset/data",
            get(handlers::graph_get)
                .head(handlers::graph_head)
                .put(handlers::graph_put)
                .post(handlers::graph_post)
                .delete(handlers::graph_delete),
        )
        .nest_service(
            "/console/assets",
            get_service(ServeDir::new(console::assets_dir())),
        )
        .with_state(state)
}
