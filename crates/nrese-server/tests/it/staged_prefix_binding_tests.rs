use super::{REPO, STATEMENTS, app, begin, commit, query, send, update, upload};
use axum::Router;
use axum::http::{Method, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::{build_app, policy::PolicyConfig};
use nrese_store::{SearchScope, StoreConfig};
use serde_json::{Value, json};

use crate::support::test_state_with_store_config;

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
async fn staging_context_keeps_the_search_scope_from_staging() {
    // Spec: A staged WHERE uses the search scope in force at staging. Its first
    // transaction read and commit keep the global winner after switching to reader scope.
    let state = test_state_with_store_config(
        StoreConfig {
            search_scope: SearchScope::Global,
            ..StoreConfig::default()
        },
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )
    .unwrap();
    let app = build_app(state.clone());
    // Among the default graph's two matches, the shorter "beta" wins. The named graph
    // makes beta common in the global corpus, so the rarer "alpha" wins there.
    let mut data =
        String::from("<urn:a> <urn:label> \"alpha one two\" . <urn:b> <urn:label> \"beta\" . ");
    for n in 0..8 {
        data.push_str(&format!("<urn:f{n}> <urn:label> \"filler {n}\" . "));
    }
    data.push_str("<urn:g> { ");
    for n in 0..60 {
        data.push_str(&format!("<urn:x{n}> <urn:label> \"beta extra {n}\" . "));
    }
    data.push('}');
    upload(&app, &data).await;

    let prefix = "PREFIX bds: <http://www.bigdata.com/rdf/search#>";
    let selection = r#"SELECT ?o WHERE { ?o bds:search "alpha beta"; bds:relevance ?score }
                       ORDER BY DESC(?score) ?o LIMIT 1"#;
    let control = format!("{prefix} {selection}");
    let global = json!([{"o":{"type":"literal","value":"alpha one two"}}]);
    let reader = json!([{"o":{"type":"literal","value":"beta"}}]);
    assert_eq!(
        query(&app, REPO, &control).await["results"]["bindings"],
        global,
        "global ranking must select the rarer word"
    );
    let location = begin(&app).await;
    update(
        &app,
        &format!("{location}?action=UPDATE"),
        &format!("{prefix} INSERT {{ <urn:winner> <urn:label> ?o }} WHERE {{ {{ {selection} }} }}"),
    )
    .await;
    state.store().set_search_scope(SearchScope::Reader);
    assert_eq!(
        query(&app, REPO, &control).await["results"]["bindings"],
        reader,
        "reader ranking must select the shorter visible match"
    );

    let result = "SELECT ?o WHERE { <urn:winner> <urn:label> ?o }";
    let pending = query(&app, &format!("{location}?action=QUERY"), result).await;
    commit(&app, &location).await;
    let committed = query(&app, REPO, result).await;
    assert_eq!(
        [
            &pending["results"]["bindings"],
            &committed["results"]["bindings"]
        ],
        [&global, &global],
        "first read after the scope change and committed data, in that order"
    );
}

#[tokio::test]
async fn staging_context_one_shot_uses_the_live_namespace() {
    // Spec: A one-shot update uses the live namespace binding even while an open
    // transaction has staged an update under an earlier binding.
    let app = app(false, false);
    bind_ex(&app, REPO, ORIGINAL_NAMESPACE).await;
    let location = begin(&app).await;
    update(&app, &format!("{location}?action=UPDATE"), INSERT).await;
    bind_ex(&app, REPO, REBOUND_NAMESPACE).await;
    update(&app, STATEMENTS, INSERT).await;
    assert_eq!(
        read_predicates(&app, REPO).await,
        json!([{"p":{"type":"uri","value":"http://example.com/b/p"}}]),
        "one-shot update while the transaction is open"
    );
    commit(&app, &location).await;
    assert_eq!(
        read_predicates(&app, REPO).await,
        json!([
            {"p":{"type":"uri","value":"http://example.com/a/p"}},
            {"p":{"type":"uri","value":"http://example.com/b/p"}},
        ]),
        "transaction commit adds the predicate from the staged binding"
    );
}

#[tokio::test]
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
