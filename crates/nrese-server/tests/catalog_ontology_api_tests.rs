mod support;

use std::fs;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use serde_json::Value;
use tower::util::ServiceExt;

use nrese_server::policy::PolicyConfig;
use support::{catalog_fixture_path, test_app_with_settings};

#[tokio::test]
async fn tell_endpoint_accepts_official_foaf_rdf_xml_and_surfaces_reasoning_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
    )?;
    let foaf_bytes = fs::read(catalog_fixture_path("foaf.rdf"))?;

    let tell_ontology = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/tell")
                .method(Method::POST)
                .header("content-type", "application/rdf+xml")
                .body(Body::from(foaf_bytes))?,
        )
        .await?;
    assert_eq!(tell_ontology.status(), StatusCode::NO_CONTENT);

    let tell_assertion = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/tell")
                .method(Method::POST)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix foaf: <http://xmlns.com/foaf/0.1/> . <http://example.com/alice> a foaf:Person .",
                ))?,
        )
        .await?;
    assert_eq!(tell_assertion.status(), StatusCode::NO_CONTENT);

    let diagnostics_response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/reasoning")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(diagnostics_response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(diagnostics_response.into_body(), usize::MAX).await?;
    let payload: Value = serde_json::from_slice(&body)?;
    assert_eq!(payload["last_run"]["status"], "completed");
    assert!(
        payload["last_run"]["inferred_triples"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        payload["last_run"]["stats"]["supported_asserted_triples"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    Ok(())
}

#[tokio::test]
async fn graph_store_roundtrip_accepts_official_org_turtle_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
    )?;
    let org_bytes = fs::read(catalog_fixture_path("org.ttl"))?;

    let put_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fontologies%2Forg")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(org_bytes))?,
        )
        .await?;
    assert_eq!(put_response.status(), StatusCode::NO_CONTENT);

    let get_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fontologies%2Forg")
                .method(Method::GET)
                .header("accept", "application/n-triples")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(get_response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("http://www.w3.org/ns/org#Organization"));
    Ok(())
}
