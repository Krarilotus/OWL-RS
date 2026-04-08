mod support;

use std::fs;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::{StoreConfig, StoreMode};
use serde_json::Value;
use tower::util::ServiceExt;

use nrese_server::policy::PolicyConfig;
use support::{catalog_fixture_path, test_app_with_settings, test_app_with_store_config};

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
    assert_eq!(put_response.status(), StatusCode::CREATED);

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

#[tokio::test]
async fn startup_with_official_prov_preload_supports_relative_base_iris()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_store_config(
        StoreConfig {
            mode: StoreMode::InMemory,
            data_dir: std::env::temp_dir().join("unused"),
            preload_ontology: true,
            ontology_path: Some(catalog_fixture_path("prov.ttl")),
            ontology_fallbacks: Vec::new(),
        },
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=PREFIX%20owl%3A%20%3Chttp%3A%2F%2Fwww.w3.org%2F2002%2F07%2Fowl%23%3E%20PREFIX%20prov%3A%20%3Chttp%3A%2F%2Fwww.w3.org%2Fns%2Fprov%23%3E%20ASK%20WHERE%20%7B%20prov%3Agenerated%20owl%3AinverseOf%20prov%3AwasGeneratedBy%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(body.to_vec())?.contains("true"));
    Ok(())
}

#[tokio::test]
async fn graph_store_roundtrip_accepts_official_prov_turtle_with_content_location_base_iri()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
    )?;
    let prov_bytes = fs::read(catalog_fixture_path("prov.ttl"))?;

    let put_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fontologies%2Fprov")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .header("content-location", "https://www.w3.org/ns/prov.ttl")
                .body(Body::from(prov_bytes))?,
        )
        .await?;
    assert_eq!(put_response.status(), StatusCode::CREATED);

    let query_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=PREFIX%20owl%3A%20%3Chttp%3A%2F%2Fwww.w3.org%2F2002%2F07%2Fowl%23%3E%20PREFIX%20prov%3A%20%3Chttp%3A%2F%2Fwww.w3.org%2Fns%2Fprov%23%3E%20ASK%20WHERE%20%7B%20GRAPH%20%3Chttp%3A%2F%2Fexample.com%2Fontologies%2Fprov%3E%20%7B%20prov%3Agenerated%20owl%3AinverseOf%20prov%3AwasGeneratedBy%20%7D%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(query_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(query_response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(body.to_vec())?.contains("true"));

    Ok(())
}

#[tokio::test]
async fn graph_store_roundtrip_accepts_official_skos_rdf_xml_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
    )?;
    let skos_bytes = fs::read(catalog_fixture_path("skos.rdf"))?;

    let put_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fontologies%2Fskos")
                .method(Method::PUT)
                .header("content-type", "application/rdf+xml")
                .body(Body::from(skos_bytes))?,
        )
        .await?;
    assert_eq!(put_response.status(), StatusCode::CREATED);

    let get_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fontologies%2Fskos")
                .method(Method::GET)
                .header("accept", "application/rdf+xml")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    assert_eq!(
        get_response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/rdf+xml")
    );

    let body = axum::body::to_bytes(get_response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("broaderTransitive"));
    Ok(())
}
