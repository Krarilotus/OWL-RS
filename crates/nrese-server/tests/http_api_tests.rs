mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_store::{StoreConfig, StoreMode};
use tower::util::ServiceExt;

use nrese_server::policy::PolicyConfig;
use support::{minimal_fixture_path, test_app, test_app_with_settings, test_app_with_store_config};

#[tokio::test]
async fn version_endpoint_exposes_capabilities() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(Request::builder().uri("/version").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));
    Ok(())
}

#[tokio::test]
async fn metrics_endpoint_exposes_prometheus_text() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
    Ok(())
}

#[tokio::test]
async fn service_description_endpoint_serves_turtle() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/service-description")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/turtle; charset=utf-8")
    );
    Ok(())
}

#[tokio::test]
async fn construct_query_supports_rdf_xml_accept() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .header("accept", "application/rdf+xml")
                .body(Body::from(
                    "CONSTRUCT { <http://example.com/s> <http://example.com/p> <http://example.com/o> } WHERE {}",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/rdf+xml")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("rdf:RDF"));
    Ok(())
}

#[tokio::test]
async fn operator_ui_endpoint_serves_html() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    Ok(())
}

#[tokio::test]
async fn root_redirects_to_user_console() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some("/console")
    );
    Ok(())
}

#[tokio::test]
async fn operator_capabilities_endpoint_exposes_ops_contracts()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/capabilities")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("/ops/api/admin/dataset/backup"));
    assert!(text.contains("/ops/api/admin/dataset/restore"));
    assert!(text.contains("/ops/api/diagnostics/reasoning"));
    assert!(text.contains("/api/ai/query-suggestions"));
    assert!(text.contains("/console"));
    Ok(())
}

#[tokio::test]
async fn operator_runtime_diagnostics_endpoint_returns_json()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/runtime")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    Ok(())
}

#[tokio::test]
async fn operator_reasoning_diagnostics_endpoint_exposes_reject_baseline()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/reasoning")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("reject_diagnostics"));
    assert!(text.contains("hybrid-heuristic-plus-deep-justification"));
    Ok(())
}

#[tokio::test]
async fn rules_mvp_update_surfaces_last_reasoning_run_in_operator_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::RulesMvp),
    )?;

    let update_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA {
                        <http://example.com/Child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example.com/Parent> .
                        <http://example.com/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Child> .
                    }",
                ))?,
        )
        .await?;

    assert_eq!(update_response.status(), StatusCode::NO_CONTENT);

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
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("\"last_run\""));
    assert!(text.contains("\"completed\""));
    assert!(text.contains("http://example.com/alice"));
    assert!(text.contains("\"stats\""));
    assert!(text.contains("\"cache\""));
    assert!(text.contains("\"supported_asserted_triples\""));
    assert!(text.contains("\"inferred_equality_link_count\""));
    assert!(text.contains("\"subclass_edge_count\""));
    assert!(text.contains("\"execution_cache_entries\""));
    Ok(())
}

#[tokio::test]
async fn rules_mvp_accepts_functional_property_multiplicity_and_reports_inferred_equality()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::RulesMvp),
    )?;

    let update_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA {
                        <http://example.com/p> a <http://www.w3.org/2002/07/owl#FunctionalProperty> .
                        <http://example.com/alice> <http://example.com/p> <http://example.com/one> .
                        <http://example.com/alice> <http://example.com/p> <http://example.com/two> .
                    }",
                ))?,
        )
        .await?;

    assert_eq!(update_response.status(), StatusCode::NO_CONTENT);

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
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("\"completed\""));
    assert!(text.contains("\"inferred_equality_link_count\":1"));
    assert!(text.contains("http://example.com/one"));
    assert!(text.contains("http://example.com/two"));
    Ok(())
}

