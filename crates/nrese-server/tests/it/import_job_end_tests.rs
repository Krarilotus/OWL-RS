//! An import job that panics still ends, and its words claim only what its worker knew: a
//! panic after the load returned names the published revision, one inside the load says the
//! effect is unknown (`nrese_store::LoadPanic`).

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::{LoadPanic, StoreConfig};
use serde_json::Value;
use tokio::time::{Instant, sleep};
use tower::util::ServiceExt;

use crate::support::{body_text, test_state_with_store_config};

const STATEMENT: &str = "<urn:t:job> <urn:t:p> 1 .";
const ASK: &str = "ASK { <urn:t:job> <urn:t:p> 1 }";

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: &str,
    body: &str,
) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::ACCEPT, "application/sparql-results+json, */*;q=0.1")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_text(response).await.unwrap())
}

/// An upload of [`STATEMENT`] as a job, its load panicking at `at`: the app, the revision the
/// load publishes (one past the store's), and the job once it ended, or after 10 s.
async fn import_panicking_at(at: LoadPanic) -> (Router, u64, Value) {
    let state = test_state_with_store_config(
        StoreConfig::in_memory(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let store = state.store();
    let revision = store.current_revision() + 1;
    store.panic_next_bulk_load(at);
    let app = nrese_server::build_app(state);
    let uri = "/api/v1/repositories/nrese/import?async=true";
    let (status, body) = send(&app, Method::POST, uri, "text/turtle", STATEMENT).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let started: Value = serde_json::from_str(&body).unwrap();
    let path = started["path"].as_str().expect(&body).to_owned();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = send(&app, Method::GET, &path, "application/json", "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let job: Value = serde_json::from_str(&body).unwrap();
        if job["state"] != "running" || Instant::now() >= deadline {
            return (app, revision, job);
        }
        sleep(Duration::from_millis(20)).await;
    }
}

// Spec: docs/ops/http-api.md says an import job ends `done`, `failed` or `cancelled`, its
// error with retry advice. A panic after its load returned -> `failed`, naming the revision
// the load published (also in the report) and saying not to send it again; the statement
// is visible.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panic_after_the_load_returned_ends_the_job_at_its_published_revision() {
    let (app, revision, job) = import_panicking_at(LoadPanic::AfterReturn).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert_eq!(
        job["error"],
        format!(
            "the load was published at revision {revision}, then the import stopped (an internal error); reasoning may not be up to date. Don't send it again."
        ),
        "{job}"
    );
    assert_eq!(job["report"]["revision"], revision, "{job}");
    let (status, body) = send(
        &app,
        Method::POST,
        "/dataset/query?infer=false",
        "application/sparql-query",
        ASK,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        answer["boolean"], true,
        "the published statement is visible"
    );
}

// Spec: as above; a panic inside the load, before it returned -> `failed`, saying whether
// any of it took effect is unknown (the worker never learned), with no revision reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panic_inside_the_load_ends_the_job_with_its_effect_unknown() {
    let (_, _, job) = import_panicking_at(LoadPanic::BeforeReturn).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert_eq!(
        job["error"],
        "the import stopped (an internal error) before it finished; whether any of it took effect is unknown. Check the data and the quarantine list before you send it again.",
        "{job}"
    );
    assert!(job.get("report").is_none(), "{job}");
}
