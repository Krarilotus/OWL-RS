mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use tower::util::ServiceExt;

use support::test_app_with_settings;

#[tokio::test]
async fn invalid_update_does_not_mutate_dataset_or_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(PolicyConfig::default(), ReasonerConfig::default())?;

    let seed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
                ))?,
        )
        .await?;
    assert_eq!(seed.status(), StatusCode::NO_CONTENT);

    let ready_before = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    let ready_before_body = axum::body::to_bytes(ready_before.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ready_before_body.to_vec())?.contains("\"revision\":1"));

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from("INSERT DATA {"))?,
        )
        .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let ask = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Chttp%3A%2F%2Fexample.com%2Flive-s%3E%20%3Chttp%3A%2F%2Fexample.com%2Fp%3E%20%3Chttp%3A%2F%2Fexample.com%2Flive-o%3E%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    let ask_body = axum::body::to_bytes(ask.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ask_body.to_vec())?.contains("true"));

    let ready_after = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    let ready_after_body = axum::body::to_bytes(ready_after.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ready_after_body.to_vec())?.contains("\"revision\":1"));

    Ok(())
}

#[tokio::test]
async fn reasoner_reject_does_not_publish_data_or_advance_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::RulesMvp),
    )?;

    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA {
                        <http://example.com/Parent> <http://www.w3.org/2002/07/owl#disjointWith> <http://example.com/Other> .
                        <http://example.com/Child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example.com/Parent> .
                        <http://example.com/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Child> .
                        <http://example.com/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Other> .
                    }",
                ))?,
        )
        .await?;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let ask = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Chttp%3A%2F%2Fexample.com%2Falice%3E%20a%20%3Chttp%3A%2F%2Fexample.com%2FOther%3E%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    let ask_body = axum::body::to_bytes(ask.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ask_body.to_vec())?.contains("false"));

    let ready = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    let ready_body = axum::body::to_bytes(ready.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ready_body.to_vec())?.contains("\"revision\":0"));

    Ok(())
}
