use super::{REPO, app, begin, commit, query, send, update};
use axum::Router;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

const NATIVE_REPO: &str = "/api/v1/repositories/nrese";
const ORIGINAL_NAMESPACE: &str = "http://example.com/a/";
const REBOUND_NAMESPACE: &str = "http://example.com/b/";
const INSERT: &str = "INSERT DATA { <urn:s> ex:p <urn:o> }";
const PREDICATES: &str = "SELECT ?p WHERE { <urn:s> ?p <urn:o> } ORDER BY ?p";

async fn bind_ex(app: &Router, repo: &str, namespace: &str) {
    let (status, _, body) = send(
        app,
        Method::PUT,
        &format!("{repo}/namespaces/ex"),
        &[("content-type", "text/plain")],
        namespace,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "bind ex to {namespace}: {body}"
    );
}

async fn read_predicates(app: &Router, uri: &str) -> Value {
    query(app, uri, PREDICATES).await["results"]["bindings"].take()
}

fn assert_staged_binding(before_rebind: &Value, after_rebind: &Value, committed: &Value) {
    let expected = json!([{"p":{"type":"uri","value":"http://example.com/a/p"}}]);
    let mut failures = Vec::new();
    for (step, actual, expected) in [
        ("read before the rebind", before_rebind, &expected),
        ("read after the rebind", after_rebind, &expected),
        (
            "read after the rebind disagrees with the commit",
            after_rebind,
            committed,
        ),
        (
            "commit differs from the binding at staging",
            committed,
            &expected,
        ),
    ] {
        if actual != expected {
            failures.push(format!("{step}: expected {expected}, got {actual}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
#[ignore = "Staged updates do not retain their original prefix binding"]
async fn rdf4j_transaction_preserves_staged_prefix_binding() {
    // Spec: A staged operation means what it meant when staged. After a rebind,
    // both transaction reads and the committed triple use only the original IRI.
    let app = app(false, false);
    bind_ex(&app, REPO, ORIGINAL_NAMESPACE).await;
    let location = begin(&app).await;
    update(&app, &format!("{location}?action=UPDATE"), INSERT).await;
    let query_uri = format!("{location}?action=QUERY");
    let before_rebind = read_predicates(&app, &query_uri).await;
    bind_ex(&app, REPO, REBOUND_NAMESPACE).await;
    let after_rebind = read_predicates(&app, &query_uri).await;
    commit(&app, &location).await;
    let committed = read_predicates(&app, REPO).await;
    assert_staged_binding(&before_rebind, &after_rebind, &committed);
}

#[tokio::test]
#[ignore = "Staged updates do not retain their original prefix binding"]
async fn native_session_preserves_staged_prefix_binding() {
    // Spec: A staged operation means what it meant when staged. After a rebind,
    // both session reads and the committed triple use only the original IRI.
    let app = app(false, false);
    bind_ex(&app, NATIVE_REPO, ORIGINAL_NAMESPACE).await;
    let (status, _, body) = send(
        &app,
        Method::POST,
        &format!("{NATIVE_REPO}/sessions"),
        &[],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "begin session: {body}");
    let opened: Value = serde_json::from_str(&body).unwrap();
    let path = opened["path"].as_str().unwrap();
    update(&app, &format!("{path}/update"), INSERT).await;
    let query_uri = format!("{path}/query");
    let before_rebind = read_predicates(&app, &query_uri).await;
    bind_ex(&app, NATIVE_REPO, REBOUND_NAMESPACE).await;
    let after_rebind = read_predicates(&app, &query_uri).await;
    let (status, _, body) = send(&app, Method::POST, &format!("{path}/commit"), &[], "").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "commit session: {body}");
    let committed = read_predicates(&app, &format!("{NATIVE_REPO}/query")).await;
    assert_staged_binding(&before_rebind, &after_rebind, &committed);
}
