use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_engine::fault_points::MaintenancePanic;
use nrese_reasoner::ReasonerConfig;
use nrese_server::build_app;
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;
use tower::util::ServiceExt;

use crate::support::{body_text, test_state_with_store_config};

const DEADLINE: Duration = Duration::from_secs(10);

fn update(uri: &str, n: u32) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/sparql-update")
        .body(Body::from(format!(
            "INSERT DATA {{ <http://example.org/s{n}> <http://example.org/p> 1 }}"
        )))
        .unwrap()
}

async fn degraded_health(app: &Router) -> (StatusCode, serde_json::Value) {
    let until = tokio::time::Instant::now() + DEADLINE / 2;
    loop {
        let answer = app
            .clone()
            .oneshot(Request::get("/api/v1/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let code = answer.status();
        let health = body_text(answer).await.unwrap();
        let document: serde_json::Value = serde_json::from_str(&health).unwrap();
        if !document["degraded"].is_null() || tokio::time::Instant::now() >= until {
            return (code, document);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Spec: extended health names a second repository whose maintenance worker stops.
#[tokio::test]
async fn maintenance_panic_in_a_second_repository_is_visible_in_health() {
    const SECOND: &str = "/api/v1/repositories/second";
    let dir = tempfile::tempdir().unwrap();
    let mut config = StoreConfig::on_disk(dir.path());
    config.checkpoint_after_wal_bytes = 1;
    let state =
        test_state_with_store_config(config, PolicyConfig::default(), ReasonerConfig::default())
            .unwrap();
    let app = build_app(state.clone());

    tokio::time::timeout(DEADLINE, async {
        let created = app
            .clone()
            .oneshot(
                Request::put(SECOND)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let second = state.for_repository("second").unwrap().store();
        let fault = MaintenancePanic::arm(&second.config().data_dir, "checkpoint");
        let committed = app
            .clone()
            .oneshot(update("/api/v1/repositories/second/update", 0))
            .await
            .unwrap();
        assert_eq!(committed.status(), StatusCode::NO_CONTENT);
        tokio::task::spawn_blocking(move || fault.wait(DEADLINE))
            .await
            .unwrap();

        let (code, health) = degraded_health(&app).await;
        assert_eq!(
            health["degraded"],
            serde_json::json!(["second: maintenance ended: checkpoint"])
        );
        assert_eq!(code, StatusCode::OK);
        assert_eq!(health["ready"], true);
    })
    .await
    .expect("maintenance health scenario exceeded its deadline");
}

/// Spec: the maintenance worker contract says extended health reports a checkpoint panic,
/// later commits still answer successfully, and their WAL grows without a checkpoint.
#[tokio::test]
async fn maintenance_checkpoint_panic_is_visible_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = StoreConfig::on_disk(dir.path());
    config.checkpoint_after_wal_bytes = 1;
    let state =
        test_state_with_store_config(config, PolicyConfig::default(), ReasonerConfig::default())
            .unwrap();
    let app = build_app(state.clone());
    let fault = MaintenancePanic::arm(dir.path(), "checkpoint");

    tokio::time::timeout(DEADLINE, async {
        let first = app
            .clone()
            .oneshot(update("/dataset/update", 0))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::NO_CONTENT);
        fault.wait(DEADLINE);
        let before = state.store().engine_stats();
        let second = app
            .clone()
            .oneshot(update("/dataset/update", 1))
            .await
            .unwrap();
        let after = state.store().engine_stats();

        let until = tokio::time::Instant::now() + DEADLINE / 2;
        let health = loop {
            let answer = app
                .clone()
                .oneshot(Request::get("/api/v1/health").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let health = body_text(answer).await.unwrap();
            let document: serde_json::Value = serde_json::from_str(&health).unwrap();
            if !document["maintenance_error"].is_null() || tokio::time::Instant::now() >= until {
                break document;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(health["maintenance_error"], "maintenance ended: checkpoint");
        assert_eq!(second.status(), StatusCode::NO_CONTENT);
        assert_eq!(after.checkpoints, before.checkpoints);
        assert!(after.wal_bytes_since_checkpoint > before.wal_bytes_since_checkpoint);
        println!(
            "maintenance health: {}; commit: {}; checkpoints: {}; WAL: {} -> {} bytes",
            health["maintenance_error"],
            second.status(),
            after.checkpoints,
            before.wal_bytes_since_checkpoint,
            after.wal_bytes_since_checkpoint,
        );
    })
    .await
    .expect("maintenance HTTP scenario exceeded its deadline");
}
