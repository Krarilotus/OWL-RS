//! The engine API (ADR-0007): every capability of the default repository's `/dataset/…`
//! routes reaches every repository under `/api/v1/repositories/{id}/…`, by the same
//! handlers, and each repository keeps its own data.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store_config};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;

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
    let app = crate::support::test_app_with_settings(
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

    // Justifications: the two asserted statements, as one set, checked.
    let justify = |mode: &str| format!("{}&justifications={mode}", explain("D"));
    let (status, text) = send(&app, Method::GET, &justify("all"), None, "").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["mode"], "all");
    assert_eq!(body["complete"], true);
    assert_eq!(body["verified"], true);
    let sets = body["justifications"].as_array().unwrap();
    assert_eq!(sets.len(), 1, "{text}");
    assert_eq!(sets[0].as_array().unwrap().len(), 2, "{text}");
    assert!(
        sets[0]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["origin"] == "asserted")
    );
    let (status, text) = send(&app, Method::GET, &justify("core"), None, "").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["statements"].as_array().unwrap().len(), 2, "{text}");
    assert!(body.get("verified").is_none());
    let (status, text) = send(
        &app,
        Method::GET,
        &format!("{}&k=2", justify("top-k")),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("{}&k=0", justify("top-k")),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(&app, Method::GET, &justify("some"), None, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn imports_load_documents_and_reason_over_them() {
    let app = crate::support::test_app_with_settings(
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

#[tokio::test]
async fn graphs_are_listed_with_their_sizes() {
    let app = test_app_with_store_config(
        StoreConfig::in_memory(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/repositories/nrese/update",
        Some("application/sparql-update"),
        "INSERT DATA { <urn:a> <urn:p> 1 . GRAPH <urn:g:2> { <urn:a> <urn:p> 1 , 2 } \
         GRAPH <urn:g:1> { <urn:b> <urn:p> 3 } }",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, text) = send(
        &app,
        Method::GET,
        "/api/v1/repositories/nrese/graphs",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let graphs: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        graphs,
        serde_json::json!([
            { "statements": 1 },
            { "graph": "urn:g:1", "statements": 1 },
            { "graph": "urn:g:2", "statements": 2 },
        ])
    );
}

/// A query in flight is listed with its text; cancelling it stops it, and its client gets
/// an error instead of results.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_queries_are_listed_and_cancelled() {
    let app = test_app_with_store_config(
        StoreConfig::in_memory(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let data: String = (0..300)
        .map(|i| format!("<urn:s{i}> <urn:p> {i} . "))
        .collect();
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/repositories/nrese/update",
        Some("application/sparql-update"),
        &format!("INSERT DATA {{ {data} }}"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 300³ rows, each compared: minutes, unless cancelled.
    let slow = "SELECT (COUNT(*) AS ?n) WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y . ?c <urn:p> ?z \
                FILTER(?x + ?y != ?z * 1000) }";
    let running = {
        let app = app.clone();
        tokio::spawn(async move {
            send(
                &app,
                Method::POST,
                "/api/v1/repositories/nrese/query",
                Some("application/sparql-query"),
                slow,
            )
            .await
        })
    };
    let mut id = None;
    for _ in 0..200 {
        let (_, text) = send(
            &app,
            Method::GET,
            "/api/v1/repositories/nrese/queries",
            None,
            "",
        )
        .await;
        let list: serde_json::Value = serde_json::from_str(&text).unwrap();
        if let Some(query) = list.as_array().unwrap().first() {
            assert!(query["query"].as_str().unwrap().contains("COUNT(*)"));
            assert_eq!(query["origin"], "roles: anonymous");
            id = query["id"].as_u64();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let id = id.expect("the query was listed while it ran");
    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/repositories/nrese/queries/{id}"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = tokio::time::timeout(std::time::Duration::from_secs(30), running)
        .await
        .expect("the cancelled query stops")
        .unwrap();
    assert!(!status.is_success(), "{status}");
    let (_, text) = send(
        &app,
        Method::GET,
        "/api/v1/repositories/nrese/queries",
        None,
        "",
    )
    .await;
    assert_eq!(text.trim(), "[]");
    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/repositories/nrese/queries/{id}"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A repository's reasoning changes at once through `PATCH`: the inferences are
/// recomputed under the new rules, writes go on, and the default repository's change is
/// kept in the data directory.
#[tokio::test]
async fn repository_settings_change_at_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app_with_store_config(
        StoreConfig::on_disk(dir.path()),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let json = Some("application/json");
    for base in ["/api/v1/repositories/r2", "/api/v1/repositories/nrese"] {
        if base.ends_with("r2") {
            let (status, text) = send(
                &app,
                Method::PUT,
                base,
                json,
                r#"{"reasoning": "disabled"}"#,
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{text}");
        } else {
            let (status, text) = send(
                &app,
                Method::PATCH,
                base,
                json,
                r#"{"reasoning": "disabled"}"#,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{text}");
        }
        let (status, _) = send(
            &app,
            Method::POST,
            &format!("{base}/update"),
            Some("application/sparql-update"),
            "INSERT DATA { <urn:C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:D> . \
             <urn:x> a <urn:C> }",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let ask = format!(
            "{base}/query?query={}",
            urlencoding("ASK { <urn:x> a <urn:D> }")
        );
        let (_, text) = send(&app, Method::GET, &ask, None, "").await;
        assert!(text.contains("false"), "{base}: {text}");
        let (status, text) = send(
            &app,
            Method::PATCH,
            base,
            json,
            r#"{"reasoning": "rdfs", "title": "With RDFS"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let view: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(view["reasoning"], "rdfs", "{text}");
        assert_eq!(view["title"], "With RDFS");
        let (_, text) = send(&app, Method::GET, &ask, None, "").await;
        assert!(text.contains("true"), "{base}: {text}");
        // Writes go on under the new rules.
        let (status, _) = send(
            &app,
            Method::POST,
            &format!("{base}/update"),
            Some("application/sparql-update"),
            "INSERT DATA { <urn:y> a <urn:C> }",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, text) = send(
            &app,
            Method::GET,
            &format!(
                "{base}/query?query={}",
                urlencoding("ASK { <urn:y> a <urn:D> }")
            ),
            None,
            "",
        )
        .await;
        assert!(text.contains("true"), "{base}: {text}");
        let (status, _) = send(&app, Method::PATCH, base, json, r#"{"reasoning": "bogus"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    // The default repository's settings are kept in the data directory.
    let stored = std::fs::read_to_string(dir.path().join("repository.json")).unwrap();
    assert!(stored.contains("rdfs"), "{stored}");
}

/// Waits for job `path` to end; its last state.
async fn finished(app: &Router, path: &str) -> serde_json::Value {
    for _ in 0..500 {
        let (status, text) = send(app, Method::GET, path, None, "").await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let job: serde_json::Value = serde_json::from_str(&text).unwrap();
        if job["state"] != "running" {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the job didn't end");
}

/// Imports run as jobs: an uploaded document with `async=true`, and files from the
/// server's import directory (administrators), whose paths can't leave it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imports_run_as_jobs() {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("a.ttl"), "<urn:a> <urn:p> 1 .").unwrap();
    std::fs::create_dir(files.path().join("sub")).unwrap();
    std::fs::write(
        files.path().join("sub").join("b.nt"),
        "<urn:b> <urn:p> \"2\" .\n",
    )
    .unwrap();
    let app = test_app_with_store_config(
        StoreConfig::in_memory(),
        PolicyConfig {
            import_directory: Some(files.path().to_path_buf()),
            ..PolicyConfig::default()
        },
        ReasonerConfig::default(),
    )
    .unwrap();
    let base = "/api/v1/repositories/nrese";
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/import?async=true&graph=urn:g"),
        Some("text/turtle"),
        "<urn:c> <urn:p> 3 .",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let started: serde_json::Value = serde_json::from_str(&text).unwrap();
    let job = finished(&app, started["path"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["report"]["inserted"], 1);
    assert_eq!(job["files_done"], 1);

    let (status, text) = send(&app, Method::GET, &format!("{base}/import/files"), None, "").await;
    assert_eq!(status, StatusCode::OK);
    let listed: serde_json::Value = serde_json::from_str(&text).unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a.ttl", "sub/b.nt"]);
    let import = |files: &str| format!(r#"{{"files": {files}}}"#);
    let json = Some("application/json");
    let (status, text) = send(
        &app,
        Method::POST,
        &format!("{base}/import/files"),
        json,
        &import(r#"["a.ttl", "sub/b.nt"]"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let started: serde_json::Value = serde_json::from_str(&text).unwrap();
    let job = finished(&app, started["path"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["report"]["inserted"], 2);
    assert_eq!(job["description"], "a.ttl, sub/b.nt");
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        COUNT,
    )
    .await;
    assert_eq!(count_of(&text), 2, "the default graph: a and b");
    for (files, expected) in [
        (r#"["../a.ttl"]"#, StatusCode::BAD_REQUEST),
        (r#"["/etc/passwd"]"#, StatusCode::BAD_REQUEST),
        (r#"["missing.ttl"]"#, StatusCode::NOT_FOUND),
        (r#"[]"#, StatusCode::BAD_REQUEST),
    ] {
        let (status, _) = send(
            &app,
            Method::POST,
            &format!("{base}/import/files"),
            json,
            &import(files),
        )
        .await;
        assert_eq!(status, expected, "{files}");
    }
    let (status, text) = send(&app, Method::GET, "/api/v1/jobs", None, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (status, _) = send(&app, Method::DELETE, "/api/v1/jobs/1", None, "").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a finished job isn't cancelled"
    );
    // Without an import directory, server-side imports don't exist.
    let plain = test_app_with_store_config(
        StoreConfig::in_memory(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let (status, _) = send(
        &plain,
        Method::GET,
        &format!("{base}/import/files"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// User rules are a managed object: uploaded as a file, compiled before they are stored
/// (a mistake is refused with its line), in effect at once, and removed again.
#[tokio::test]
async fn rules_are_uploaded_checked_and_removed() {
    let app = test_app_with_store_config(
        StoreConfig::in_memory(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let base = "/api/v1/repositories/nrese";
    let n3 = Some("text/n3");
    let (status, text) = send(
        &app,
        Method::PUT,
        &format!("{base}/rules?name=broken.n3"),
        n3,
        "@prefix : <http://e/> .\n\n{ ?x :parent ?y } => { ?y :child ?x  .\n",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("line 4, column 1"), "{text}");
    let (status, _) = send(&app, Method::GET, &format!("{base}/rules"), None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nothing stored");

    let rules = "@prefix : <http://e/> .\n{ ?x :parent ?y } => { ?y :child ?x } .\n";
    let (status, text) = send(
        &app,
        Method::PUT,
        &format!("{base}/rules?name=family.n3"),
        n3,
        rules,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, text) = send(&app, Method::GET, &format!("{base}/rules"), None, "").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let stored: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(stored["name"], "family.n3");
    assert_eq!(stored["format"], "n3");
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("{base}/update"),
        Some("application/sparql-update"),
        "INSERT DATA { <http://e/ann> <http://e/parent> <http://e/bob> }",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let ask = "ASK { <http://e/bob> <http://e/child> <http://e/ann> }";
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        ask,
    )
    .await;
    assert!(text.contains("true"), "the rule derives: {text}");

    let (status, _) = send(&app, Method::DELETE, &format!("{base}/rules"), None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, Method::GET, &format!("{base}/rules"), None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, text) = send(
        &app,
        Method::POST,
        &format!("{base}/query"),
        Some("application/sparql-query"),
        ask,
    )
    .await;
    assert!(text.contains("false"), "the inference is gone: {text}");
}
