mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::auth::{AuthConfig, StaticBearerConfig};
use nrese_server::policy::{PolicyConfig, RateLimitConfig};
use nrese_store::{StoreConfig, StoreMode};
use tower::util::ServiceExt;

use support::{test_app_with_policy, test_app_with_store_config};

fn admin_policy() -> PolicyConfig {
    PolicyConfig {
        auth: AuthConfig::BearerStatic(StaticBearerConfig {
            read_token: Some("reader".to_owned()),
            admin_token: "admin".to_owned(),
        }),
        ..PolicyConfig::default()
    }
}

#[tokio::test]
async fn backup_endpoint_requires_admin_policy() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(admin_policy())?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/admin/dataset/backup")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn backup_endpoint_returns_dataset_payload_with_expected_headers()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let app = test_app_with_store_config(
        StoreConfig {
            mode: StoreMode::InMemory,
            data_dir: temp.path().join("unused"),
            preload_ontology: true,
            ontology_path: Some(support::minimal_fixture_path()),
            ontology_fallbacks: Vec::new(),
        },
        admin_policy(),
        ReasonerConfig::default(),
    )?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/admin/dataset/backup")
                .method(Method::GET)
                .header("authorization", "Bearer admin")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/n-quads")
    );
    assert_eq!(
        response
            .headers()
            .get("x-nrese-backup-format")
            .and_then(|value| value.to_str().ok()),
        Some("n-quads")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("http://example.com/alice"));

    Ok(())
}

#[tokio::test]
async fn restore_endpoint_replaces_dataset_and_updates_ready_surface()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(admin_policy())?;

    let backup_payload = b"<http://example.com/restored-s> <http://example.com/p> <http://example.com/restored-o> .\n".to_vec();
    let restore = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ops/api/admin/dataset/restore")
                .method(Method::POST)
                .header("authorization", "Bearer admin")
                .header("content-type", "application/n-quads")
                .body(Body::from(backup_payload))?,
        )
        .await?;

    assert_eq!(restore.status(), StatusCode::OK);
    let restore_body = axum::body::to_bytes(restore.into_body(), usize::MAX).await?;
    let restore_text = String::from_utf8(restore_body.to_vec())?;
    assert!(restore_text.contains("\"status\":\"restored\""));
    assert!(restore_text.contains("\"revision\":1"));

    let ask = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Chttp%3A%2F%2Fexample.com%2Frestored-s%3E%20%3Chttp%3A%2F%2Fexample.com%2Fp%3E%20%3Chttp%3A%2F%2Fexample.com%2Frestored-o%3E%20%7D")
                .method(Method::GET)
                .header("authorization", "Bearer reader")
                .body(Body::empty())?,
        )
        .await?;
    let ask_body = axum::body::to_bytes(ask.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ask_body.to_vec())?.contains("true"));

    let ready = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    let ready_body = axum::body::to_bytes(ready.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ready_body.to_vec())?.contains("\"revision\":1"));

    Ok(())
}

#[tokio::test]
async fn restore_endpoint_rejects_invalid_payload_with_problem_json()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(admin_policy())?;

    let seed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("authorization", "Bearer admin")
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA { <http://example.com/live-s> <http://example.com/p> <http://example.com/live-o> }",
                ))?,
        )
        .await?;
    assert_eq!(seed.status(), StatusCode::NO_CONTENT);

    let restore = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ops/api/admin/dataset/restore")
                .method(Method::POST)
                .header("authorization", "Bearer admin")
                .header("content-type", "application/n-quads")
                .body(Body::from("not valid n-quads"))?,
        )
        .await?;

    assert_eq!(restore.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        restore
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );

    let ask = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Chttp%3A%2F%2Fexample.com%2Flive-s%3E%20%3Chttp%3A%2F%2Fexample.com%2Fp%3E%20%3Chttp%3A%2F%2Fexample.com%2Flive-o%3E%20%7D")
                .method(Method::GET)
                .header("authorization", "Bearer reader")
                .body(Body::empty())?,
        )
        .await?;
    let ask_body = axum::body::to_bytes(ask.into_body(), usize::MAX).await?;
    assert!(String::from_utf8(ask_body.to_vec())?.contains("true"));

    Ok(())
}

#[tokio::test]
async fn admin_backup_endpoint_is_rate_limited_when_enabled()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(PolicyConfig {
        rate_limits: RateLimitConfig {
            admin_requests_per_window: 1,
            ..RateLimitConfig::default()
        },
        ..admin_policy()
    })?;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ops/api/admin/dataset/backup")
                .method(Method::GET)
                .header("authorization", "Bearer admin")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/admin/dataset/backup")
                .method(Method::GET)
                .header("authorization", "Bearer admin")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    Ok(())
}
