use axum::Router;
use axum::extract::DefaultBodyLimit;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::http::request_metrics::track;
use crate::http::router;
use crate::state::AppState;

pub fn build_app(state: AppState) -> Router {
    // The policy's limits decide what is too large, per kind of request and with a
    // problem document. The framework's own body limit (2 MiB unless set) only has to
    // stay out of their way: the largest policy limit, tripled because a form-encoded
    // body can be three times its content.
    let limits = &state.policy().limits;
    let largest = limits
        .max_query_bytes
        .max(limits.max_update_bytes)
        .max(limits.max_rdf_upload_bytes);
    let body_limit = largest.saturating_mul(3).saturating_add(64 * 1024);
    let metrics = axum::middleware::from_fn_with_state(state.clone(), track);
    router(state)
        .layer(metrics)
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
}
