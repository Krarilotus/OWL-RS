//! What a client transaction keeps between its requests is charged to the shared runtime
//! (`nrese_store::sessions`): each body within the upload limit, but together they are
//! refused once the runtime has no room for them, and the charge goes with the session.
//!
//! Spec: the v1.5 bar (no unbounded retention outside every budget) and the witness of the
//! quality triage's N3 (2026-11-09, S-02): an authenticated RDF4J transaction on a runtime
//! of 1 MiB, 32 valid 64 KiB ADDs of one triple padded with comments.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use jsonwebtoken::{EncodingKey, Header, encode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::auth::{AuthConfig, JwtBearerConfig};
use nrese_server::policy::{PolicyConfig, RequestLimits};
use nrese_store::{Runtime, StoreConfig, StoreService};
use serde::Serialize;
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store};

const RUNTIME: usize = 1 << 20;
const BODY: usize = 64 << 10;
const BODIES: usize = 32;

#[derive(Serialize)]
struct Claims {
    exp: usize,
    roles: Vec<String>,
}

fn token() -> String {
    encode(
        &Header::default(),
        &Claims {
            exp: 4_102_444_800,
            roles: vec!["nrese.admin".to_owned()],
        },
        &EncodingKey::from_secret(b"test-secret"),
    )
    .expect("jwt")
}

/// An app with bearer authentication on a store whose runtime holds 1 MiB, and that
/// runtime's memory (the process's: queries charge it too, there is no queries' pool).
fn app() -> (Router, Arc<nrese_exec::SharedBudget>) {
    let runtime = Arc::new(Runtime::new(RUNTIME as u64, 0));
    let memory = runtime.memory().expect("a finite runtime");
    let store = StoreService::with_runtime(StoreConfig::default(), runtime).expect("store");
    let policy = PolicyConfig {
        auth: AuthConfig::BearerJwt(JwtBearerConfig {
            shared_secret: "test-secret".to_owned(),
            issuer: None,
            audience: None,
            read_role: "nrese.read".to_owned(),
            admin_role: "nrese.admin".to_owned(),
            leeway_seconds: 0,
        }),
        limits: RequestLimits {
            max_rdf_upload_bytes: 2 * BODY,
            ..RequestLimits::default()
        },
        ..PolicyConfig::default()
    };
    let app = test_app_with_store(store, policy, ReasonerConfig::default()).expect("app");
    (app, memory)
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {}", token()));
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

async fn begin(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/repositories/nrese/transactions")
                .header(header::AUTHORIZATION, format!("Bearer {}", token()))
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

/// One triple, padded with a comment to exactly `BODY` bytes of valid Turtle.
fn body() -> Vec<u8> {
    let triple = "<http://example.com/s> <http://example.com/p> <http://example.com/o> .\n#";
    let mut text = triple.to_owned();
    text.push_str(&"x".repeat(BODY - triple.len() - 1));
    text.push('\n');
    assert_eq!(text.len(), BODY);
    text.into_bytes()
}

/// ADDs `BODIES` bodies to `tx`, or until one is refused: how many were taken, and the
/// first refusal's status and text. Every taken body must stay charged: the runtime's use
/// never falls below the bytes taken.
async fn add_all(
    app: &Router,
    tx: &str,
    memory: &nrese_exec::SharedBudget,
) -> (usize, Option<(StatusCode, String)>) {
    let uri = format!("{tx}?action=ADD");
    for taken in 0..BODIES {
        let (status, text) = send(app, Method::PUT, &uri, Some("text/turtle"), body()).await;
        if status != StatusCode::OK {
            return (taken, Some((status, text)));
        }
        let kept = (taken + 1) * BODY;
        assert!(
            memory.used() >= kept,
            "{kept} bytes kept by the transaction, {} charged",
            memory.used()
        );
    }
    (BODIES, None)
}

/// The witness: the bodies fill the runtime and the next is refused (503, the shared
/// pool's), before the runtime is exceeded; a rollback returns all of it, and the
/// transaction after it is taken again.
#[tokio::test]
async fn a_transactions_bodies_are_charged_and_refused_past_the_runtime() {
    let (app, memory) = app();
    let tx = begin(&app).await;
    let (taken, refused) = add_all(&app, &tx, &memory).await;
    let (status, text) = refused.unwrap_or_else(|| {
        panic!(
            "all {BODIES} bodies of {BODY} bytes taken on a runtime of {RUNTIME}; {} charged",
            memory.used()
        )
    });
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert!(taken > 0 && taken * BODY <= RUNTIME, "{taken} taken");
    assert!(memory.peak() <= RUNTIME);
    let (status, text) = send(&app, Method::DELETE, &tx, None, Vec::new()).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    assert_eq!(memory.used(), 0, "charged after the rollback");
    let again = begin(&app).await;
    let (status, text) = send(
        &app,
        Method::PUT,
        &format!("{again}?action=ADD"),
        Some("text/turtle"),
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

/// A read inside the transaction takes no copy beside what is charged, and the commit
/// returns the charge: afterwards the runtime holds nothing and the one triple is stored.
#[tokio::test]
async fn a_read_and_the_commit_stay_within_the_transactions_charge() {
    let (app, memory) = app();
    let tx = begin(&app).await;
    let uri = format!("{tx}?action=ADD");
    for _ in 0..4 {
        let (status, text) = send(&app, Method::PUT, &uri, Some("text/turtle"), body()).await;
        assert_eq!(status, StatusCode::OK, "{text}");
    }
    let (status, text) = send(
        &app,
        Method::PUT,
        &format!("{tx}?action=SIZE"),
        None,
        Vec::new(),
    )
    .await;
    assert_eq!((status, text.trim()), (StatusCode::OK, "1"));
    assert!(memory.used() >= 4 * BODY, "{} charged", memory.used());
    let (status, text) = send(
        &app,
        Method::PUT,
        &format!("{tx}?action=COMMIT"),
        None,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(memory.used(), 0, "charged after the commit");
    let (status, text) = send(
        &app,
        Method::GET,
        "/repositories/nrese/size",
        None,
        Vec::new(),
    )
    .await;
    assert_eq!((status, text.trim()), (StatusCode::OK, "1"));
}
