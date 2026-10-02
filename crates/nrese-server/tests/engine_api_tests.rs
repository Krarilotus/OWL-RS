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
    let (status, _) = send(
        &app,
        Method::GET,
        "/api/v1/repositories/second/info",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &app,
        Method::PUT,
        "/repositories/second",
        Some("text/turtle"),
        "",
    )
    .await;
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
    assert!(
        text.contains("false") || text.contains("Violation"),
        "{text}"
    );
    for path in [
        "classification",
        "info",
        "summary",
        "reasoning",
        "service-description",
    ] {
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
    let (status, _) = send(
        &app,
        Method::PUT,
        "/repositories/second",
        Some("text/turtle"),
        "",
    )
    .await;
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
    let (status, _) = send(
        &app,
        Method::PUT,
        &format!("{base}/ex"),
        None,
        "http://example.com/",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, text) = send(&app, Method::GET, base, None, "").await;
    let map: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(map["ex"], "http://example.com/");
    assert_eq!(map["rdf"], "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
    // The same prefixes through RDF4J; the default repository's are its own.
    let (_, text) = send(
        &app,
        Method::GET,
        "/repositories/second/namespaces/ex",
        None,
        "",
    )
    .await;
    assert_eq!(text.trim(), "http://example.com/");
    let (_, text) = send(
        &app,
        Method::GET,
        "/api/v1/repositories/nrese/namespaces",
        None,
        "",
    )
    .await;
    assert!(!text.contains("example.com"), "{text}");
    let (status, _) = send(&app, Method::DELETE, &format!("{base}/ex"), None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, Method::DELETE, &format!("{base}/ex"), None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn repositories_are_created_and_removed_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app_with_store_config(
        StoreConfig::on_disk(dir.path()),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let base = "/api/v1/repositories/third";
    let json = Some("application/json");
    let settings = r#"{"title": "Third", "reasoning": "rdfs"}"#;
    let (status, text) = send(&app, Method::PUT, base, json, settings).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, text) = send(&app, Method::PUT, base, json, settings).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("problems/conflict"), "{text}");
    let (status, text) = send(&app, Method::GET, base, None, "").await;
    assert_eq!(status, StatusCode::OK);
    let view: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(view["title"], "Third");
    assert_eq!(view["reasoning"], "rdfs");
    assert_eq!(view["settings"]["reasoning"], "rdfs");
    // It reasons by its own settings.
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("{base}/update"),
        Some("application/sparql-update"),
        "PREFIX ex: <http://example.com/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { ex:C rdfs:subClassOf ex:D . ex:x a ex:C }",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, text) = send(
        &app,
        Method::GET,
        &format!(
            "{base}/query?query={}",
            urlencoding("ASK { <http://example.com/x> a <http://example.com/D> }")
        ),
        None,
        "",
    )
    .await;
    assert!(text.contains("true"), "{text}");
    // Settings that name no reasoning mode are refused.
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/v1/repositories/fourth",
        json,
        r#"{"reasoning": "owl-full"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(&app, Method::DELETE, base, None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, Method::GET, base, None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, Method::DELETE, "/api/v1/repositories/nrese", None, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn urlencoding(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn sessions_collect_writes_and_commit_them_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app_with_store_config(
        StoreConfig::on_disk(dir.path()),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let base = "/api/v1/repositories/nrese";
    let (status, text) = send(&app, Method::POST, &format!("{base}/sessions"), None, "").await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let opened: serde_json::Value = serde_json::from_str(&text).unwrap();
    let session = opened["path"].as_str().unwrap().to_owned();
    assert!(opened["idle_seconds"].as_u64().unwrap() > 0);
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("{session}/update"),
        Some("application/sparql-update"),
        "INSERT DATA { <http://example.com/a> <http://example.com/p> 1 }",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("{session}/data?graph=http%3A%2F%2Fexample.com%2Fg"),
        Some("text/turtle"),
        "<http://example.com/b> <http://example.com/p> 2 .",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let all = "SELECT (COUNT(*) AS ?n) WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }";
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{session}/query"),
        Some("application/sparql-query"),
        all,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(count_of(&text), 2, "the session sees its writes");
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        all,
    )
    .await;
    assert_eq!(count_of(&text), 0, "nothing committed yet");
    let (status, _) = send(&app, Method::POST, &format!("{session}/commit"), None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        all,
    )
    .await;
    assert_eq!(count_of(&text), 2, "committed as one");
    // A committed session is closed.
    let (status, _) = send(&app, Method::DELETE, &session, None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, Method::POST, &format!("{session}/commit"), None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn explanations_through_the_engine_api() {
    let app = support::test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Rdfs),
    )
    .unwrap();
    let (status, _) = send(
        &app,
        Method::POST,
        "/dataset/update",
        Some("application/sparql-update"),
        "PREFIX ex: <http://example.com/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { ex:a a ex:C . ex:C rdfs:subClassOf ex:D }",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let explain = |object: &str| {
        format!(
            "/api/v1/repositories/nrese/explain?subj=%3Chttp%3A%2F%2Fexample.com%2Fa%3E\
             &pred=%3Chttp%3A%2F%2Fwww.w3.org%2F1999%2F02%2F22-rdf-syntax-ns%23type%3E\
             &obj=%3Chttp%3A%2F%2Fexample.com%2F{object}%3E"
        )
    };
    let (status, text) = send(&app, Method::GET, &explain("D"), None, "").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    let steps = body["steps"].as_array().unwrap();
    assert_eq!(steps[0]["origin"], "inferred");
    assert_eq!(
        steps[0]["rule"], "rdfs9",
        "the RDFS name of the subclass rule"
    );
    assert_eq!(steps[0]["premises"].as_array().unwrap().len(), 2);
    let (status, _) = send(&app, Method::GET, &explain("X"), None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn imports_load_documents_and_reason_over_them() {
    let app = support::test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Rdfs),
    )
    .unwrap();
    let base = "/api/v1/repositories/nrese";
    let turtle = "@prefix ex: <http://example.com/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        ex:a a ex:C . ex:b a ex:C . ex:C rdfs:subClassOf ex:D .";
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/import"),
        Some("text/turtle"),
        turtle,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["inserted"], 3);
    assert!(
        report["reasoning"]["inferred"].as_u64().unwrap() >= 2,
        "{text}"
    );
    let ds = "SELECT (COUNT(*) AS ?n) WHERE { ?x a <http://example.com/D> }";
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        ds,
    )
    .await;
    assert_eq!(count_of(&text), 2, "the import was reasoned over");
    // Into a named graph; then replacing everything; a broken document with skip_errors.
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/import?graph=http%3A%2F%2Fexample.com%2Fg"),
        Some("application/n-triples"),
        "<http://example.com/x> <http://example.com/p> \"1\" .\n",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let named = "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <http://example.com/g> { ?s ?p ?o } }";
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        named,
    )
    .await;
    assert_eq!(count_of(&text), 1);
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/import?replace=true&skip_errors=true"),
        Some("application/n-triples"),
        "<http://example.com/y> <http://example.com/p> \"2\" .\nthis is not a statement\n",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["skipped"], 1, "{text}");
    let all = "SELECT (COUNT(*) AS ?n) WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } FILTER(?p != <http://www.w3.org/1999/02/22-rdf-syntax-ns#type>) }";
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        all,
    )
    .await;
    assert!(count_of(&text) >= 1);
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        ds,
    )
    .await;
    assert_eq!(
        count_of(&text),
        0,
        "replaced: the old statements and their inferences are gone"
    );
    // Without skipping, a broken document is refused.
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("{base}/import"),
        Some("application/n-triples"),
        "not rdf\n",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Rematerialising on demand.
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/reasoning/rematerialise"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["ruleset"],
        "rdfs"
    );
}
