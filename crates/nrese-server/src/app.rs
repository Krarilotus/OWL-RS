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
    let screened = axum::middleware::from_fn_with_state(state.clone(), screen);
    router(state)
        .layer(screened)
        .layer(metrics)
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(PropagateRequestIdLayer::x_request_id())
        // Inside the request id, outside its propagation: each problem document reads the id
        // its response carries.
        .layer(axum::middleware::map_response(problems))
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
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

/// Every request before the router, in one layer (each layer costs a request its own
/// allocations and state clones). First, in the `mtls` mode, drops the client certificate
/// subject header from requests whose peer isn't one of the trusted proxies (or is unknown:
/// a router served without peer addresses): only a proxy that terminated TLS may say who
/// the client is. Then, while the default repository can't serve (a failed change left it
/// in no known state, [`nrese_store::StoreService::unavailable`]), requests that resolve to
/// it get 503; those for other repositories, the server's health, readiness, metrics and
/// version, the repository lists, the access API and the console go on. One atomic load a
/// request otherwise.
async fn screen(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Method, Request, StatusCode};
    use tower::util::ServiceExt;

    use crate::auth::{AuthConfig, MtlsConfig};

    const TRUSTED: [u8; 4] = [127, 0, 0, 1];
    const OTHER: [u8; 4] = [203, 0, 113, 9];

    /// The whole server, its default repository in no known state when `broken` (a failed
    /// change's mark).
    fn server(broken: bool, auth: AuthConfig) -> axum::Router {
        let store = nrese_store::StoreService::new(nrese_store::StoreConfig::default()).unwrap();
        if broken {
            store
                .unavailable_flag()
                .set("a change failed".to_owned())
                .unwrap();
        }
        let state = crate::state::AppState::new(
            store,
            nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::default()),
            crate::policy::PolicyConfig {
                auth,
                ..crate::policy::PolicyConfig::default()
            },
            crate::ai::AiSuggestionService::disabled(),
            crate::runtime_posture::DeploymentPosture::OpenWorkbench,
        );
        state.mark_ready();
        super::build_app(state)
    }

    /// Callers known by a client certificate's subject, which counts only from loopback.
    fn mtls() -> AuthConfig {
        AuthConfig::Mtls(MtlsConfig {
            subject_header: "x-client-cert-subject".to_owned(),
            trusted_proxies: crate::auth::peers::AddressRange::loopback(),
            read_subjects: ["CN=reader".to_owned()].into(),
            admin_subjects: Default::default(),
        })
    }

    /// A query of the default repository, with a reader's subject or without.
    fn query(subject: bool) -> Request<Body> {
        let request = Request::builder().uri("/dataset/query?query=ASK%7B%7D");
        let request = match subject {
            true => request.header("x-client-cert-subject", "CN=reader"),
            false => request,
        };
        request.body(Body::empty()).unwrap()
    }

    /// `request` from `peer` as request `req-1`: the status, the `x-request-id` header and the
    /// body as JSON (`Null` if it isn't).
    async fn answer(
        app: axum::Router,
        peer: [u8; 4],
        mut request: Request<Body>,
    ) -> (StatusCode, Option<String>, serde_json::Value) {
        request
            .headers_mut()
            .insert("x-request-id", "req-1".parse().unwrap());
        request
            .extensions_mut()
            .insert(ConnectInfo(std::net::SocketAddr::from((peer, 4000))));
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let id = response
            .headers()
            .get("x-request-id")
            .map(|id| id.to_str().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let document = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, id, document)
    }

    /// A request for the default repository while a failed change left it in no known state
    /// is answered 503 whoever sends it: a proxy trusted with the client certificate's
    /// subject, or another peer with that subject or without. A request that meets both
    /// checks (the repository in no known state, a subject from an untrusted peer) gets that
    /// 503: the subject's check drops a header and answers nothing itself, so the answer is
    /// the one a request without the subject gets, in whichever order the two checks run.
    ///
    /// - Expected: for each caller, this document, written out here.
    #[tokio::test]
    async fn a_default_repository_in_no_known_state_answers_503_to_every_caller() {
        let expected = serde_json::json!({
            "type": "https://nrese.dev/problems/service-unavailable",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "repository 'nrese' is unavailable until the server restarts: a change to \
                it failed and its state couldn't be confirmed; operators see why and how to \
                repair it in the repository list; the server's other repositories serve",
            "reasoner_reject": null,
            "request_id": "req-1",
        });
        for (peer, subject) in [(TRUSTED, true), (OTHER, true), (OTHER, false)] {
            let (status, id, document) = answer(server(true, mtls()), peer, query(subject)).await;
            let case = format!("{peer:?} with subject {subject}");
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{case}");
            assert_eq!(id.as_deref(), Some("req-1"), "{case}");
            assert_eq!(document, expected, "{case}");
        }
    }

    /// While the default repository serves, a client certificate's subject counts only from
    /// a trusted proxy: from another peer, the request is answered as one without it.
    #[tokio::test]
    async fn a_subject_counts_only_from_a_trusted_proxy() {
        let (trusted, ..) = answer(server(false, mtls()), TRUSTED, query(true)).await;
        let (other, _, document) = answer(server(false, mtls()), OTHER, query(true)).await;
        let (without, _, plain) = answer(server(false, mtls()), OTHER, query(false)).await;
        assert_eq!(trusted, StatusCode::OK);
        assert_eq!(other, StatusCode::UNAUTHORIZED);
        assert_eq!((other, document), (without, plain));
    }

    /// What the framework answers on its own is a problem document with the request's id
    /// (a route that doesn't exist, a method the route doesn't take, a body its extractor
    /// rejects), and a plain answer carries the id in its header.
    #[tokio::test]
    async fn framework_errors_are_problem_documents_with_the_request_id() {
        let request = |method: Method, uri: &str, body: &'static str| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap()
        };
        for (sent, status, kind) in [
            (
                request(Method::GET, "/no-such-route", ""),
                StatusCode::NOT_FOUND,
                "not-found",
            ),
            (
                request(Method::DELETE, "/healthz", ""),
                StatusCode::METHOD_NOT_ALLOWED,
                "method-not-allowed",
            ),
            (
                request(Method::POST, "/api/v1/ai/query-suggestions", "{"),
                StatusCode::BAD_REQUEST,
                "bad-request",
            ),
        ] {
            let (got, id, document) = answer(server(false, AuthConfig::None), TRUSTED, sent).await;
            assert_eq!(got, status, "{kind}");
            assert_eq!(id.as_deref(), Some("req-1"), "{kind}");
            let expected = format!("https://nrese.dev/problems/{kind}");
            assert_eq!(document["type"], expected.as_str(), "{kind}");
            assert_eq!(document["request_id"], "req-1", "{kind}");
        }
        let healthy = request(Method::GET, "/healthz", "");
        let (status, id, _) = answer(server(false, AuthConfig::None), TRUSTED, healthy).await;
        assert_eq!((status, id.as_deref()), (StatusCode::OK, Some("req-1")));
    }

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
