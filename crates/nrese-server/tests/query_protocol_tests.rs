//! Q1 gate (HTTP side): the query timeout is a real deadline, large results stream
//! completely, and the SPARQL Protocol dataset parameters are honoured.

mod support;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::{PolicyConfig, RequestTimeouts};
use nrese_store::StoreConfig;
use tower::util::ServiceExt;

use support::{body_text, test_app_with_store_config};

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

#[tokio::test]
async fn a_query_past_its_deadline_times_out() -> Result<(), Box<dyn std::error::Error>> {
    let (app, _dir) = app_with_triples(2_000, Duration::from_millis(300))?;
    let query = "SELECT (COUNT(*) AS ?n) WHERE { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i }";
    let started = Instant::now();
    let response = app
        .oneshot(get(&format!("/dataset/query?query={}", encode(query)))?)
        .await?;
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "answered at the deadline, not when evaluation ended: {:?}",
        started.elapsed()
    );
    Ok(())
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
