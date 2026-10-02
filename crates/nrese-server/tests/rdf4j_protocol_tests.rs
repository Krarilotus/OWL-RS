//! The RDF4J REST protocol (`http/rdf4j.rs`): what RDF4J's `HTTPRepository` and GraphDB
//! clients send, end to end through the app.

mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::util::ServiceExt;

use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;
use support::{body_text, test_app, test_app_with_store_config};

const REPO: &str = "/repositories/nrese";

fn encode(pairs: &[(&str, &str)]) -> String {
    serde_urlencoded::to_string(pairs).unwrap()
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    accept: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    if let Some(accept) = accept {
        request = request.header(header::ACCEPT, accept);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (status, body_text(response).await.unwrap())
}

async fn size(app: &Router, context: Option<&str>) -> u64 {
    let uri = match context {
        Some(context) => format!("{REPO}/size?{}", encode(&[("context", context)])),
        None => format!("{REPO}/size"),
    };
    let (status, text) = send(app, Method::GET, &uri, None, None, "").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    text.trim().parse().unwrap()
}

const DATA: &str = "@prefix ex: <http://example.com/> .\n\
                    ex:a ex:p \"one\" ; ex:q ex:b .\n\
                    ex:b ex:p \"two\"@en .\n";

#[tokio::test]
async fn statements_size_contexts_and_queries() {
    let app = test_app().unwrap();
    let (status, version) = send(&app, Method::GET, "/protocol", None, None, "").await;
    assert_eq!((status, version.as_str()), (StatusCode::OK, "12"));
    let (status, list) = send(
        &app,
        Method::GET,
        "/repositories",
        None,
        Some("application/sparql-results+json"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(list.contains("\"nrese\""), "{list}");

    // Turtle into a named graph, then into the default graph.
    let graph = "<http://example.com/g>";
    let uri = format!("{REPO}/statements?{}", encode(&[("context", graph)]));
    let (status, text) = send(&app, Method::POST, &uri, Some("text/turtle"), None, DATA).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    let uri = format!("{REPO}/statements");
    let (status, _) = send(&app, Method::POST, &uri, Some("text/turtle"), None, DATA).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(size(&app, None).await, 6);
    assert_eq!(size(&app, Some(graph)).await, 3);
    assert_eq!(size(&app, Some("null")).await, 3);

    // Statements by pattern, with their graphs in N-Quads.
    let uri = format!(
        "{REPO}/statements?{}",
        encode(&[("pred", "<http://example.com/p>"), ("context", graph)])
    );
    let (status, quads) = send(
        &app,
        Method::GET,
        &uri,
        None,
        Some("application/n-quads"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(quads.lines().count(), 2, "{quads}");
    assert!(quads.lines().all(|line| line.contains(graph)), "{quads}");
    let uri = format!("{REPO}/statements?{}", encode(&[("obj", "\"two\"@en")]));
    let (_, triples) = send(
        &app,
        Method::GET,
        &uri,
        None,
        Some("application/n-triples"),
        "",
    )
    .await;
    assert_eq!(triples.lines().count(), 2, "{triples}");

    let (_, contexts) = send(
        &app,
        Method::GET,
        &format!("{REPO}/contexts"),
        None,
        Some("application/sparql-results+json"),
        "",
    )
    .await;
    assert!(contexts.contains("http://example.com/g"), "{contexts}");

    // A query at the repository, by form; an update at the statements.
    let (status, results) = send(
        &app,
        Method::POST,
        REPO,
        Some("application/x-www-form-urlencoded"),
        Some("application/sparql-results+json"),
        &encode(&[("query", "SELECT ?o WHERE { <http://example.com/b> ?p ?o }")]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(results.contains("\"two\""), "{results}");
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("{REPO}/statements"),
        Some("application/x-www-form-urlencoded"),
        None,
        &encode(&[(
            "update",
            "INSERT DATA { <http://example.com/c> <http://example.com/p> 3 }",
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(size(&app, Some("null")).await, 4);

    // Removal by pattern; replacement of a graph.
    let uri = format!(
        "{REPO}/statements?{}",
        encode(&[("subj", "<http://example.com/a>"), ("context", "null")])
    );
    let (status, _) = send(&app, Method::DELETE, &uri, None, None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(size(&app, Some("null")).await, 2);
    assert_eq!(size(&app, Some(graph)).await, 3);
    let uri = format!("{REPO}/statements?{}", encode(&[("context", graph)]));
    let (status, _) = send(
        &app,
        Method::PUT,
        &uri,
        Some("application/n-triples"),
        None,
        "<http://example.com/x> <http://example.com/p> \"only\" .\n",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(size(&app, Some(graph)).await, 1);
    assert_eq!(size(&app, None).await, 3);
}

#[tokio::test]
async fn transactions_commit_together_or_not_at_all() {
    let app = test_app().unwrap();
    let begin = |app: Router| async move {
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("{REPO}/transactions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned()
    };
    // Added, then partly deleted and updated in the same transaction: nothing visible
    // before the commit, all of it after.
    let tx = begin(app.clone()).await;
    let action = |name: &str| format!("{tx}?action={name}");
    let (status, _) = send(
        &app,
        Method::PUT,
        &action("ADD"),
        Some("text/turtle"),
        None,
        DATA,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        Method::PUT,
        &action("DELETE"),
        Some("application/n-triples"),
        None,
        "<http://example.com/a> <http://example.com/p> \"one\" .\n",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        Method::PUT,
        &action("UPDATE"),
        Some("application/x-www-form-urlencoded"),
        None,
        &encode(&[(
            "update",
            "INSERT DATA { <http://example.com/d> <http://example.com/p> 4 }",
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(size(&app, None).await, 0);
    // Reads inside the transaction see its changes; outside, nothing yet.
    let (status, inside) = send(&app, Method::PUT, &action("SIZE"), None, None, "").await;
    assert_eq!((status, inside.trim()), (StatusCode::OK, "3"));
    let (status, results) = send(
        &app,
        Method::PUT,
        &format!(
            "{}&{}",
            action("QUERY"),
            encode(&[("query", "ASK { <http://example.com/d> ?p 4 }")])
        ),
        None,
        Some("application/sparql-results+json"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(results.contains("true"), "{results}");
    let (status, quads) = send(
        &app,
        Method::PUT,
        &format!(
            "{}&{}",
            action("GET"),
            encode(&[("subj", "<http://example.com/a>")])
        ),
        None,
        Some("application/n-triples"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(quads.lines().count(), 1, "{quads}");
    let (status, _) = send(&app, Method::PUT, &action("COMMIT"), None, None, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(size(&app, None).await, 3);
    // Committed transactions are gone.
    let (status, _) = send(&app, Method::PUT, &action("COMMIT"), None, None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A rolled back transaction leaves nothing.
    let tx = begin(app.clone()).await;
    let (status, _) = send(
        &app,
        Method::PUT,
        &format!("{tx}?action=ADD"),
        Some("text/turtle"),
        None,
        DATA,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, Method::DELETE, &tx, None, None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(size(&app, None).await, 3);
}

#[tokio::test]
async fn namespaces_are_listed_set_and_removed() {
    let app = test_app().unwrap();
    let uri = format!("{REPO}/namespaces/ex");
    let (status, _) = send(
        &app,
        Method::PUT,
        &uri,
        Some("text/plain"),
        None,
        "http://example.com/",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, iri) = send(&app, Method::GET, &uri, None, None, "").await;
    assert_eq!(
        (status, iri.as_str()),
        (StatusCode::OK, "http://example.com/")
    );
    let (_, list) = send(
        &app,
        Method::GET,
        &format!("{REPO}/namespaces"),
        None,
        Some("application/sparql-results+xml"),
        "",
    )
    .await;
    assert!(list.contains("<literal>ex</literal>"), "{list}");
    assert!(list.contains("<literal>rdfs</literal>"), "{list}");
    let (status, _) = send(&app, Method::DELETE, &uri, None, None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, Method::GET, &uri, None, None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Autocompletion: resources by the beginning of their labels' words or local names.
#[tokio::test]
async fn autocomplete_finds_labels_and_local_names() {
    let app = test_app().unwrap();
    let data = "@prefix ex: <http://example.com/> .\n\
                @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                @prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
                ex:q1 rdfs:label \"Albert Einstein\"@en .\n\
                ex:q2 rdfs:label \"Albertina\" .\n\
                ex:hasPart a owl:ObjectProperty .\n\
                ex:q3 ex:note \"Albert Schweitzer\" .\n";
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("{REPO}/statements"),
        Some("text/turtle"),
        None,
        data,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let suggest = |q: &str| {
        let app = app.clone();
        let uri = format!("/dataset/autocomplete?{}", encode(&[("q", q)]));
        async move {
            let (status, body) = send(&app, Method::GET, &uri, None, None, "").await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            json["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| {
                    s["iri"]
                        .as_str()
                        .unwrap()
                        .trim_start_matches("http://example.com/")
                        .to_owned()
                })
                .collect::<Vec<_>>()
        }
    };
    // Every word begins a word; a note is not a label.
    assert_eq!(suggest("alb ein").await, ["q1"]);
    let mut albert = suggest("alb").await;
    albert.sort();
    assert_eq!(albert, ["q1", "q2"]);
    // Local names, split at camel case.
    assert_eq!(suggest("part").await, ["hasPart"]);
    assert_eq!(suggest("haspa").await, ["hasPart"]);
    let (status, _) = send(&app, Method::GET, "/dataset/autocomplete", None, None, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// An on-disk store keeps its namespaces across a restart.
#[tokio::test]
async fn namespaces_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app = || {
        test_app_with_store_config(
            StoreConfig::on_disk(dir.path()),
            PolicyConfig::default(),
            ReasonerConfig::default(),
        )
        .unwrap()
    };
    let uri = format!("{REPO}/namespaces/ex");
    {
        let app = app();
        let (status, _) = send(
            &app,
            Method::PUT,
            &uri,
            Some("text/plain"),
            None,
            "http://example.com/",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = send(
            &app,
            Method::DELETE,
            &format!("{REPO}/namespaces/owl"),
            None,
            None,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let app = app();
    let (status, iri) = send(&app, Method::GET, &uri, None, None, "").await;
    assert_eq!(
        (status, iri.as_str()),
        (StatusCode::OK, "http://example.com/")
    );
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("{REPO}/namespaces/owl"),
        None,
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Several repositories: created and removed through the protocol, each its own dataset,
/// kept on disk across a restart; an unknown repository is 404.
#[tokio::test]
async fn repositories_are_created_used_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let app = || {
        test_app_with_store_config(
            StoreConfig::on_disk(dir.path()),
            PolicyConfig::default(),
            ReasonerConfig::default(),
        )
        .unwrap()
    };
    let size_of = |app: Router, repository: &'static str| async move {
        let (status, text) = send(
            &app,
            Method::GET,
            &format!("/repositories/{repository}/size"),
            None,
            None,
            "",
        )
        .await;
        (status, text.trim().to_owned())
    };
    {
        let app = app();
        let (status, _) = send(
            &app,
            Method::GET,
            "/repositories/second/size",
            None,
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
            None,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = send(
            &app,
            Method::PUT,
            "/repositories/second",
            Some("text/turtle"),
            None,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = send(&app, Method::PUT, "/repositories/a%2Fb", None, None, "").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = send(
            &app,
            Method::POST,
            "/repositories/second/statements",
            Some("text/turtle"),
            None,
            DATA,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            size_of(app.clone(), "second").await,
            (StatusCode::OK, "3".to_owned())
        );
        assert_eq!(
            size_of(app.clone(), "nrese").await,
            (StatusCode::OK, "0".to_owned())
        );
        let (_, list) = send(
            &app,
            Method::GET,
            "/repositories",
            None,
            Some("application/sparql-results+json"),
            "",
        )
        .await;
        assert!(
            list.contains("\"second\"") && list.contains("\"nrese\""),
            "{list}"
        );
        // Queries at the repository read its data.
        let (status, results) = send(
            &app,
            Method::GET,
            &format!(
                "/repositories/second?{}",
                encode(&[("query", "ASK { <http://example.com/b> ?p ?o }")])
            ),
            None,
            Some("application/sparql-results+json"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(results.contains("true"), "{results}");
    }
    // After a restart the repository and its data are back; then it goes.
    let app = app();
    assert_eq!(
        size_of(app.clone(), "second").await,
        (StatusCode::OK, "3".to_owned())
    );
    let (status, _) = send(&app, Method::DELETE, "/repositories/nrese", None, None, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(&app, Method::DELETE, "/repositories/second", None, None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        size_of(app.clone(), "second").await.0,
        StatusCode::NOT_FOUND
    );
}
