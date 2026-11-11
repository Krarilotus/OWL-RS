//! D16-489: the server's process-wide store settings (index encoding, vocabulary, huge pages,
//! reasoning release) are its configuration's, from start-up on, whatever stores it opens
//! afterwards. Each test in a process of its own ([`measure::alone`]): the settings are the
//! process's.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_exec::measure;
use nrese_exec::memory::HugePages;
use nrese_reasoner::ReasonerService;
use nrese_server::ai::AiSuggestionService;
use nrese_server::{AppState, DeploymentPosture, ServerConfig, build_app};
use nrese_store::{
    IndexEncoding, ProcessSettings, StoreConfig, StoreService, VocabularyEncoding, process_settings,
};
use serde_json::{Value, json};
use tower::util::ServiceExt;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// An operator's configuration with a value other than the default for each process-wide
/// setting.
fn operators() -> ServerConfig {
    ServerConfig {
        bind_address: "127.0.0.1:0".parse().unwrap(),
        deployment_posture: DeploymentPosture::OpenWorkbench,
        store: StoreConfig {
            index_encoding: IndexEncoding::Compact,
            vocabulary: VocabularyEncoding::Fsst,
            reasoning_release: true,
            huge_pages: HugePages::Off,
            ..StoreConfig::in_memory()
        },
        reasoner: Default::default(),
        policy: Default::default(),
        ai: Default::default(),
        replication: Default::default(),
    }
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Option<Value>) -> Value {
    let request = Request::builder()
        .uri(uri)
        .method(method)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(status, StatusCode::OK, "{uri}: {text}");
    serde_json::from_str(&text).unwrap()
}

/// Started as `main` starts it (the configuration installed, the default repository opened,
/// then the app state with its system store), the server runs with the operator's values,
/// and still does after a draft check opened its scratch store.
#[test]
fn the_configured_settings_hold_through_start_up_and_a_draft_check() {
    if !measure::alone() {
        return;
    }
    let config = operators();
    let configured = Some(ProcessSettings::of(&config.store));
    config.install_process_settings().unwrap();
    let store = StoreService::new(config.store.clone()).unwrap();
    let state = AppState::try_new(
        store,
        ReasonerService::new(config.reasoner.clone()),
        config.policy.clone(),
        AiSuggestionService::disabled(),
        config.deployment_posture,
    )
    .unwrap();
    assert_eq!(process_settings(), configured, "after start-up");

    state.mark_ready();
    let app = build_app(state);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let reply = runtime.block_on(async {
        let declared = call(&app, Method::GET, "/api/v1/draft-check/capabilities", None).await;
        let request = json!({
            "protocol": "dmw-store-check/1",
            "operation": "shacl",
            "request_sha256": "a".repeat(64),
            "scope": {"project_id": "project", "owner_id": "owner"},
            "build_id": declared["build_id"],
            "semantics_id": declared["semantics"][0],
            "profile": "owl2-rl",
            "input_hashes": {"validation": "b".repeat(64)},
            "limits": {"max_seconds": 5.0, "max_results": 10, "max_triples": 100},
            "inputs": {
                "data": "<https://example.test/a> <https://example.test/p> <https://example.test/b> .\n",
                "schema": "",
                "shapes": ""
            }
        });
        call(&app, Method::POST, "/api/v1/draft-check", Some(request)).await
    });
    assert_eq!(reply["terminal_status"], "completed", "{reply}");
    assert_eq!(process_settings(), configured, "after a draft check");
}

/// A store opened before the configuration is installed (one that sets the defaults) makes
/// start-up fail instead of leaving the server on the defaults.
#[test]
fn a_store_opened_before_the_configuration_stops_start_up() {
    if !measure::alone() {
        return;
    }
    let _early = StoreService::new(StoreConfig::in_memory()).unwrap();
    let message = format!("{:#}", operators().install_process_settings().unwrap_err());
    assert!(
        message.contains("before the configuration was installed"),
        "{message}"
    );
}
