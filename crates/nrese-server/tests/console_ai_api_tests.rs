mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::util::ServiceExt;

use support::test_app;

#[tokio::test]
async fn ai_status_endpoint_reports_disabled_by_default() -> Result<(), Box<dyn std::error::Error>>
{
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/ai/status")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("\"enabled\":false"));
    assert!(text.contains("\"provider\":\"disabled\""));
    Ok(())
}

#[tokio::test]
async fn query_suggestions_endpoint_rejects_when_disabled() -> Result<(), Box<dyn std::error::Error>>
{
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/ai/query-suggestions")
                .method(Method::POST)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"prompt":"show me useful overview queries","locale":"en"}"#,
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("disabled"));
    Ok(())
}
