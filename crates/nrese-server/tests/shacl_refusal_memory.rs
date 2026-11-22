//! Follow a store refusal into its first HTTP response, with the write budget occupied
//! after listing. Check the budget while the response's actual bytes are still alive.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use nrese_exec::{Budget, Reservation};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::error::ApiError;
use nrese_store::{
    DEFAULT_SHAPES_GRAPH, GateSeverity, MutationError, MutationPipeline, ShaclGate, StoreConfig,
    StoreService,
};
use serde_json::Value;

const MESSAGE_BYTES: usize = 30 * 1024;

/// Spec: the write's existing refusal reservation can pay for
/// the full response. Its encoded bytes stay charged until their last owner drops.
#[tokio::test]
async fn the_response_keeps_its_encoded_shacl_refusal_charged() {
    let message = "x".repeat(MESSAGE_BYTES);
    let (error, memory, pressure) = refusal_under_pressure(&message, "second", 1);
    let mut response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response
        .body_mut()
        .frame()
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    drop(response);
    let problem: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(problem["type"], "https://nrese.dev/problems/shacl-reject");
    assert_eq!(
        problem["shacl"]["results"][0]["resultMessage"][0].as_str(),
        Some(message.as_str())
    );
    assert_eq!(problem["shacl"]["results_omitted"], 0);
    assert_eq!(memory.used() - pressure.bytes(), bytes.len());
    drop(bytes);
    assert_eq!(memory.used(), pressure.bytes());
}

/// Spec: if keeping the listed results needs a top-up the write
/// cannot afford, the committer still receives a SHACL refusal with the correct counts.
#[tokio::test]
async fn a_shacl_refusal_still_reports_counts_when_encoding_cannot_fit() {
    let message = "x".repeat(MESSAGE_BYTES);
    let (error, _memory, _pressure) = refusal_under_pressure("first", &message, 2);
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let problem: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(problem["type"], "https://nrese.dev/problems/shacl-reject");
    assert_eq!(problem["shacl"]["results"], serde_json::json!([]));
    assert_eq!(problem["shacl"]["failures"], serde_json::json!([]));
    assert_eq!(problem["shacl"]["results_omitted"], 2);
    assert_eq!(problem["shacl"]["failures_omitted"], 0);
    assert!(problem["reasoner_reject"].is_null());
}

fn refusal_under_pressure(
    first: &str,
    second: &str,
    listed: usize,
) -> (ApiError, Arc<Budget>, Reservation) {
    const PREFIX: &str =
        "PREFIX ex: <http://example.com/> PREFIX sh: <http://www.w3.org/ns/shacl#>";
    let store = StoreService::new(StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        total_query_memory_bytes: 1024 * 1024,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    let pipeline = MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    );
    pipeline
        .execute_update_str(&format!(
            r#"{PREFIX} INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                ex:FirstShape sh:targetClass ex:First ; sh:path ex:name ; sh:minCount 1 ; sh:message "{first}" .
                ex:SecondShape sh:targetClass ex:Second ; sh:path ex:name ; sh:minCount 1 ; sh:message "{second}" .
            }} }}"#
        ))
        .unwrap();
    let data = if listed == 1 {
        "ex:a a ex:First"
    } else {
        "ex:a a ex:First . ex:b a ex:Second"
    };
    let Err(MutationError::Rejected(rejected)) =
        pipeline.execute_update_str(&format!("{PREFIX} INSERT DATA {{ {data} }}"))
    else {
        panic!("expected a SHACL refusal");
    };
    let refusal = rejected.shacl.as_ref().expect("SHACL refusal");
    assert_eq!(
        (refusal.results.len(), refusal.results_omitted),
        (listed, 0)
    );
    let memory = Arc::clone(refusal.memory());
    let mut pressure = Reservation::new(Arc::clone(&memory));
    pressure.charge(memory.remaining()).unwrap();
    assert_eq!(memory.remaining(), 0);
    (ApiError::rejected(*rejected), memory, pressure)
}
