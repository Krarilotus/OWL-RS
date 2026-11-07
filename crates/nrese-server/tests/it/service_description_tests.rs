//! A repository's service description names that repository's endpoints: a client that
//! discovers another repository through its description and follows the endpoint it
//! names reaches that repository, not the default one.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store_config};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;

async fn send(app: &Router, method: Method, uri: &str, body: &str) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if !body.is_empty() {
        request = request.header(header::CONTENT_TYPE, "text/turtle");
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    (status, body_text(response).await.expect("body"))
}

/// Repository `second`'s description (at its own path, and as its combined endpoint's
/// answer to a GET without a query) names `second`'s endpoints, never the default
/// repository's `/dataset/…` aliases; the default repository's still names those. Every
/// row is checked; the failures are reported together.
#[tokio::test]
async fn a_repository_describes_its_own_endpoints() {
    let dir = tempfile::tempdir().expect("data directory");
    let app = test_app_with_store_config(
        StoreConfig::on_disk(dir.path()),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .expect("app");
    let (status, text) = send(&app, Method::PUT, "/repositories/second", "").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    let base = "/api/v1/repositories/second";
    let mut wrong = Vec::new();
    for uri in [
        format!("{base}/service-description"),
        format!("{base}/sparql"),
    ] {
        let (status, ttl) = send(&app, Method::GET, &uri, "").await;
        if status != StatusCode::OK {
            wrong.push(format!("{uri}: {status} {ttl}"));
            continue;
        }
        for endpoint in [
            format!("sd:endpoint <{base}/query>"),
            format!("nrese:sparqlEndpoint <{base}/sparql>"),
            format!("nrese:graphStoreEndpoint <{base}/data>"),
            format!("nrese:serviceDescriptionEndpoint <{base}/service-description>"),
        ] {
            if !ttl.contains(&endpoint) {
                wrong.push(format!("{uri}: no {endpoint}"));
            }
        }
        if ttl.contains("/dataset/") {
            wrong.push(format!("{uri}: names a /dataset/ alias:\n{ttl}"));
        }
    }
    // Guard (passes before the fix too): the default repository keeps its aliases.
    let (_, ttl) = send(&app, Method::GET, "/dataset/service-description", "").await;
    if !ttl.contains("sd:endpoint </dataset/query>") {
        wrong.push(format!("the default repository:\n{ttl}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
