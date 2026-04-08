mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use tower::util::ServiceExt;

use support::{query_text, readyz_text, test_app_with_settings};

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

    assert!(readyz_text(app.clone()).await?.contains("\"revision\":1"));

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

    assert!(
        query_text(
            app.clone(),
            "ASK WHERE { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
        )
        .await?
        .contains("true")
    );

    assert!(readyz_text(app).await?.contains("\"revision\":1"));

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

    assert!(
        query_text(
            app.clone(),
            "ASK WHERE { <http://example.com/alice> a <http://example.com/Other> }",
        )
        .await?
        .contains("false")
    );

    assert!(readyz_text(app).await?.contains("\"revision\":0"));

    Ok(())
}
