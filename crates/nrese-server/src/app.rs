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
    let available =
        axum::middleware::from_fn_with_state(state.clone(), default_repository_available);
    router(state)
        .layer(available)
        .layer(proxies)
        .layer(metrics)
        .layer(DefaultBodyLimit::max(body_limit))
        // Inside the request id, so each problem document carries it.
        .layer(axum::middleware::from_fn(problems))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
}

/// While the default repository can't serve (a failed change left it in no known state,
/// [`nrese_store::StoreService::unavailable`]), requests that resolve to it get 503; those for
/// other repositories, the server's health, readiness, metrics and version, the repository
/// lists, the access API and the console go on. One atomic load a request otherwise.
async fn default_repository_available(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if state.default_unavailable().is_some() && resolves_to_default(request.uri().path()) {
        return crate::error::ApiError::unavailable(format!(
            "repository '{}' {}; the server's other repositories serve",
            crate::repositories::DEFAULT_REPOSITORY,
            crate::state::UNAVAILABLE_UNTIL_RESTART
        ))
        .into_response();
    }
    next.run(request).await
}

/// Whether a request for `path` is the default repository's: not another repository's
/// (`/api/v1/repositories/{id}/…`, `/repositories/{id}/…`), nor the server's own.
fn resolves_to_default(path: &str) -> bool {
    const SERVER: [&str; 7] = [
        "/healthz",
        "/readyz",
        "/metrics",
        "/version",
        "/api/v1/health",
        "/api/v1/openapi.json",
        "/protocol",
    ];
    const SERVER_TREES: [&str; 3] = ["/api/v1/access", "/console", "/ui"];
    if SERVER.contains(&path)
        || SERVER_TREES
            .iter()
            .any(|tree| path == *tree || path.starts_with(&format!("{tree}/")))
    {
        return false;
    }
    for list in ["/api/v1/repositories", "/repositories"] {
        if path == list || path == format!("{list}/") {
            return false;
        }
        if let Some(rest) = path.strip_prefix(&format!("{list}/")) {
            let id = rest.split('/').next().unwrap_or_default();
            return id == crate::repositories::DEFAULT_REPOSITORY;
        }
    }
    true
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

#[cfg(test)]
mod tests {
    /// Which requests are the default repository's while it can't serve.
    #[test]
    fn requests_resolve_to_their_repository() {
        for path in [
            "/dataset/query",
            "/sparql",
            "/update",
            "/api/v1/repositories/nrese",
            "/api/v1/repositories/nrese/query",
            "/repositories/nrese/statements",
            "/namespaces",
        ] {
            assert!(super::resolves_to_default(path), "{path}");
        }
        for path in [
            "/healthz",
            "/readyz",
            "/metrics",
            "/api/v1/repositories",
            "/repositories",
            "/api/v1/repositories/r2/query",
            "/repositories/r2/statements",
            "/api/v1/access/me",
            "/console/index.html",
        ] {
            assert!(!super::resolves_to_default(path), "{path}");
        }
    }
}
