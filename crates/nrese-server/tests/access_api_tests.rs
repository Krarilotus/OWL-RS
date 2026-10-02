//! Users, workspaces and graph policies through the engine API (ADR-0008): every user's
//! personal space, workspaces with owners, editors and viewers, role rules changed by
//! administrators, each change with a reason and kept in the history, and what that
//! changes for SPARQL reads and writes.

mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use jsonwebtoken::{EncodingKey, Header, encode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::auth::{AuthConfig, JwtBearerConfig};
use nrese_server::policy::PolicyConfig;
use serde::Serialize;
use serde_json::{Value, json};
use tower::util::ServiceExt;

use support::{body_text, test_app_with_settings};

#[derive(Serialize)]
struct Claims {
    exp: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    sub: Option<String>,
    roles: Vec<String>,
}

/// Who sends a request: a user name (the token's subject) and roles.
#[derive(Clone, Copy)]
struct As<'a>(Option<&'a str>, &'a [&'a str]);

const ADMIN: As<'static> = As(Some("root"), &["nrese.admin"]);

fn token(who: As<'_>) -> String {
    encode(
        &Header::default(),
        &Claims {
            exp: 4_102_444_800,
            sub: who.0.map(str::to_owned),
            roles: who.1.iter().map(|role| (*role).to_owned()).collect(),
        },
        &EncodingKey::from_secret(b"test-secret"),
    )
    .expect("jwt")
}

fn app() -> axum::Router {
    let policy = PolicyConfig {
        auth: AuthConfig::BearerJwt(JwtBearerConfig {
            shared_secret: "test-secret".to_owned(),
            issuer: None,
            audience: None,
            read_role: "nrese.read".to_owned(),
            admin_role: "nrese.admin".to_owned(),
            leeway_seconds: 0,
        }),
        ..PolicyConfig::default()
    };
    test_app_with_settings(policy, ReasonerConfig::default()).unwrap()
}

async fn send(
    app: &axum::Router,
    who: As<'_>,
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", format!("Bearer {}", token(who)))
        .header("accept", "application/sparql-results+json, */*;q=0.1");
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (status, body_text(response).await.unwrap())
}

