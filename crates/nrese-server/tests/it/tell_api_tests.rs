use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use tower::util::ServiceExt;

use crate::support::{query_text, test_app_with_settings};
use nrese_server::policy::PolicyConfig;

#[tokio::test]
async fn tell_endpoint_accepts_default_graph_turtle() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;

    let tell_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/tell")
                .method(Method::POST)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:s ex:p ex:o .",
                ))?,
        )
        .await?;

    assert_eq!(tell_response.status(), StatusCode::NO_CONTENT);

    let text = query_text(
        app,
        "ASK WHERE { <http://example.com/s> <http://example.com/p> <http://example.com/o> }",
    )
    .await?;
    assert!(text.contains("true"));
    Ok(())
}

#[tokio::test]
async fn tell_endpoint_supports_named_graph_ingest() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;

    let tell_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/tell?graph=http%3A%2F%2Fexample.com%2Fg")
                .method(Method::POST)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:s ex:p \"v\" .",
                ))?,
        )
        .await?;

    assert_eq!(tell_response.status(), StatusCode::NO_CONTENT);

    let text = query_text(
        app,
        "ASK WHERE { GRAPH <http://example.com/g> { <http://example.com/s> <http://example.com/p> \"v\" } }",
    )
    .await?;
    assert!(text.contains("true"));
    Ok(())
}
