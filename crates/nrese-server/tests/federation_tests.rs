//! SERVICE over HTTP: one server asks another, through the SPARQL protocol, only at the
//! endpoints `federation.allow` names.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::federation::HttpServiceClient;
use nrese_server::policy::PolicyConfig;
use nrese_store::{FederationConfig, StoreConfig, StoreService};
use tower::util::ServiceExt;

use support::{body_text, test_app_with_store};

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
