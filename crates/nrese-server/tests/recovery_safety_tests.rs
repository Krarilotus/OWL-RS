mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use tower::util::ServiceExt;

use support::{query_text, readyz_text, test_app, test_app_with_settings};

#[tokio::test]
async fn malformed_update_keeps_ready_revision_unchanged() -> Result<(), Box<dyn std::error::Error>>
{
    let app = test_app()?;

    let initial_text = readyz_text(app.clone()).await?;
    assert!(initial_text.contains("\"revision\":0"));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from("INSERT DATA {"))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let final_text = readyz_text(app).await?;
    assert!(final_text.contains("\"revision\":0"));
    assert!(final_text.contains("\"status\":\"ready\""));

    Ok(())
}

#[tokio::test]
async fn reasoner_reject_keeps_ready_revision_unchanged_and_data_unpublished()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::RulesMvp),
    )?;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA {
                        <http://example.com/Parent> <http://www.w3.org/2002/07/owl#disjointWith> <http://example.com/Other> .
                        <http://example.com/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Parent> .
                        <http://example.com/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Other> .
                    }",
                ))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let ready_text = readyz_text(app.clone()).await?;
    assert!(ready_text.contains("\"revision\":0"));
    assert!(ready_text.contains("\"status\":\"ready\""));

    assert!(
        query_text(
            app,
            "ASK WHERE { <http://example.com/alice> a <http://example.com/Other> }",
        )
        .await?
        .contains("false")
    );

    Ok(())
}

#[tokio::test]
async fn malformed_graph_replace_keeps_revision_unchanged_and_data_published()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;

    let seed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:live-s ex:p ex:live-o .",
                ))?,
        )
        .await?;
    assert_eq!(seed.status(), StatusCode::NO_CONTENT);

    let failed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:broken ex:p ",
                ))?,
        )
        .await?;
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);

    let ready_text = readyz_text(app.clone()).await?;
    assert!(ready_text.contains("\"revision\":1"));
    assert!(ready_text.contains("\"status\":\"ready\""));

    assert!(
        query_text(
            app,
            "ASK WHERE { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
        )
        .await?
        .contains("true")
    );

    Ok(())
}
