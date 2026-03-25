mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::{StoreConfig, StoreMode};
use tower::util::ServiceExt;

use nrese_server::policy::PolicyConfig;
use support::{minimal_fixture_path, test_app_with_settings, test_app_with_store_config};

#[tokio::test]
async fn tell_endpoint_accepts_default_graph_turtle() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
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

    let ask_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .body(Body::from(
                    "ASK WHERE { <http://example.com/s> <http://example.com/p> <http://example.com/o> }",
                ))?,
        )
        .await?;

    assert_eq!(ask_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(ask_response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("true"));
    Ok(())
}

#[tokio::test]
async fn tell_endpoint_supports_named_graph_ingest() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
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

    let ask_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .body(Body::from(
                    "ASK WHERE { GRAPH <http://example.com/g> { <http://example.com/s> <http://example.com/p> \"v\" } }",
                ))?,
        )
        .await?;

    assert_eq!(ask_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(ask_response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("true"));
    Ok(())
}

#[tokio::test]
async fn tell_endpoint_uses_reasoner_gate_and_rejects_without_publish()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let app = test_app_with_store_config(
        StoreConfig {
            mode: StoreMode::InMemory,
            data_dir: temp.path().join("unused"),
            preload_ontology: true,
            ontology_path: Some(minimal_fixture_path()),
            ontology_fallbacks: Vec::new(),
        },
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
    )?;

    let tell_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/tell")
                .method(Method::POST)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> .
                     @prefix owl: <http://www.w3.org/2002/07/owl#> .
                     @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
                     ex:Parent owl:disjointWith ex:Other .
                     ex:carol rdf:type ex:Child .
                     ex:carol rdf:type ex:Other .",
                ))?,
        )
        .await?;

    assert_eq!(tell_response.status(), StatusCode::BAD_REQUEST);
    let tell_body = axum::body::to_bytes(tell_response.into_body(), usize::MAX).await?;
    let tell_text = String::from_utf8(tell_body.to_vec())?;
    assert!(tell_text.contains("reasoner_reject"));
    assert!(tell_text.contains("owl:disjointWith"));

    let ask_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .body(Body::from(
                    "ASK WHERE { <http://example.com/carol> a <http://example.com/Other> }",
                ))?,
        )
        .await?;

    assert_eq!(ask_response.status(), StatusCode::OK);
    let ask_body = axum::body::to_bytes(ask_response.into_body(), usize::MAX).await?;
    let ask_text = String::from_utf8(ask_body.to_vec())?;
    assert!(ask_text.contains("false"));
    Ok(())
}
