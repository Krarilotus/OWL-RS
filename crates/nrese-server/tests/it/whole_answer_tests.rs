//! Spec: an HTTP/1.0 request gets every streamed answer whole, with its `Content-Length`
//! (result_stream's "HTTP/1.0"): on every route that streams, the same answer HTTP/1.1 gets.
//! The answer held is charged to the request's memory envelope (`budgets.query_memory`):
//! past it the request is refused with 413, and what it held is released when it is sent or
//! refused, or when its client leaves.

use std::error::Error;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, Version, header};
use nrese_reasoner::ReasonerConfig;
use nrese_server::auth::{AuthConfig, StaticBearerConfig};
use nrese_server::policy::PolicyConfig;
use nrese_store::{StoreConfig, StoreService};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::util::ServiceExt;

use crate::cut_body_wire_tests::{WHOLE, query};
use crate::support::held_service::{HANG, HeldEndpoint};
use crate::support::{minimal_fixture_path, test_app_with_store, test_app_with_store_config};

/// A store whose queries share a budget, so that what a request holds shows in it.
fn shared_store() -> Result<StoreService, Box<dyn Error>> {
    Ok(StoreService::new(StoreConfig {
        total_query_memory_bytes: 1 << 30,
        ..StoreConfig::in_memory()
    })?)
}

/// The bytes the queries' shared budget holds.
fn held(store: &StoreService) -> usize {
    store.query_memory().expect("a shared budget").0
}

/// Waits, in real time, until the queries' shared budget holds `bytes` again.
async fn released_to(store: &StoreService, bytes: usize) {
    let mut waited = Duration::ZERO;
    while held(store) != bytes {
        assert!(waited < HANG, "{} bytes still held", held(store));
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += Duration::from_millis(10);
    }
}

fn query_request(version: Version) -> Result<Request<Body>, axum::http::Error> {
    Request::post("/dataset/query")
        .version(version)
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/csv")
        .body(Body::from(query()))
}

#[tokio::test]
async fn an_http10_answer_is_sent_whole_and_its_memory_released() -> Result<(), Box<dyn Error>> {
    let store = shared_store()?;
    let app = test_app_with_store(
        store.clone(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    let before = held(&store);
    let response = app.oneshot(query_request(Version::HTTP_10)?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_LENGTH),
        Some(&WHOLE.into())
    );
    // All of it held before its first byte goes out, in the request's envelope.
    assert!(held(&store) >= before + WHOLE, "{} held", held(&store));
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.len(), WHOLE);
    assert_eq!(held(&store), before);
    Ok(())
}

#[tokio::test]
async fn an_http10_answer_past_its_envelope_is_refused_and_its_memory_released()
-> Result<(), Box<dyn Error>> {
    let store = shared_store()?;
    let mut policy = PolicyConfig::default();
    policy.limits.max_query_memory_bytes = 1 << 20;
    let app = test_app_with_store(store.clone(), policy, ReasonerConfig::default())?;
    let before = held(&store);
    let response = app.oneshot(query_request(Version::HTTP_10)?).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let problem: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    assert_eq!(
        problem["type"],
        "https://nrese.dev/problems/payload-too-large"
    );
    assert_eq!(
        problem["detail"],
        "This answer is larger than the 1 MiB an HTTP/1.0 request can be answered with: \
         HTTP/1.0 has no way to mark a streamed answer as cut, so the server sends it only \
         whole. Ask over HTTP/1.1 or HTTP/2, which stream it. Behind a reverse proxy, set the \
         proxy's upstream HTTP version to 1.1 (for nginx: `proxy_http_version 1.1;`)."
    );
    released_to(&store, before).await;
    Ok(())
}

