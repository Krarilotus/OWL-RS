//! The engine API (ADR-0007): every capability of the default repository's `/dataset/…`
//! routes reaches every repository under `/api/v1/repositories/{id}/…`, by the same
//! handlers, and each repository keeps its own data.

mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::util::ServiceExt;

use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;
use support::{body_text, test_app_with_store_config};

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::ACCEPT, "application/sparql-results+json, */*;q=0.1");
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (status, body_text(response).await.unwrap())
}

const COUNT: &str = "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }";

fn count_of(json: &str) -> u64 {
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    value["results"]["bindings"][0]["n"]["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn every_capability_reaches_every_repository() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app_with_store_config(
        StoreConfig::on_disk(dir.path()),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    // Unknown repositories are absent on every route.
    let (status, _) = send(&app, Method::GET, "/api/v1/repositories/second/info", None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, Method::PUT, "/repositories/second", Some("text/turtle"), "").await;
    assert!(status.is_success(), "{status}");

    let base = "/api/v1/repositories/second";
    let update = "PREFIX ex: <http://example.com/>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        INSERT DATA { ex:a rdfs:label \"Windmill of Sanssouci\" . ex:a a ex:Mill .
                      GRAPH ex:g { ex:b ex:p ex:c } }";
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/update"),
        Some("application/sparql-update"),
        update,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    // The query endpoint and the combined endpoint of that repository see it; the default
    // repository doesn't.
    for path in ["query", "sparql"] {
        let (status, text) = send(
            &app,
            Method::POST,
            &format!("{base}/{path}"),
            Some("application/sparql-query"),
            COUNT,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {text}");
        assert_eq!(count_of(&text), 2, "{path}");
    }
    let (_, text) = send(
        &app,
        Method::POST,
        "/dataset/query",
        Some("application/sparql-query"),
        COUNT,
    )
    .await;
    assert_eq!(count_of(&text), 0, "the default repository stays empty");
    // The default repository is reachable by its id too.
    let (status, text) = send(
        &app,
        Method::POST,
        "/api/v1/repositories/nrese/query",
        Some("application/sparql-query"),
        COUNT,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count_of(&text), 0);

    // Graph Store: the named graph, then a new one.
    let (status, text) = send(
        &app,
        Method::GET,
        &format!("{base}/data?graph=http%3A%2F%2Fexample.com%2Fg"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("http://example.com/b"), "{text}");
    let (status, _) = send(
        &app,
        Method::PUT,
        &format!("{base}/data?graph=http%3A%2F%2Fexample.com%2Fh"),
        Some("text/turtle"),
        "<http://example.com/x> <http://example.com/p> 1 .",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Search, SHACL, classification, information and the service description.
    let (status, text) = send(
        &app,
        Method::GET,
        &format!("{base}/autocomplete?q=windm"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("http://example.com/a"), "{text}");
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://example.com/> .
        ex:S a sh:NodeShape ; sh:targetClass ex:Mill ;
             sh:property [ sh:path ex:built ; sh:minCount 1 ] .";
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/shacl"),
        Some("text/turtle"),
        shapes,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("false") || text.contains("Violation"), "{text}");
    for path in ["classification", "info", "summary", "reasoning", "service-description"] {
        let (status, text) = send(&app, Method::GET, &format!("{base}/{path}"), None, "").await;
        assert_eq!(status, StatusCode::OK, "{path}: {text}");
    }
    // A backup of that repository holds its statements.
    let (status, text) = send(&app, Method::GET, &format!("{base}/backup"), None, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("Windmill of Sanssouci"), "{text}");
}

#[tokio::test]
async fn repositories_and_namespaces_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app_with_store_config(
        StoreConfig::on_disk(dir.path()),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let (status, _) = send(&app, Method::PUT, "/repositories/second", Some("text/turtle"), "").await;
    assert!(status.is_success());
    let (status, text) = send(&app, Method::GET, "/api/v1/repositories", None, "").await;
    assert_eq!(status, StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&text).unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["nrese", "second"]);
    let base = "/api/v1/repositories/second/namespaces";
    let (status, _) = send(&app, Method::PUT, &format!("{base}/ex"), None, "http://example.com/").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, text) = send(&app, Method::GET, base, None, "").await;
    let map: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(map["ex"], "http://example.com/");
    assert_eq!(map["rdf"], "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
    // The same prefixes through RDF4J; the default repository's are its own.
    let (_, text) = send(&app, Method::GET, "/repositories/second/namespaces/ex", None, "").await;
    assert_eq!(text.trim(), "http://example.com/");
    let (_, text) = send(&app, Method::GET, "/api/v1/repositories/nrese/namespaces", None, "").await;
    assert!(!text.contains("example.com"), "{text}");
    let (status, _) = send(&app, Method::DELETE, &format!("{base}/ex"), None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, Method::DELETE, &format!("{base}/ex"), None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
