//! Follow a store refusal into its first HTTP response, with the write budget occupied
//! after listing. Check the budget while the response's actual bytes are still alive.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use nrese_exec::{Budget, Reservation};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::error::ApiError;
use nrese_sparql::GraphAccess;
use nrese_store::{
    DEFAULT_SHAPES_GRAPH, GateSeverity, MutationCommand, MutationError, MutationPipeline,
    MutationReject, MutationTicket, ReadScope, Requester, ShaclGate, SparqlUpdateRequest,
    StoreConfig, StoreService, WriteScope,
};
use serde_json::Value;

const MESSAGE_BYTES: usize = 30 * 1024;
const OPEN: &str = "http://example.com/open";

/// Spec: the write's existing refusal reservation can pay for
/// the full response. Its encoded bytes stay charged until their last owner drops.
#[tokio::test]
async fn the_response_keeps_its_encoded_shacl_refusal_charged() {
    let message = "x".repeat(MESSAGE_BYTES);
    let (error, memory, pressure) = refusal_under_pressure(&message, "second", 1, false);
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
/// cannot afford, the committer still receives a SHACL refusal with the correct counts,
/// its detail the counts alone.
#[tokio::test]
async fn a_shacl_refusal_still_reports_counts_when_encoding_cannot_fit() {
    let problem = counts_only(false).await;
    assert_eq!(
        problem["detail"],
        "the commit introduces 2 SHACL result(s) the gate rejects"
    );
}

/// Spec (docs/ops/http-api.md): a committer with limited graph access
/// still reads, in the count-only refusal, that it is shown only what it can read.
#[tokio::test]
async fn a_restricted_committers_count_only_refusal_keeps_its_framing() {
    let problem = counts_only(true).await;
    let detail = problem["detail"].as_str().unwrap();
    assert!(
        detail.contains("the commit introduces 2 SHACL result(s) the gate rejects"),
        "{detail}"
    );
    assert!(
        detail.contains("Only statements in graphs you can read are shown"),
        "{detail}"
    );
}

/// Spec: the encoding reuses only the part of the refusal's
/// reservation its live texts don't need. Asked for more than the whole reservation,
/// the write holds the encoding and the listed message besides, before and after.
#[test]
fn the_encoding_leaves_the_listed_texts_charged() {
    let message = "x".repeat(MESSAGE_BYTES);
    let rejected = rejection(&message, "second", 1, false);
    let refusal = rejected.shacl.as_ref().expect("SHACL refusal");
    let memory = Arc::clone(refusal.memory());
    let encoding = memory.used();
    assert!(
        memory.remaining() >= encoding,
        "the test's budget is too small"
    );
    let held = refusal.reserve_encoding(encoding).unwrap();
    assert!(
        memory.used() >= encoding + MESSAGE_BYTES,
        "{} held for an encoding of {encoding}",
        memory.used()
    );
    drop(held);
    assert!(memory.used() >= MESSAGE_BYTES, "{}", memory.used());
}

/// The count-only answer to a refusal of two listed results whose encoding needs more
/// than the write has left: its common fields checked, the body returned.
async fn counts_only(restricted: bool) -> Value {
    let message = "x".repeat(MESSAGE_BYTES);
    let (error, _memory, _pressure) = refusal_under_pressure("first", &message, 2, restricted);
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let problem: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(problem["type"], "https://nrese.dev/problems/shacl-reject");
    assert_eq!(problem["shacl"]["results"], serde_json::json!([]));
    assert_eq!(problem["shacl"]["failures"], serde_json::json!([]));
    assert_eq!(problem["shacl"]["results_omitted"], 2);
    assert_eq!(problem["shacl"]["failures_omitted"], 0);
    assert_eq!(problem.get("reasoner_reject"), Some(&Value::Null));
    problem
}

/// A refusal of `listed` results (1 or 2), with the rest of the write's budget taken.
fn refusal_under_pressure(
    first: &str,
    second: &str,
    listed: usize,
    restricted: bool,
) -> (ApiError, Arc<Budget>, Reservation) {
    let rejected = rejection(first, second, listed, restricted);
    let refusal = rejected.shacl.as_ref().expect("SHACL refusal");
    let memory = Arc::clone(refusal.memory());
    let mut pressure = Reservation::new(Arc::clone(&memory));
    pressure.charge(memory.remaining()).unwrap();
    assert_eq!(memory.remaining(), 0);
    (ApiError::rejected(*rejected), memory, pressure)
}

/// A write refused by two shapes whose messages are `first` and `second`, with `listed`
/// of them violated, by a committer who reads everything or, if `restricted`, only the
/// written graph and the shapes graph.
fn rejection(first: &str, second: &str, listed: usize, restricted: bool) -> Box<MutationReject> {
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
    let (committer, update) = if restricted {
        let access = Arc::new(GraphAccess {
            graphs: vec![OPEN.to_owned(), DEFAULT_SHAPES_GRAPH.to_owned()],
            ..GraphAccess::default()
        });
        let committer = Requester::new(
            ReadScope::Graphs(Arc::clone(&access)),
            WriteScope::Graphs(access),
        );
        (
            committer,
            format!("{PREFIX} INSERT DATA {{ GRAPH <{OPEN}> {{ {data} }} }}"),
        )
    } else {
        (
            Requester::all(),
            format!("{PREFIX} INSERT DATA {{ {data} }}"),
        )
    };
    let result = pipeline.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(update)),
        &committer,
        &MutationTicket::new(),
    );
    let Err(MutationError::Rejected(rejected)) = result else {
        panic!("expected a SHACL refusal, got {result:?}");
    };
    let refusal = rejected.shacl.as_ref().expect("SHACL refusal");
    assert_eq!(
        (refusal.results.len(), refusal.results_omitted),
        (listed, 0)
    );
    rejected
}
