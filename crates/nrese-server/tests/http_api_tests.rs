mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::StoreConfig;
use tower::util::ServiceExt;

use nrese_server::DeploymentPosture;
use nrese_server::policy::PolicyConfig;
use support::{
    minimal_fixture_path, test_app, test_app_with_posture, test_app_with_settings,
    test_app_with_store_config,
};

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

/// `/version` names the semantics the closure has, so a client can tell when an upgrade
/// changes what is inferred.
#[tokio::test]
async fn version_endpoint_names_the_reasoning_semantics() -> Result<(), Box<dyn std::error::Error>>
{
    for (mode, expected) in [
        (ReasoningMode::Owl2Rl, Some("owl2-rl v")),
        (ReasoningMode::Rdfs, Some("rdfs v")),
        (ReasoningMode::Disabled, None),
    ] {
        let app = test_app_with_settings(PolicyConfig::default(), ReasonerConfig::for_mode(mode))?;
        let response = app
            .oneshot(Request::builder().uri("/version").body(Body::empty())?)
            .await?;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        match expected {
            Some(prefix) => {
                let semantics = json["reasoning_semantics"].as_str().expect("semantics");
                assert!(semantics.starts_with(prefix), "{semantics}");
                assert_eq!(semantics.rsplit(' ').next().map(str::len), Some(16));
            }
            None => assert!(json["reasoning_semantics"].is_null()),
        }
    }
    Ok(())
}

#[tokio::test]
async fn version_endpoint_reflects_disabled_optional_surfaces()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig {
            expose_operator_ui: false,
            expose_metrics: false,
            ..PolicyConfig::default()
        },
        ReasonerConfig::default(),
    )?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/version")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("\"operator_surface_enabled\":false"));
    assert!(text.contains("\"metrics_enabled\":false"));
    Ok(())
}

#[tokio::test]
async fn read_only_demo_reports_disabled_write_surfaces() -> Result<(), Box<dyn std::error::Error>>
{
    let app = test_app_with_posture(
        PolicyConfig::default(),
        ReasonerConfig::default(),
        DeploymentPosture::ReadOnlyDemo,
    )?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/version")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("\"deployment_posture\":\"read-only-demo\""));
    assert!(text.contains("\"graph_write_enabled\":false"));
    assert!(text.contains("\"sparql_update_enabled\":false"));
    assert!(text.contains("\"tell_enabled\":false"));
    assert!(text.contains("\"admin_surface_enabled\":false"));
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
async fn service_description_omits_disabled_optional_endpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig {
            expose_operator_ui: false,
            expose_metrics: false,
            ..PolicyConfig::default()
        },
        ReasonerConfig::default(),
    )?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/service-description")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(!text.contains("nrese:metricsEndpoint"));
    assert!(!text.contains("nrese:operatorEndpoint"));
    Ok(())
}

#[tokio::test]
async fn read_only_demo_service_description_omits_mutation_endpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_posture(
        PolicyConfig::default(),
        ReasonerConfig::default(),
        DeploymentPosture::ReadOnlyDemo,
    )?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/service-description")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(!text.contains("nrese:updateEndpoint"));
    assert!(!text.contains("nrese:tellEndpoint"));
    assert!(text.contains("nrese:graphStoreWriteEnabled \"false\""));
    assert!(text.contains("nrese:sparqlUpdateEnabled \"false\""));
    assert!(text.contains("nrese:tellEnabled \"false\""));
    Ok(())
}

#[tokio::test]
async fn read_only_demo_returns_not_found_for_mutation_surfaces()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_posture(
        PolicyConfig::default(),
        ReasonerConfig::default(),
        DeploymentPosture::ReadOnlyDemo,
    )?;

    let update_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from("INSERT DATA {}"))?,
        )
        .await?;
    assert_eq!(update_response.status(), StatusCode::NOT_FOUND);

    let tell_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/tell")
                .method(Method::POST)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "<http://example.com/s> <http://example.com/p> <http://example.com/o> .",
                ))?,
        )
        .await?;
    assert_eq!(tell_response.status(), StatusCode::NOT_FOUND);

    let graph_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "<http://example.com/s> <http://example.com/p> <http://example.com/o> .",
                ))?,
        )
        .await?;
    assert_eq!(graph_response.status(), StatusCode::NOT_FOUND);
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
    assert!(text.contains("rule-premises-plus-commit-delta-attribution"));
    Ok(())
}

