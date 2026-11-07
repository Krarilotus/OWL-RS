//! SERVICE over HTTP: one server asks another, through the SPARQL protocol, only at the
//! endpoints `federation.allow` names.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::build_app;
use nrese_server::federation::HttpServiceClient;
use nrese_server::policy::PolicyConfig;
use nrese_store::{FederationConfig, StoreConfig, StoreService};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store, test_state_with_store_config};

const EX: &str = "http://example.com/";

fn store_with(
    triples: &str,
) -> Result<(StoreService, tempfile::TempDir), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("data.nt");
    std::fs::write(&path, triples)?;
    Ok((
        StoreService::new(StoreConfig::in_memory().with_ontology(path))?,
        dir,
    ))
}

fn query(text: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::builder()
        .uri("/dataset/query")
        .method(Method::POST)
        .header("content-type", "application/sparql-query")
        .header("accept", "text/tab-separated-values")
        .body(Body::from(text.to_owned()))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_asks_another() -> Result<(), Box<dyn std::error::Error>> {
    // The remote server: cities and their countries, on a real port.
    let remote_triples: String = (0..300)
        .map(|c| format!("<{EX}city{c}> <{EX}country> <{EX}country{}> .\n", c % 3))
        .collect();
    let (remote, _remote_dir) = store_with(&remote_triples)?;
    let remote_app =
        test_app_with_store(remote, PolicyConfig::default(), ReasonerConfig::default())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/dataset/query", listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, remote_app)
            .await
            .expect("remote server");
    });

    // The local server: people and their cities; SERVICE may call the remote one only.
    let local_triples: String = (0..500)
        .map(|i| format!("<{EX}person{i}> <{EX}livesIn> <{EX}city{}> .\n", i % 400))
        .collect();
    let (local, _local_dir) = store_with(&local_triples)?;
    let federation = FederationConfig {
        allow: vec![endpoint.clone()],
        ..FederationConfig::default()
    };
    local.set_service_client(Arc::new(HttpServiceClient::new(
        federation,
        tokio::runtime::Handle::current(),
    )?));
    let app = test_app_with_store(local, PolicyConfig::default(), ReasonerConfig::default())?;

    // People per country: the 300 cities the remote knows, 400 of the 500 people.
    let text = format!(
        "SELECT ?country (COUNT(?person) AS ?n) WHERE {{ ?person <{EX}livesIn> ?city . SERVICE <{endpoint}> {{ ?city <{EX}country> ?country }} }} GROUP BY ?country ORDER BY ?country"
    );
    let response = app.clone().oneshot(query(&text)?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await?;
    let counts: Vec<&str> = body.lines().skip(1).collect();
    assert_eq!(counts.len(), 3, "{body}");
    let total: u32 = counts
        .iter()
        .map(|line| line.split('\t').nth(1).unwrap().parse::<u32>().unwrap())
        .sum();
    assert_eq!(total, 400, "{body}");

    // An endpoint outside the allow list is refused, unless SILENT.
    let elsewhere =
        "SELECT * WHERE { SERVICE <http://127.0.0.1:9/sparql> { ?s ?p ?o } }".to_owned();
    let response = app.clone().oneshot(query(&elsewhere)?).await?;
    assert!(response.status().is_client_error() || response.status().is_server_error());
    let text = body_text(response).await?;
    assert!(text.contains("federation.allow"), "{text}");
    let silent = elsewhere.replace("SERVICE", "SERVICE SILENT");
    let response = app.oneshot(query(&silent)?).await?;
    assert_eq!(response.status(), StatusCode::OK);

    server.abort();
    Ok(())
}

/// An endpoint that answers every request with `response`, on every connection and for
/// every request a kept-alive connection brings, and keeps each connection open until its
/// client closes it, whatever the response's own length says.
fn canned(response: Vec<u8>) -> std::io::Result<String> {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/sparql", listener.local_addr()?);
    let response = std::sync::Arc::new(response);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let response = std::sync::Arc::clone(&response);
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    // A request's head, then as much of its body as its length says.
                    let head = loop {
                        if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buffer[..n]),
                        }
                    };
                    let text = String::from_utf8_lossy(&request[..head]).to_ascii_lowercase();
                    let length: usize = text
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    while request.len() < head + length {
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buffer[..n]),
                        }
                    }
                    request.drain(..head + length);
                    if stream.write_all(&response).is_err() {
                        return;
                    }
                }
            });
        }
    });
    Ok(url)
}

