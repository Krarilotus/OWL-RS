//! Q1 gate (HTTP side): the query timeout is a real deadline, large results stream
//! completely, and the SPARQL Protocol dataset parameters are honoured.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::build_app;
use nrese_server::policy::{PolicyConfig, RequestTimeouts};
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

use crate::support::held_service::{HANG, HeldEndpoint};
use crate::support::{
    body_text, test_app_with_store, test_app_with_store_config, test_state_with_store_config,
};

/// An app whose store holds `count` triples (preloaded from an N-Triples file).
fn app_with_triples(
    count: usize,
    query_timeout: Duration,
) -> Result<(axum::Router, tempfile::TempDir), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("data.nt");
    let triples: String = (0..count)
        .map(|i| {
            format!(
                "<http://example.com/s{i}> <http://example.com/p{}> \"value {i}\" .\n",
                i % 5
            )
        })
        .collect();
    std::fs::write(&path, triples)?;
    let policy = PolicyConfig {
        timeouts: RequestTimeouts {
            query: query_timeout,
            ..RequestTimeouts::default()
        },
        ..PolicyConfig::default()
    };
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(path),
        policy,
        ReasonerConfig::default(),
    )?;
    Ok((app, dir))
}

fn get(uri: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::builder()
        .uri(uri)
        .method(Method::GET)
        .body(Body::empty())
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The deadline: a query held in a `SERVICE` call that never answers gets its 408 once the
/// deadline has passed, and lets go of the call.
/// The test's clock is paused: tokio doesn't move it while the query's blocking evaluator runs,
/// so the deadline passes when the test moves it, once the call has connected, however slow
/// the host.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_query_past_its_deadline_times_out() -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Duration::from_secs(1);
    let endpoint = HeldEndpoint::bind().await?;
    let store = StoreService::new(StoreConfig::in_memory())?;
    store.set_service_client(endpoint.client()?);
    let policy = PolicyConfig {
        timeouts: RequestTimeouts {
            query: deadline,
            ..RequestTimeouts::default()
        },
        ..PolicyConfig::default()
    };
    let app = test_app_with_store(store, policy, ReasonerConfig::default())?;
    let query = format!(
        "SELECT * WHERE {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        endpoint.url()
    );
    let started = tokio::time::Instant::now();
    let mut answer =
        tokio::spawn(app.oneshot(get(&format!("/dataset/query?query={}", encode(&query)))?));
    let call = endpoint.call(&mut answer).await?;
    tokio::time::advance(deadline + Duration::from_millis(1)).await;
    // Real time again for the bounds below.
    tokio::time::resume();
    let response = tokio::time::timeout(HANG, answer)
        .await
        .map_err(|_| format!("no answer in {HANG:?}"))???;
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(started.elapsed() >= deadline, "{:?}", started.elapsed());
    call.closed().await
}

#[tokio::test]
async fn large_results_stream_completely() -> Result<(), Box<dyn std::error::Error>> {
    let (app, _dir) = app_with_triples(5_000, Duration::from_secs(30))?;
    let query = "SELECT * WHERE { ?s ?p ?o }";
    let response = app
        .oneshot(get(&format!("/dataset/query?query={}", encode(query)))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/sparql-results+json"
    );
    let body = body_text(response).await?;
    assert!(
        body.len() > 4 * 64 * 1024,
        "spans several chunks: {}",
        body.len()
    );
    assert_eq!(body.matches("\"s\":").count(), 5_000);
    assert!(body.trim_end().ends_with('}'), "complete JSON document");
    Ok(())
}

