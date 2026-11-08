//! What the RDF4J protocol's write paths admit: an update past the policy's update limit is
//! refused (413) through every path that takes one, not only `/dataset/update`; a payload
//! past the upload limit is refused before it is read; and a read inside a transaction that
//! can't see the transaction's changes (EXPLAIN) is refused rather than answered about the
//! committed data.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_policy};
use nrese_server::policy::{PolicyConfig, RequestLimits};

const REPO: &str = "/repositories/nrese";
const FORM: &str = "application/x-www-form-urlencoded";
const UPDATE: &str = "application/sparql-update";

/// An app whose update limit (64 bytes) is far below its upload limit (4 KiB).
fn app() -> Router {
    test_app_with_policy(PolicyConfig {
        limits: RequestLimits {
            max_update_bytes: 64,
            max_rdf_upload_bytes: 4096,
            ..RequestLimits::default()
        },
        ..PolicyConfig::default()
    })
    .expect("app")
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: String,
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    (status, body_text(response).await.expect("body"))
}

async fn size(app: &Router) -> String {
    send(
        app,
        Method::GET,
        &format!("{REPO}/size"),
        None,
        String::new(),
    )
    .await
    .1
}

/// An RDF4J transaction's URL.
async fn transaction(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("{REPO}/transactions"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::CREATED);
    response.headers()[header::LOCATION]
        .to_str()
        .expect("location")
        .to_owned()
}

/// A session's path (`/api/v1/repositories/nrese/sessions/{id}`).
async fn session(app: &Router) -> String {
    let (status, text) = send(
        app,
        Method::POST,
        "/api/v1/repositories/nrese/sessions",
        None,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let opened: serde_json::Value = serde_json::from_str(&text).expect("json");
    opened["path"].as_str().expect("path").to_owned()
}

/// A valid update of about 200 bytes: past the update limit, within the upload limit.
fn long_update() -> String {
    format!(
        "INSERT DATA {{ <http://example.com/{}> <http://example.com/p> 1 }}",
        "s".repeat(150)
    )
}

/// Every path that takes a SPARQL update refuses one past the update limit, and nothing of
/// it is applied. Every row is checked; the failures are reported together.
#[tokio::test]
async fn an_update_past_its_limit_is_refused_on_every_path() {
    let update = long_update();
    let form = serde_urlencoded::to_string([("update", update.as_str())]).expect("form");
    let mut wrong = Vec::new();
    for row in [
        "repository POST",
        "statements POST",
        "statements POST, form",
        "transaction UPDATE",
        "session update",
    ] {
        let app = app();
        let (status, text) = match row {
            "repository POST" => send(&app, Method::POST, REPO, Some(UPDATE), update.clone()).await,
            "statements POST" => {
                let uri = format!("{REPO}/statements");
                send(&app, Method::POST, &uri, Some(UPDATE), update.clone()).await
            }
            "statements POST, form" => {
                let uri = format!("{REPO}/statements");
                send(&app, Method::POST, &uri, Some(FORM), form.clone()).await
            }
            "transaction UPDATE" => {
                let uri = format!("{}?action=UPDATE", transaction(&app).await);
                send(&app, Method::PUT, &uri, Some(UPDATE), update.clone()).await
            }
            _ => {
                let uri = format!("{}/update", session(&app).await);
                send(&app, Method::POST, &uri, Some(UPDATE), update.clone()).await
            }
        };
        if status != StatusCode::PAYLOAD_TOO_LARGE {
            wrong.push(format!("{row}: {status} {text}"));
        }
        let held = size(&app).await;
        if held.trim() != "0" {
            wrong.push(format!("{row}: the store holds {held} statements"));
        }
    }
    // Guard (passes before the check too): a short update is applied.
    let app = app();
    let short = "INSERT DATA { <http://example.com/a> <http://example.com/p> 1 }".to_owned();
    let uri = format!("{REPO}/statements");
    let (status, text) = send(&app, Method::POST, &uri, Some(UPDATE), short).await;
    if status != StatusCode::NO_CONTENT || size(&app).await.trim() != "1" {
        wrong.push(format!("a short update: {status} {text}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A transaction's DELETE past the upload limit is refused as too large before its payload
/// is read: malformed, it is still 413, not a syntax error.
#[tokio::test]
async fn a_transaction_delete_past_the_upload_limit_is_refused_before_it_is_read() {
    let app = app();
    let uri = format!("{}?action=DELETE", transaction(&app).await);
    let payload = format!("<http://example.com/s> {} .\n", "x".repeat(8192));
    let (status, text) = send(
        &app,
        Method::PUT,
        &uri,
        Some("application/n-triples"),
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{text}");
}

/// EXPLAIN inside a transaction (`explain=plan`, `explain=true`) would describe the
/// committed data, not the transaction's: it is refused. A plain query there sees the
/// transaction's changes (the guard row).
#[tokio::test]
async fn explain_inside_a_transaction_is_refused() {
    let app = app();
    let tx = transaction(&app).await;
    let (status, text) = send(
        &app,
        Method::PUT,
        &format!("{tx}?action=ADD"),
        Some("application/n-triples"),
        "<http://example.com/a> <http://example.com/p> \"1\" .\n".to_owned(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let query = "SELECT * WHERE { ?s ?p ?o }";
    let mut wrong = Vec::new();
    for explain in ["plan", "true"] {
        let pairs = [("action", "QUERY"), ("query", query), ("explain", explain)];
        let uri = format!(
            "{tx}?{}",
            serde_urlencoded::to_string(pairs).expect("query")
        );
        let (status, text) = send(&app, Method::PUT, &uri, None, String::new()).await;
        if status != StatusCode::BAD_REQUEST {
            wrong.push(format!("explain={explain}: {status} {text}"));
        }
    }
    let pairs = [("action", "QUERY"), ("query", query)];
    let uri = format!(
        "{tx}?{}",
        serde_urlencoded::to_string(pairs).expect("query")
    );
    let (status, text) = send(&app, Method::PUT, &uri, None, String::new()).await;
    if status != StatusCode::OK || !text.contains("http://example.com/a") {
        wrong.push(format!("a plain query: {status} {text}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