async fn put(app: &axum::Router, who: As<'_>, uri: &str, body: Value) -> (StatusCode, Value) {
    let (status, text) = send(
        app,
        who,
        Method::PUT,
        uri,
        Some("application/json"),
        &body.to_string(),
    )
    .await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn get(app: &axum::Router, who: As<'_>, uri: &str) -> (StatusCode, Value) {
    let (status, text) = send(app, who, Method::GET, uri, None, "").await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn update(app: &axum::Router, who: As<'_>, text: &str) -> StatusCode {
    send(
        app,
        who,
        Method::POST,
        "/dataset/update",
        Some("application/sparql-update"),
        text,
    )
    .await
    .0
}

/// The graphs the requester sees statements in, sorted.
async fn graphs(app: &axum::Router, who: As<'_>) -> Vec<String> {
    let (status, text) = send(
        app,
        who,
        Method::POST,
        "/dataset/query",
        Some("application/sparql-query"),
        "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json: Value = serde_json::from_str(&text).unwrap();
    let mut graphs: Vec<String> = json["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["g"]["value"].as_str().unwrap().to_owned())
        .collect();
    graphs.sort();
    graphs
}

const ALICE: As<'static> = As(Some("alice"), &[]);
const BOB: As<'static> = As(Some("bob"), &[]);
const CAROL: As<'static> = As(Some("carol"), &[]);

#[tokio::test]
async fn personal_spaces_and_workspaces_decide_what_users_read_and_write() {
    let app = app();
    // Enforcement is off until an administrator turns it on, with a reason.
    let (status, me) = get(&app, ADMIN, "/api/v1/access/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["enforced"], false);
    let (status, body) = put(
        &app,
        ADMIN,
        "/api/v1/access/settings",
        json!({ "enforced": true }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a reason is required: {body}"
    );
    let (status, _) = put(
        &app,
        ALICE,
        "/api/v1/access/settings",
        json!({ "enforced": true, "reason": "mine" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "alice isn't an administrator"
    );
    let (status, record) = put(
        &app,
        ADMIN,
        "/api/v1/access/settings",
        json!({ "enforced": true, "reason": "go live" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{record}");
    assert_eq!(record["number"], 1);

    // Every user has a personal space it alone writes, without any rule for it.
    let (_, me) = get(&app, ALICE, "/api/v1/access/me").await;
    assert_eq!(me["user"], "alice");
    let space = me["personal_space"]["prefix"].as_str().unwrap().to_owned();
    assert_eq!(space, "urn:nrese:space/alice/");
    let draft = format!("{space}draft");
    assert_eq!(
        update(
            &app,
            ALICE,
            &format!("INSERT DATA {{ GRAPH <{draft}> {{ <urn:x> <urn:p> 1 }} }}")
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        update(
            &app,
            BOB,
            &format!("INSERT DATA {{ GRAPH <{draft}> {{ <urn:y> <urn:p> 2 }} }}")
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(graphs(&app, ALICE).await, std::slice::from_ref(&draft));
    assert!(graphs(&app, BOB).await.is_empty());
    // Shared with bob as a viewer: bob reads it, still can't write it.
    let (status, body) = put(
        &app,
        ALICE,
        "/api/v1/access/workspaces/~alice/members/bob",
        json!({ "level": "viewer", "reason": "review my draft" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(graphs(&app, BOB).await, std::slice::from_ref(&draft));
    let (status, _) = put(
        &app,
        ALICE,
        "/api/v1/access/workspaces/~alice/members/bob",
        json!({ "level": "editor", "reason": "co-author" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "personal spaces have viewers only"
    );

    // A workspace: alice creates and owns it, bob edits, carol views.
    let (status, body) = put(
        &app,
        ALICE,
        "/api/v1/access/workspaces/project",
        json!({ "title": "Project", "reason": "project start" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (user, level) in [("bob", "editor"), ("carol", "viewer")] {
        let (status, body) = put(
            &app,
            ALICE,
            &format!("/api/v1/access/workspaces/project/members/{user}"),
            json!({ "level": level, "reason": "team" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _) = put(
        &app,
        BOB,
        "/api/v1/access/workspaces/project/members/dave",
        json!({ "level": "viewer", "reason": "my friend" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only owners manage members");
    let shared = "urn:nrese:workspace/project/data";
    assert_eq!(
        update(
            &app,
            BOB,
            &format!("INSERT DATA {{ GRAPH <{shared}> {{ <urn:z> <urn:p> 3 }} }}")
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        update(
            &app,
            CAROL,
            &format!("INSERT DATA {{ GRAPH <{shared}> {{ <urn:w> <urn:p> 4 }} }}")
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(graphs(&app, CAROL).await, [shared.to_owned()]);
    let (_, mine) = get(&app, CAROL, "/api/v1/access/workspaces").await;
    let names: Vec<&str> = mine
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["~carol", "project"]);
    let (status, _) = get(
        &app,
        As(Some("dave"), &[]),
        "/api/v1/access/workspaces/project",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not a member");
    let (status, view) = get(&app, CAROL, "/api/v1/access/workspaces/project").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["members"]["alice"], "owner");
    assert_eq!(view["level"], "viewer");
    // Administrators see everything.
    assert_eq!(graphs(&app, ADMIN).await.len(), 2);
}

#[tokio::test]
async fn administrators_manage_roles_and_users_with_a_history() {
    let app = app();
    let (status, _) = put(
        &app,
        ADMIN,
        "/api/v1/access/roles/analyst",
        json!({
            "read": ["http://example.com/g/pub/*"],
            "default_graph": "read",
            "reason": "analysts read the public graphs",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = put(
        &app,
        ADMIN,
        "/api/v1/access/settings",
        json!({ "enforced": true, "reason": "go live" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = put(
        &app,
        ADMIN,
        "/api/v1/access/roles/bad",
        json!({ "read": ["not an iri"], "reason": "typo" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        update(
            &app,
            ADMIN,
            "INSERT DATA { GRAPH <http://example.com/g/pub/1> { <urn:a> <urn:p> 1 } \
             GRAPH <http://example.com/g/hr> { <urn:b> <urn:p> 2 } }"
        )
        .await,
        StatusCode::NO_CONTENT
    );
    // A token without a subject but with the role reads what the role reads.
    let analyst = As(None, &["analyst"]);
    assert_eq!(graphs(&app, analyst).await, ["http://example.com/g/pub/1"]);
    // A user record gives a role to whoever logs in under its name.
    assert!(graphs(&app, BOB).await.is_empty());
    let (status, _) = put(
        &app,
        ADMIN,
        "/api/v1/access/users/bob",
        json!({ "roles": ["analyst"], "reason": "bob joined the analysts" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graphs(&app, BOB).await, ["http://example.com/g/pub/1"]);
    let (_, me) = get(&app, BOB, "/api/v1/access/me").await;
    assert_eq!(me["roles"], json!(["analyst"]));
    // ... or the administrator's right.
    let (status, _) = put(
        &app,
        ADMIN,
        "/api/v1/access/users/carol",
        json!({ "admin": true, "reason": "carol runs the server" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graphs(&app, CAROL).await.len(), 2);
    let (status, overview) = get(&app, CAROL, "/api/v1/access").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview["roles"][0]["name"], "analyst");
    let (status, _) = get(&app, BOB, "/api/v1/access").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The policy file form round-trips.
    let (status, toml) = send(&app, ADMIN, Method::GET, "/api/v1/access/export", None, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(toml.contains("name = \"analyst\""), "{toml}");
    let (status, text) = send(
        &app,
        ADMIN,
        Method::POST,
        "/api/v1/access/import?reason=restore%20the%20file",
        Some("application/toml"),
        &toml,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    // Removals take their reason as a parameter.
    let (status, _) = send(
        &app,
        ADMIN,
        Method::DELETE,
        "/api/v1/access/users/bob",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no reason");
    let (status, _) = send(
        &app,
        ADMIN,
        Method::DELETE,
        "/api/v1/access/users/bob?reason=left",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(graphs(&app, BOB).await.is_empty());
    // The history: every change, the latest first, who and why.
    let (status, history) = get(&app, CAROL, "/api/v1/access/history?limit=3").await;
    assert_eq!(status, StatusCode::OK);
    let entries: Vec<(u64, &str, &str)> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["number"].as_u64().unwrap(),
                r["author"].as_str().unwrap(),
                r["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            (6, "root", "left"),
            (5, "root", "restore the file"),
            (4, "root", "carol runs the server"),
        ]
    );
}

async fn send_with(
    app: &axum::Router,
    authorization: &str,
    method: Method,
    uri: &str,
    body: &str,
) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", authorization)
        .header("content-type", "application/json")
        .header("accept", "application/sparql-results+json, */*;q=0.1")
        .body(Body::from(body.to_owned()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    (status, body_text(response).await.unwrap())
}

async fn update_as(app: &axum::Router, authorization: &str, text: &str) -> StatusCode {
    let request = Request::builder()
        .uri("/api/v1/repositories/nrese/update")
        .method(Method::POST)
        .header("authorization", authorization)
        .header("content-type", "application/sparql-update")
        .body(Body::from(text.to_owned()))
        .unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

fn basic(user: &str, password: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

#[tokio::test]
async fn local_users_log_in_with_passwords_besides_tokens() {
    let app = app();
    let (status, body) = put(
        &app,
        ADMIN,
        "/api/v1/access/users/dana",
        json!({ "password": "short", "reason": "desktop user" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = put(
        &app,
        ADMIN,
        "/api/v1/access/users/dana",
        json!({ "password": "a long enough secret", "reason": "desktop user" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains("argon2"), "{body}");
    // Basic credentials: dana reads (enforcement is off) and has a personal space.
    let dana = basic("dana", "a long enough secret");
    let (status, me) = send_with(&app, &dana, Method::GET, "/api/v1/access/me", "").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    let me: Value = serde_json::from_str(&me).unwrap();
    assert_eq!(me["user"], "dana");
    let (status, _) = send_with(
        &app,
        &basic("dana", "wrong"),
        Method::GET,
        "/api/v1/access/me",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A session: log in once, then the token.
    let (status, session) = send_with(
        &app,
        "",
        Method::POST,
        "/api/v1/access/login",
        &json!({ "user": "dana", "password": "a long enough secret" }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let session: Value = serde_json::from_str(&session).unwrap();
    let bearer = format!("Bearer {}", session["token"].as_str().unwrap());
    assert_eq!(session["expires_in_seconds"], 12 * 3600);
    let (status, _) = send_with(&app, &bearer, Method::GET, "/api/v1/access/me", "").await;
    assert_eq!(status, StatusCode::OK);
    // Writes need enforcement (the personal space) or a role; then dana writes its own.
    let (status, _) = put(
        &app,
        ADMIN,
        "/api/v1/access/settings",
        json!({ "enforced": true, "reason": "go live" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let write = |graph: &str| format!("INSERT DATA {{ GRAPH <{graph}> {{ <urn:a> <urn:p> 1 }} }}");
    assert_eq!(
        update_as(&app, &bearer, &write("urn:nrese:space/dana/x")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        update_as(&app, &bearer, &write("urn:nrese:space/erin/x")).await,
        StatusCode::FORBIDDEN
    );
    // dana changes its own password: the session ends, the new password works.
    let (status, body) = send_with(
        &app,
        &bearer,
        Method::PUT,
        "/api/v1/access/users/dana",
        &json!({ "password": "another long secret", "reason": "rotation" }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = send_with(&app, &bearer, Method::GET, "/api/v1/access/me", "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send_with(
        &app,
        &basic("dana", "another long secret"),
        Method::GET,
        "/api/v1/access/me",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // ... but not its roles.
    let (status, _) = send_with(
        &app,
        &basic("dana", "another long secret"),
        Method::PUT,
        "/api/v1/access/users/dana",
        &json!({ "admin": true, "reason": "promotion" }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Logging out ends a session.
    let (_, session) = send_with(
        &app,
        "",
        Method::POST,
        "/api/v1/access/login",
        &json!({ "user": "dana", "password": "another long secret" }).to_string(),
    )
    .await;
    let session: Value = serde_json::from_str(&session).unwrap();
    let bearer = format!("Bearer {}", session["token"].as_str().unwrap());
    let (status, _) = send_with(&app, &bearer, Method::POST, "/api/v1/access/logout", "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send_with(&app, &bearer, Method::GET, "/api/v1/access/me", "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A client transaction (an `/api/v1` session or an RDF4J transaction) exists for the user
/// who opened it only: nobody else adds to it, reads in it, commits or closes it.
#[tokio::test]
async fn sessions_belong_to_whoever_opened_them() {
    let app = app();
    // Administrators both (updates need the role): a session is still its opener's.
    const ALICE: As<'static> = As(Some("alice"), &["nrese.admin"]);
    const BOB: As<'static> = As(Some("bob"), &["nrese.admin"]);
    let base = "/api/v1/repositories/nrese";
    let (status, text) = send(
        &app,
        ALICE,
        Method::POST,
        &format!("{base}/sessions"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let opened: Value = serde_json::from_str(&text).unwrap();
    let session = opened["path"].as_str().unwrap().to_owned();
    let write = "INSERT DATA { GRAPH <urn:nrese:space/alice/x> { <urn:a> <urn:p> 1 } }";
    let sparql_update = Some("application/sparql-update");
    let sparql_query = Some("application/sparql-query");
    let ask = "ASK { GRAPH ?g { <urn:a> <urn:p> 1 } }";
    for (method, path, content_type, body) in [
        (Method::POST, "/update", sparql_update, write),
        (Method::POST, "/query", sparql_query, ask),
        (Method::POST, "/commit", None, ""),
        (Method::DELETE, "", None, ""),
    ] {
        let uri = format!("{session}{path}");
        let (status, text) = send(&app, BOB, method, &uri, content_type, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {text}");
    }
    let (status, text) = send(
        &app,
        ALICE,
        Method::POST,
        &format!("{session}/update"),
        sparql_update,
        write,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    let (status, text) = send(
        &app,
        ALICE,
        Method::POST,
        &format!("{session}/commit"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");

    // RDF4J: the same for its transactions.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/repositories/nrese/transactions")
                .method(Method::POST)
                .header("authorization", format!("Bearer {}", token(ALICE)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    let transaction = location[location.find("/repositories/").unwrap()..].to_owned();
    for action in ["UPDATE", "SIZE", "COMMIT"] {
        let uri = format!("{transaction}?action={action}");
        let (status, text) = send(&app, BOB, Method::PUT, &uri, sparql_update, write).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {text}");
    }
    let (status, text) = send(&app, BOB, Method::DELETE, &transaction, None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    let (status, text) = send(&app, ALICE, Method::DELETE, &transaction, None, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
}

/// What a user may do in a repository depends on its workspaces there: a workspace bound
/// to one repository counts in that one only, whichever protocol the request speaks.
#[tokio::test]
async fn workspaces_count_in_their_own_repository() {
    let app = app();
    let (status, text) = send(
        &app,
        ADMIN,
        Method::PUT,
        "/api/v1/repositories/bench",
        Some("application/json"),
        "{}",
    )
    .await;
    assert!(status.is_success(), "{status}: {text}");
    let (status, body) = put(
        &app,
        ADMIN,
        "/api/v1/access/settings",
        json!({ "enforced": true, "reason": "go live" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = put(
        &app,
        ALICE,
        "/api/v1/access/workspaces/lab",
        json!({ "repository": "bench", "reason": "a lab in bench" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let insert = "update=INSERT+DATA+%7B+GRAPH+%3Curn%3Anrese%3Aworkspace%2Flab%2Fg%3E+%7B+%3Curn%3Aa%3E+%3Curn%3Ap%3E+1+%7D+%7D";
    let write = |repository: &'static str| {
        let app = app.clone();
        async move {
            send(
                &app,
                ALICE,
                Method::POST,
                &format!("/repositories/{repository}/statements"),
                Some("application/x-www-form-urlencoded"),
                insert,
            )
            .await
        }
    };
    // RDF4J on bench: the workspace is alice's there.
    let (status, text) = write("bench").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    // On the default repository it isn't.
    let (status, text) = write("nrese").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
}

/// Saved queries over the engine API: kept in the requester's spaces, private to them.
#[tokio::test]
async fn saved_queries_are_kept_per_space() {
    let app = app();
    // Enforced access: every named user has a personal space (and may query).
    let (status, body) = put(
        &app,
        ADMIN,
        "/api/v1/access/settings",
        json!({ "enforced": true, "reason": "go live" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = put(
        &app,
        ALICE,
        "/api/v1/queries/~alice/everything",
        json!({ "query": "SELECT * WHERE { ?s ?p ?o }", "title": "Everything" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["author"], "alice");
    let (status, body) = put(
        &app,
        ALICE,
        "/api/v1/queries/~alice/broken",
        json!({ "query": "SELECT * WHERE {" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = put(
        &app,
        BOB,
        "/api/v1/queries/~alice/mine",
        json!({ "query": "ASK {}" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, list) = get(&app, ALICE, "/api/v1/queries").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().map(Vec::len), Some(1), "{list}");
    let (_, list) = get(&app, BOB, "/api/v1/queries").await;
    assert_eq!(list.as_array().map(Vec::len), Some(0), "{list}");
    let (status, _) = get(&app, BOB, "/api/v1/queries/~alice/everything").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &app,
        ALICE,
        Method::DELETE,
        "/api/v1/queries/~alice/everything",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