#[tokio::test]
async fn syntax_errors_are_rejected_before_streaming() -> Result<(), Box<dyn std::error::Error>> {
    let (app, _dir) = app_with_triples(10, Duration::from_secs(30))?;
    let response = app
        .oneshot(get(&format!(
            "/dataset/query?query={}",
            encode("SELECT WHERE {")
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn dataset_parameters_select_the_graphs() -> Result<(), Box<dyn std::error::Error>> {
    let (app, _dir) = app_with_triples(0, Duration::from_secs(30))?;
    let update = "INSERT DATA {
        GRAPH <http://example.com/g1> { <http://example.com/a> <http://example.com/p> 1 }
        GRAPH <http://example.com/g2> { <http://example.com/b> <http://example.com/p> 2 }
    }";
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(update))?,
        )
        .await?;
    assert!(response.status().is_success(), "{}", response.status());

    let query = encode("SELECT ?s WHERE { ?s ?p ?o }");
    let g1 = encode("http://example.com/g1");
    let g2 = encode("http://example.com/g2");
    let rows = |body: &str| body.matches("\"value\"").count();

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/dataset/query?query={query}&default-graph-uri={g1}&default-graph-uri={g2}"
        ))?)
        .await?;
    assert_eq!(rows(&body_text(response).await?), 2);

    // Direct POST: the query in the body, the dataset in the URL.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/dataset/query?default-graph-uri={g2}"))
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .body(Body::from("SELECT ?s WHERE { ?s ?p ?o }"))?,
        )
        .await?;
    assert_eq!(rows(&body_text(response).await?), 1);

    let response = app
        .oneshot(get(&format!(
            "/dataset/query?query={query}&default-graph-uri=not%20an%20iri"
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn explain_reports_how_the_query_ran() -> Result<(), Box<dyn std::error::Error>> {
    let (app, _dir) = app_with_triples(1_000, Duration::from_secs(30))?;
    let query = "SELECT ?s ?v WHERE { ?s <http://example.com/p1> ?v . ?s ?p ?v }";
    let response = app
        .oneshot(get(&format!(
            "/dataset/query?query={}&explain=true",
            encode(query)
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/json");
    let explanation: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(explanation["executor"], "native");
    assert_eq!(explanation["rewrites"], serde_json::json!([]));
    assert_eq!(explanation["rows"], 200);
    let steps = explanation["steps"].as_array().expect("steps");
    assert_eq!(steps[0]["operator"], "project");
    assert!(
        steps
            .iter()
            .any(|s| s["operator"] == "scan" && s["estimated_rows"] == 200),
        "{steps:?}"
    );
    Ok(())
}

#[tokio::test]
async fn explain_plan_shows_the_plan_without_running_it() -> Result<(), Box<dyn std::error::Error>>
{
    let (app, _dir) = app_with_triples(1_000, Duration::from_secs(30))?;
    let query = "SELECT ?s ?v WHERE { ?s <http://example.com/p1> ?v . ?s ?p ?v }";
    let response = app
        .oneshot(get(&format!(
            "/dataset/query?query={}&explain=plan",
            encode(query)
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let planned: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(planned["rewrites"], serde_json::json!([]));
    let steps = planned["steps"].as_array().expect("steps");
    assert_eq!(steps[0]["operator"], "project");
    assert!(
        steps
            .iter()
            .any(|s| s["operator"] == "scan" && s["estimated_rows"] == 200),
        "{steps:?}"
    );
    // Nothing ran: no actual rows or times.
    assert!(steps.iter().all(|s| s.get("rows").is_none()), "{steps:?}");
    Ok(())
}

#[tokio::test]
async fn a_query_beyond_its_memory_limit_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let (app, _dir) = app_with_triples(2_000, Duration::from_secs(30))?;
    // 8·10⁹ rows: refused before any memory is taken, which the executor's join measures
    // (nrese-exec's tests/join_limits.rs); here, the refusal's status and message.
    let query = "SELECT (COUNT(*) AS ?n) WHERE { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i }";
    let response = app
        .oneshot(get(&format!("/dataset/query?query={}", encode(query)))?)
        .await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(body_text(response).await?.contains("memory budget"));
    Ok(())
}

/// The server's budget for all running queries: a query that fits its own limit and asks
/// for more than the server has left is told to come back, and the server's budget is
/// free again afterwards.
#[tokio::test]
async fn a_query_beyond_the_servers_memory_budget_is_told_to_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("data.nt");
    let triples: String = (0..2_000)
        .map(|i| format!("<http://example.com/s{i}> <http://example.com/p> \"value {i}\" .\n"))
        .collect();
    std::fs::write(&path, triples)?;
    let store = StoreConfig {
        total_query_memory_bytes: 8 << 20,
        ..StoreConfig::in_memory().with_ontology(path)
    };
    let app =
        test_app_with_store_config(store, PolicyConfig::default(), ReasonerConfig::default())?;
    // 4·10⁶ rows of six columns: within the query's own 4 GiB, beyond the server's 8 MiB.
    let query = "SELECT * WHERE { ?a ?b ?c . ?d ?e ?f }";
    let response = app
        .clone()
        .oneshot(get(&format!("/dataset/query?query={}", encode(query)))?)
        .await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_text(response).await?;
    assert!(
        body.contains("the server's query memory is in use") && body.contains("total_query_memory"),
        "{body}"
    );
    // Nothing of the failed query is still charged: a query that fits runs.
    let response = app
        .oneshot(get(&format!(
            "/dataset/query?query={}",
            encode("SELECT * WHERE { ?a ?b ?c }")
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn a_repository_answers_within_its_own_query_timeout()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    // The server's budget: an hour, past anything this test waits for.
    let policy = PolicyConfig {
        timeouts: RequestTimeouts {
            query: Duration::from_secs(3_600),
            ..RequestTimeouts::default()
        },
        ..PolicyConfig::default()
    };
    let state = test_state_with_store_config(
        StoreConfig::on_disk(dir.path()),
        policy,
        ReasonerConfig::default(),
    )?;
    let app = build_app(state.clone());
    let send = |method: Method, uri: &str, content_type: &str, body: String| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", content_type)
            .body(Body::from(body))
    };
    let created = app
        .clone()
        .oneshot(send(
            Method::PUT,
            "/api/v1/repositories/quick",
            "application/json",
            r#"{"query_timeout_ms": 1000}"#.to_owned(),
        )?)
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let refused = app
        .clone()
        .oneshot(send(
            Method::PUT,
            "/api/v1/repositories/never",
            "application/json",
            r#"{"query_timeout_ms": 0}"#.to_owned(),
        )?)
        .await?;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    // A query held in a SERVICE call: the repository's second stops it, not the server's
    // hour. The server installs its SERVICE client on the default repository only
    // (main.rs), so the test gives this one the endpoint's.
    let endpoint = HeldEndpoint::bind().await?;
    state
        .repositories()
        .get("quick")
        .ok_or("the repository just created")?
        .pipeline
        .read()
        .store()
        .set_service_client(endpoint.client()?);
    let query = format!(
        "SELECT * WHERE {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        endpoint.url()
    );
    // The clock paused, as in the deadline test: the second passes once the call has connected.
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let mut answer = tokio::spawn(app.clone().oneshot(get(&format!(
        "/api/v1/repositories/quick/query?query={}",
        encode(&query)
    ))?));
    let call = endpoint.call(&mut answer).await?;
    tokio::time::advance(Duration::from_millis(1_001)).await;
    tokio::time::resume();
    let response = tokio::time::timeout(HANG, answer)
        .await
        .map_err(|_| format!("no answer in {HANG:?}: not the repository's deadline"))???;
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    call.closed().await?;
    // The default repository keeps the server's budget.
    let response = app
        .oneshot(get(&format!(
            "/dataset/query?query={}",
            encode("SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }")
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}