/// A server whose SERVICE may call `endpoint`, with `memory` bytes per query (0: unlimited).
fn federated_app(
    endpoint: &str,
    memory: usize,
) -> Result<axum::Router, Box<dyn std::error::Error>> {
    let local = StoreService::new(StoreConfig::in_memory())?;
    local.set_service_client(Arc::new(HttpServiceClient::new(
        FederationConfig {
            allow: vec![endpoint.to_owned()],
            timeout_ms: 3_600_000,
            ..FederationConfig::default()
        },
        tokio::runtime::Handle::current(),
    )?));
    let policy = PolicyConfig {
        limits: nrese_server::policy::RequestLimits {
            max_query_memory_bytes: memory,
            ..PolicyConfig::default().limits
        },
        ..PolicyConfig::default()
    };
    test_app_with_store(local, policy, ReasonerConfig::default())
}

/// An endpoint's error is its status: the call fails with it at once, reading no more of
/// the body than the message shows, however long the body says it is (here it never ends).
#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_error_is_read_no_further_than_its_message()
-> Result<(), Box<dyn std::error::Error>> {
    let mut response = b"HTTP/1.1 500 Internal Server Error\r\ncontent-type: text/plain\r\ncontent-length: 100000000\r\n\r\n".to_vec();
    response.extend(std::iter::repeat_n(b'x', 600));
    let endpoint = canned(response)?;
    let app = federated_app(&endpoint, 0)?;
    let text = format!("SELECT * WHERE {{ SERVICE <{endpoint}> {{ ?s ?p ?o }} }}");
    let response = tokio::time::timeout(
        crate::support::held_service::HANG,
        app.oneshot(query(&text)?),
    )
    .await
    .map_err(|_| "the SERVICE call waited for the rest of the error's body")??;
    assert!(!response.status().is_success(), "{}", response.status());
    let body = body_text(response).await?;
    assert!(body.contains("500 Internal Server Error"), "{body}");
    Ok(())
}

/// An endpoint's answer is memory of the query that asked for it: one larger than the
/// query's budget is refused (413), SILENT or not, instead of being read whole. Large in
/// its body, or only once its rows are laid out over its variables.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_beyond_the_query_budget_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    // For a query budget of 1 MB: one row, a literal of 2 MB; and a body of 109 KB, its
    // bindings (10,000, empty) before its head (10,000 variables), so 100 million values.
    let value = "a".repeat(2_000_000);
    let large = format!(
        r#"{{"head":{{"vars":["o"]}},"results":{{"bindings":[{{"o":{{"type":"literal","value":"{value}"}}}}]}}}}"#
    );
    let vars: Vec<String> = (0..10_000).map(|i| format!("\"v{i}\"")).collect();
    let wide = format!(
        r#"{{"results":{{"bindings":[{}]}},"head":{{"vars":[{}]}}}}"#,
        vec!["{}"; 10_000].join(","),
        vars.join(",")
    );
    for json in [large, wide] {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/sparql-results+json\r\ncontent-length: {}\r\n\r\n",
            json.len()
        )
        .into_bytes();
        response.extend_from_slice(json.as_bytes());
        let endpoint = canned(response)?;
        let app = federated_app(&endpoint, 1_000_000)?;
        for service in ["SERVICE", "SERVICE SILENT"] {
            let text = format!("SELECT ?o WHERE {{ {service} <{endpoint}> {{ ?s ?p ?o }} }}");
            let response = app.clone().oneshot(query(&text)?).await?;
            assert_eq!(
                response.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "{service}, {} bytes",
                json.len()
            );
        }
    }
    Ok(())
}

/// A repository created at run time calls the endpoints the server's SERVICE client may
/// call, as the default repository does.
#[tokio::test(flavor = "multi_thread")]
async fn a_created_repository_calls_the_servers_endpoints() -> Result<(), Box<dyn std::error::Error>>
{
    let json = r#"{"head":{"vars":["o"]},"results":{"bindings":[{"o":{"type":"literal","value":"there"}}]}}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/sparql-results+json\r\ncontent-length: {}\r\n\r\n{json}",
        json.len()
    );
    let endpoint = canned(response.into_bytes())?;
    let dir = tempfile::tempdir()?;
    let state = test_state_with_store_config(
        StoreConfig::on_disk(dir.path()),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    // The server installs its client on the default repository's store (main.rs).
    state
        .store()
        .set_service_client(Arc::new(HttpServiceClient::new(
            FederationConfig {
                allow: vec![endpoint.clone()],
                ..FederationConfig::default()
            },
            tokio::runtime::Handle::current(),
        )?));
    let app = build_app(state);
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/v1/repositories/other")
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let text = format!("SELECT ?o WHERE {{ SERVICE <{endpoint}> {{ ?s ?p ?o }} }}");
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/repositories/other/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .header("accept", "text/tab-separated-values")
                .body(Body::from(text))?,
        )
        .await?;
    let status = response.status();
    let body = body_text(response).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"there\""), "{body}");
    Ok(())
}
