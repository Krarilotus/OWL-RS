//! Reasoning lifecycle over HTTP: a store whose data is inconsistent under its reasoning
//! is quarantined (not ready), but serves reads for diagnosis and writes for repair.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use nrese_store::{MutationPipeline, SparqlUpdateRequest, StoreConfig, StoreService};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store};

const EX: &str = "http://example.com/";

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .method(Method::GET)
        .body(Body::empty())
        .unwrap()
}

fn update(text: &str) -> Request<Body> {
    Request::builder()
        .uri("/dataset/update")
        .method(Method::POST)
        .header("content-type", "application/sparql-update")
        .body(Body::from(text.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn inconsistent_data_is_quarantined_but_readable_and_repairable()
-> Result<(), Box<dyn std::error::Error>> {
    let store = StoreService::new(StoreConfig::in_memory())?;
    // Imported with reasoning off: through a pipeline without a reasoner.
    let import = MutationPipeline::new(
        std::sync::Arc::new(store.clone()),
        ReasonerConfig::default(),
    );
    import.execute_update(&SparqlUpdateRequest::new(format!(
        "INSERT DATA {{ <{EX}A> <http://www.w3.org/2002/07/owl#disjointWith> <{EX}B> .
                       <{EX}x> a <{EX}A> , <{EX}B> }}"
    )))?;
    store.rematerialise(Ruleset::Owl2Rl)?;
    let app = test_app_with_store(
        store,
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;

    let ready = app.clone().oneshot(get("/readyz")).await?;
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_text(ready).await?;
    // Not ready, the probe still says only its status and what is degraded.
    let probe: serde_json::Value = serde_json::from_str(&body)?;
    let mut keys: Vec<&str> = probe
        .as_object()
        .ok_or("an object")?
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["degraded", "status"], "{body}");
    assert_eq!(probe["status"], "quarantined", "{body}");
    let info = body_text(app.clone().oneshot(get("/dataset/info")).await?).await?;
    assert!(info.contains("\"violations\":1"), "{info}");

    let query = format!("/dataset/query?query=ASK%20%7B%20%3C{EX}x%3E%20%3Fp%20%3Fo%20%7D");
    let answer = app.clone().oneshot(get(&query)).await?;
    assert_eq!(answer.status(), StatusCode::OK, "reads serve diagnosis");

    let repair = app
        .clone()
        .oneshot(update(&format!("DELETE DATA {{ <{EX}x> a <{EX}B> }}")))
        .await?;
    assert!(repair.status().is_success(), "{}", repair.status());
    let ready = app.oneshot(get("/readyz")).await?;
    assert_eq!(ready.status(), StatusCode::OK);
    Ok(())
}