#[tokio::test]
async fn owl2_rl_update_surfaces_last_reasoning_run_in_operator_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl),
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
    assert!(text.contains("\"ruleset\":\"owl2-rl\""));
    assert!(text.contains("\"inferred_inserted\""));
    assert!(text.contains("\"rounds\""));
    assert!(text.contains("\"elapsed_micros\""));
    Ok(())
}

#[tokio::test]
async fn owl2_rl_accepts_functional_property_multiplicity_and_reports_inferred_equality()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl),
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
    // prp-fp derives one sameAs two (and its symmetric and reflexive consequences).
    assert!(!text.contains("\"inferred_inserted\":0"));
    Ok(())
}

#[tokio::test]
async fn owl2_rl_rejects_disjoint_type_conflicts_and_surfaces_reason()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl),
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
    assert!(update_text.contains("cax-dw"));
    assert!(update_text.contains("reasoner_reject"));
    assert!(update_text.contains("likely_commit_trigger"));
    assert!(update_text.contains("commit_attribution"));
    assert!(update_text.contains("\"evidence\""));
    assert!(update_text.contains("\"premise\""));
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
    assert!(diagnostics_text.contains("cax-dw"));
    assert!(diagnostics_text.contains("\"last_reject\""));
    assert!(diagnostics_text.contains("likely_commit_trigger"));
    assert!(diagnostics_text.contains("commit_attribution"));
    assert!(diagnostics_text.contains("\"evidence\""));
    assert!(diagnostics_text.contains("matched_evidence_roles"));
    Ok(())
}

#[tokio::test]
async fn owl2_rl_rejects_owl_nothing_type_conflicts() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl),
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
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(minimal_fixture_path()),
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl),
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
    assert!(diagnostics_text.contains("\"rounds\""));
    assert!(diagnostics_text.contains("\"inferred_triples\""));

    Ok(())
}

/// The user console comes out of the binary: the page, its hashed assets (cacheable) and
/// its runtime configuration. A binary built without the console says so instead.
#[tokio::test]
async fn the_console_is_served_from_the_binary() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let get = |uri: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
        }
    };
    let version: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(get("/version").await?.into_body(), usize::MAX).await?,
    )?;
    let page = get("/console").await?;
    if version["user_console_embedded"] != true {
        // No console build beside this checkout (the Rust CI job): the page explains it.
        assert_eq!(page.status(), StatusCode::SERVICE_UNAVAILABLE);
        return Ok(());
    }
    assert_eq!(page.status(), StatusCode::OK);
    let html = String::from_utf8(
        axum::body::to_bytes(page.into_body(), usize::MAX)
            .await?
            .to_vec(),
    )?;
    // Every script and stylesheet the page names is served, with its type.
    let mut assets = 0;
    for reference in html
        .split('"')
        .filter(|part| part.starts_with("/console/assets/"))
    {
        let uri: &'static str = Box::leak(reference.to_owned().into_boxed_str());
        let response = get(uri).await?;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        assert!(
            header("content-type").starts_with("text/javascript")
                || header("content-type").starts_with("text/css"),
            "{uri}: {}",
            header("content-type")
        );
        assert!(header("cache-control").contains("immutable"), "{uri}");
        assets += 1;
    }
    assert!(assets >= 1, "{html}");
    let config = get("/console/console-config.js").await?;
    assert_eq!(config.status(), StatusCode::OK);
    assert_eq!(
        config
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    assert_eq!(
        get("/console/assets/missing.js").await?.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get("/console/../Cargo.toml").await?.status(),
        StatusCode::NOT_FOUND
    );
    Ok(())
}
