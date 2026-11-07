//! Guard of what an RDF4J transaction's `DELETE` holds: its document is kept for the
//! commit as it came, and read as it goes, never kept as one owned pattern per statement.
//! A Turtle document with one long namespace and many short statements costs about 3 KB
//! per 11-byte statement as owned patterns (the upload sizing's witness). Measured
//! (`nrese_exec::measure`, every thread counts), so this file is a test binary of its own.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use nrese_exec::measure;
use nrese_reasoner::ReasonerService;
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// Statements in the document: held as owned patterns, about 150 MB.
const STATEMENTS: usize = 50_000;
/// What the request may take at its peak: the body, its few terms, the session.
const PEAK: usize = 32 << 20;

#[test]
fn a_transaction_delete_keeps_its_document_not_a_pattern_per_statement() {
    if !measure::alone() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let state = AppState::new(
        StoreService::new(StoreConfig::in_memory()).unwrap(),
        ReasonerService::new(Default::default()),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    );
    state.mark_ready();
    let app = build_app(state);
    let begin = runtime.block_on(
        app.clone().oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/repositories/nrese/transactions")
                .body(Body::empty())
                .unwrap(),
        ),
    );
    let begin = begin.unwrap();
    assert_eq!(begin.status(), StatusCode::CREATED);
    let location = begin.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let namespace = format!("http://example.com/{}/", "x".repeat(1_000));
    let mut document = format!("@prefix : <{namespace}> .\n");
    for _ in 0..STATEMENTS {
        document.push_str(":s :p :o .\n");
    }
    let request = Request::builder()
        .method(Method::PUT)
        .uri(format!("{location}?action=DELETE"))
        .header(header::CONTENT_TYPE, "text/turtle")
        .body(Body::from(document))
        .unwrap();
    let (response, peak) = measure::measure(|| runtime.block_on(app.clone().oneshot(request)));
    assert_eq!(response.unwrap().status(), StatusCode::OK);
    assert!(
        peak < PEAK,
        "a transaction DELETE: {} MiB at its peak, more than {} MiB",
        peak >> 20,
        PEAK >> 20
    );
}
