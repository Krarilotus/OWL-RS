mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use tower::util::ServiceExt;

use support::{body_text, query_text, readyz_text, test_app, test_app_with_settings};

#[tokio::test]
async fn graph_put_rejects_unsupported_content_type_with_problem_json()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Funsupported")
                .method(Method::PUT)
                .header("content-type", "application/json")
                .body(Body::from("{\"not\":\"rdf\"}"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let text = body_text(response).await?;
    assert!(text.contains("unsupported graph content type"));

    Ok(())
}

#[tokio::test]
async fn graph_put_rejects_malformed_turtle_with_problem_json()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fmalformed")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:broken ex:p ",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let text = body_text(response).await?;
    assert!(text.contains("Bad Request"));

    Ok(())
}

#[tokio::test]
async fn graph_roundtrip_supports_rdf_xml() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let put_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Frdfxml")
                .method(Method::PUT)
                .header("content-type", "application/rdf+xml")
                .body(Body::from(
                    r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:ex="http://example.com/">
  <rdf:Description rdf:about="http://example.com/a">
    <ex:p rdf:resource="http://example.com/b"/>
  </rdf:Description>
</rdf:RDF>
"#,
                ))?,
        )
        .await?;
    assert_eq!(put_response.status(), StatusCode::CREATED);

    let get_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Frdfxml")
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
    let text = body_text(get_response).await?;
    assert!(text.contains("rdf:RDF"));
    assert!(text.contains("http://example.com/a"));

    Ok(())
}

#[tokio::test]
async fn graph_put_honors_content_location_as_base_iri() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let put_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .header("content-location", "https://www.w3.org/ns/prov.ttl")
                .body(Body::from(
                    "@prefix : <#> . :generated <http://www.w3.org/2002/07/owl#inverseOf> :wasGeneratedBy .",
                ))?,
        )
        .await?;
    assert_eq!(put_response.status(), StatusCode::NO_CONTENT);

    let get_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=PREFIX%20owl%3A%20%3Chttp%3A%2F%2Fwww.w3.org%2F2002%2F07%2Fowl%23%3E%20ASK%20WHERE%20%7B%20%3Chttps%3A%2F%2Fwww.w3.org%2Fns%2Fprov.ttl%23generated%3E%20owl%3AinverseOf%20%3Chttps%3A%2F%2Fwww.w3.org%2Fns%2Fprov.ttl%23wasGeneratedBy%3E%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    assert!(body_text(get_response).await?.contains("true"));

    Ok(())
}

#[tokio::test]
async fn graph_put_returns_ok_when_replacing_existing_named_graph()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let payload = r#"@prefix ex: <http://example.com/> . ex:a ex:p ex:b ."#;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fexisting")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(payload))?,
        )
        .await?;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fexisting")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(payload))?,
        )
        .await?;
    assert_eq!(second.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn graph_put_uses_reasoner_gate_and_rejects_without_publish()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::RulesMvp),
    )?;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> .
                     @prefix owl: <http://www.w3.org/2002/07/owl#> .
                     @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
                     ex:Parent owl:disjointWith ex:Other .
                     ex:alice rdf:type ex:Parent .
                     ex:alice rdf:type ex:Other .",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let text = body_text(response).await?;
    assert!(text.contains("reasoner_reject"));
    assert!(text.contains("owl:disjointWith"));

    assert!(
        query_text(
            app.clone(),
            "ASK WHERE { <http://example.com/alice> a <http://example.com/Other> }",
        )
        .await?
        .contains("false")
    );

    let ready_text = readyz_text(app).await?;
    assert!(ready_text.contains("\"revision\":0"));

    Ok(())
}
