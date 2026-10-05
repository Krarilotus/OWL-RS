use axum::Router;
use axum::extract::DefaultBodyLimit;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::http::problems::problems;
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
    let proxies = axum::middleware::from_fn_with_state(state.clone(), untrusted_subjects);
    router(state)
        .layer(proxies)
        .layer(metrics)
        .layer(DefaultBodyLimit::max(body_limit))
        // Inside the request id, so each problem document carries it.
        .layer(axum::middleware::from_fn(problems))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
}

/// In the `mtls` mode, drops the client certificate subject header from requests whose
/// peer isn't one of the trusted proxies (or is unknown: a router served without peer
/// addresses): only a proxy that terminated TLS may say who the client is.
async fn untrusted_subjects(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let crate::auth::AuthConfig::Mtls(config) = &state.policy().auth {
        let peer = request
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|info| info.0.ip());
        if !crate::auth::peers::trusted(&config.trusted_proxies, peer)
            && request
                .headers_mut()
                .remove(&config.subject_header)
                .is_some()
        {
            tracing::warn!(
                peer = ?peer,
                header = %config.subject_header,
                "client certificate header from an untrusted peer dropped"
            );
        }
    }
    next.run(request).await
}