/// While the memory all queries share is taken, an HTTP/1.0 answer that outgrows it is turned
/// away for now, as any request that can't have its memory: 503 with `Retry-After: 1`, and
/// what it held goes back. Its own envelope (4 GiB) has room; the shared total (1 MiB) not.
#[tokio::test]
async fn an_http10_answer_past_the_shared_memory_is_turned_away_for_now()
-> Result<(), Box<dyn Error>> {
    let store = StoreService::new(StoreConfig {
        total_query_memory_bytes: 1 << 20,
        ..StoreConfig::in_memory()
    })?;
    let app = test_app_with_store(
        store.clone(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    let before = held(&store);
    let response = app.oneshot(query_request(Version::HTTP_10)?).await?;
    let status = response.status();
    let retry = response.headers().get(header::RETRY_AFTER).cloned();
    let problem: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    eprintln!("{status}; retry-after {retry:?}; {problem}");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry, Some(HeaderValue::from_static("1")));
    released_to(&store, before).await;
    Ok(())
}

/// A query whose own memory grows with its answer: every row is a distinct pair the
/// `DISTINCT` keeps, and a short line of CSV.
fn distinct_query() -> String {
    let values: String = (1..=400).map(|n| format!("{n} ")).collect();
    format!("SELECT DISTINCT ?a ?b WHERE {{ VALUES ?a {{ {values} }} VALUES ?b {{ {values} }} }}")
}

/// [`distinct_query`] asked as `version` in a memory limit of `limit` bytes, on a store of
/// its own (no answer cached from before): the status and the body, once all the request
/// held has gone back.
async fn distinct_answer(
    limit: usize,
    version: Version,
) -> Result<(StatusCode, axum::body::Bytes), Box<dyn Error>> {
    let store = shared_store()?;
    let mut policy = PolicyConfig::default();
    policy.limits.max_query_memory_bytes = limit;
    let app = test_app_with_store(store.clone(), policy, ReasonerConfig::default())?;
    let before = held(&store);
    let request = Request::post("/dataset/query")
        .version(version)
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/csv")
        .body(Body::from(distinct_query()))?;
    let response = app.oneshot(request).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    released_to(&store, before).await;
    Ok((status, body))
}

/// The query's own work and its answer held whole draw on one envelope. When the query's
/// work is refused while part of its answer is held, the request gets the same 413 as an
/// answer too large for HTTP/1.0, which tells the client to ask over HTTP/1.1, not to raise
/// the query's memory: over HTTP/1.1, in the same limit, the same query is answered.
#[tokio::test]
async fn an_http10_query_whose_work_is_refused_beside_its_held_answer_is_told_to_stream()
-> Result<(), Box<dyn Error>> {
    // The least limit, to 16 KiB, that HTTP/1.1 answers the query in.
    let (mut refused, mut answered) = (0, 256 << 20);
    while answered - refused > 16 << 10 {
        let limit = (refused + answered) / 2;
        match distinct_answer(limit, Version::HTTP_11).await?.0 {
            StatusCode::OK => answered = limit,
            StatusCode::PAYLOAD_TOO_LARGE => refused = limit,
            other => panic!("{other} in {limit} bytes"),
        }
    }
    let (status, streamed) = distinct_answer(answered, Version::HTTP_11).await?;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = distinct_answer(answered, Version::HTTP_10).await?;
    let problem: serde_json::Value = serde_json::from_slice(&body)?;
    eprintln!(
        "{} bytes answered in {answered}: {status} {problem}",
        streamed.len()
    );
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let detail = problem["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("an HTTP/1.0 request can be answered with")
            && detail.contains("Ask over HTTP/1.1 or HTTP/2"),
        "{detail}"
    );
    Ok(())
}

/// A client that leaves while its HTTP/1.0 answer is being made stops the request: hyper
/// drops it once the connection reads the client's end, which cancels the query and
/// releases its envelope. Pins, for HTTP/1.0, what the hold's own exit relies on.
#[tokio::test]
async fn an_http10_client_that_leaves_stops_its_request() -> Result<(), Box<dyn Error>> {
    let endpoint = HeldEndpoint::bind().await?;
    let store = shared_store()?;
    store.set_service_client(endpoint.client()?);
    let app = test_app_with_store(
        store.clone(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let before = held(&store);
    let query = format!(
        "SELECT * WHERE {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        endpoint.url()
    );
    let request = format!(
        "POST /dataset/query HTTP/1.0\r\nHost: {addr}\r\n\
         Content-Type: application/sparql-query\r\nAccept: text/csv\r\n\
         Content-Length: {}\r\n\r\n{query}",
        query.len()
    );
    let mut client = TcpStream::connect(addr).await?;
    client.write_all(request.as_bytes()).await?;
    let mut answer = tokio::spawn(async move {
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.map(|_| response)
    });
    let call = endpoint.call(&mut answer).await?;
    // The client leaves: its connection closes with the task that holds it.
    answer.abort();
    call.closed().await?;
    released_to(&store, before).await;
    server.abort();
    Ok(())
}

fn admin_policy() -> PolicyConfig {
    PolicyConfig {
        auth: AuthConfig::BearerStatic(StaticBearerConfig {
            read_token: None,
            admin_token: "admin".to_owned(),
        }),
        ..PolicyConfig::default()
    }
}

/// `uri` asked over HTTP/1.1 and over HTTP/1.0: the same answer, over HTTP/1.0 with its
/// length.
async fn the_same_answer_whole(uri: &str) -> Result<(), Box<dyn Error>> {
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(minimal_fixture_path()),
        admin_policy(),
        ReasonerConfig::default(),
    )?;
    let get = |version| {
        Request::get(uri)
            .version(version)
            .header(header::AUTHORIZATION, "Bearer admin")
            .body(Body::empty())
    };
    let streamed = app.clone().oneshot(get(Version::HTTP_11)?).await?;
    let whole = app.oneshot(get(Version::HTTP_10)?).await?;
    assert_eq!(streamed.status(), StatusCode::OK, "{uri}");
    assert_eq!(whole.status(), StatusCode::OK, "{uri}");
    let length = whole.headers().get(header::CONTENT_LENGTH).cloned();
    let streamed = to_bytes(streamed.into_body(), usize::MAX).await?;
    let whole = to_bytes(whole.into_body(), usize::MAX).await?;
    assert!(!whole.is_empty(), "{uri}");
    assert_eq!(length, Some(whole.len().into()), "{uri}");
    assert_eq!(whole, streamed, "{uri}");
    Ok(())
}

#[tokio::test]
async fn an_http10_graph_store_read_is_sent_whole() -> Result<(), Box<dyn Error>> {
    the_same_answer_whole("/dataset/data?default").await
}

#[tokio::test]
async fn an_http10_rdf4j_statements_read_is_sent_whole() -> Result<(), Box<dyn Error>> {
    the_same_answer_whole("/repositories/nrese/statements").await
}

#[tokio::test]
async fn an_http10_export_is_sent_whole() -> Result<(), Box<dyn Error>> {
    the_same_answer_whole("/api/v1/repositories/nrese/backup").await
}