#[tokio::test]
async fn rules_mvp_rejects_disjoint_type_conflicts_and_surfaces_reason()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::RulesMvp),
    )?;

    let update_response = app
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

    assert_eq!(update_response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        update_response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    let update_body = axum::body::to_bytes(update_response.into_body(), usize::MAX).await?;
    let update_text = String::from_utf8(update_body.to_vec())?;
    assert!(update_text.contains("owl:disjointWith"));
    assert!(update_text.contains("reasoner_reject"));
    assert!(update_text.contains("likely_commit_trigger"));
    assert!(update_text.contains("commit_attribution"));
    assert!(update_text.contains("principal_contributor"));
    assert!(update_text.contains("\"evidence\""));
    assert!(update_text.contains("constraint-axiom"));
    assert!(update_text.contains("matched_evidence_roles"));
    assert!(update_text.contains("http://example.com/alice"));
    assert!(update_text.contains("Likely commit-local trigger"));

    let ask_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .body(Body::from(
                    "ASK WHERE { <http://example.com/alice> a <http://example.com/Other> }",
                ))?,
        )
        .await?;
    assert_eq!(ask_response.status(), StatusCode::OK);
    let ask_body = axum::body::to_bytes(ask_response.into_body(), usize::MAX).await?;
    let ask_text = String::from_utf8(ask_body.to_vec())?;
    assert!(ask_text.contains("false"));

    let diagnostics_response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/reasoning")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(diagnostics_response.status(), StatusCode::OK);
    let diagnostics_body =
        axum::body::to_bytes(diagnostics_response.into_body(), usize::MAX).await?;
    let diagnostics_text = String::from_utf8(diagnostics_body.to_vec())?;
    assert!(diagnostics_text.contains("\"rejected\""));
    assert!(diagnostics_text.contains("owl:disjointWith"));
    assert!(diagnostics_text.contains("\"last_reject\""));
    assert!(diagnostics_text.contains("likely_commit_trigger"));
    assert!(diagnostics_text.contains("commit_attribution"));
    assert!(diagnostics_text.contains("principal_contributor"));
    assert!(diagnostics_text.contains("\"evidence\""));
    assert!(diagnostics_text.contains("matched_evidence_roles"));
    Ok(())
}

#[tokio::test]
async fn rules_mvp_rejects_owl_nothing_type_conflicts() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::RulesMvp),
    )?;

    let update_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA {
                        <http://example.com/Impossible> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://www.w3.org/2002/07/owl#Nothing> .
                        <http://example.com/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Impossible> .
                    }",
                ))?,
        )
        .await?;

    assert_eq!(update_response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(update_response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("owl#Nothing"));
    assert!(text.contains("reasoner_reject"));
    Ok(())
}

#[tokio::test]
async fn graph_store_head_returns_content_type() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::HEAD)
                .header("accept", "text/turtle")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/turtle")
    );
    Ok(())
}

#[tokio::test]
async fn ttl_fixture_is_loaded_and_drives_reasoner_aware_update_flow()
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
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::RulesMvp),
    )?;

    let ready_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(ready_response.status(), StatusCode::OK);
    let ready_body = axum::body::to_bytes(ready_response.into_body(), usize::MAX).await?;
    let ready_text = String::from_utf8(ready_body.to_vec())?;
    assert!(ready_text.contains("minimal_services.ttl"));

    let update_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA {
                        <http://example.com/carol> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Child> .
                    }",
                ))?,
        )
        .await?;
    assert_eq!(update_response.status(), StatusCode::NO_CONTENT);

    let diagnostics_response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/reasoning")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(diagnostics_response.status(), StatusCode::OK);
    let diagnostics_body =
        axum::body::to_bytes(diagnostics_response.into_body(), usize::MAX).await?;
    let diagnostics_text = String::from_utf8(diagnostics_body.to_vec())?;
    assert!(diagnostics_text.contains("\"last_run\""));
    assert!(diagnostics_text.contains("\"completed\""));
    assert!(diagnostics_text.contains("\"stats\""));
    assert!(diagnostics_text.contains("\"inferred_triples\""));

    Ok(())
}
