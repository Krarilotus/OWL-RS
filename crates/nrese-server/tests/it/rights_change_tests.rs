//! A write publishes only with the rights it was evaluated with (docs/ops/http-api.md: what a
//! requester may write and read are its own rights): where its requester's rights change
//! while it runs, it is refused with nothing published (503, `Retry-After: 1`), and sent again
//! it is judged under the new rights. A change that leaves them as they were refuses nothing.
//!
//! Each write is held inside the pipeline by a `SERVICE` call in its `WHERE` clause
//! (`support::held_service`): once the call has connected, the write has its rights and is
//! being evaluated, so the change lands between the two, on every host alike.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::{StoreConfig, StoreService};
use serde_json::{Value, json};
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::held_service::HeldEndpoint;
use crate::support::{body_text, test_app_with_store};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

const TARGET: &str = "urn:x:target";
const SOURCE: &str = "urn:x:source";
const WRITER: &[&str] = &["writer"];
const ADMIN: &[&str] = &["nrese.admin"];
const SPARQL_UPDATE: Option<&str> = Some("application/sparql-update");

const POLICY: &str = r#"
default = "deny"

[[role]]
name = "writer"
read = ["urn:x:source", "urn:x:target"]
write = ["urn:x:target"]
service = true

[[role]]
name = "other"
read = ["urn:x:source"]
"#;

/// Where a write enters the server.
#[derive(Clone, Copy, Debug)]
enum Entry {
    SparqlUpdate,
    Rdf4jStatements,
    Rdf4jTransaction,
    Session,
}

/// What the server answered.
#[derive(Debug)]
struct Answer {
    status: StatusCode,
    retry_after: Option<String>,
    location: Option<String>,
    body: String,
}

async fn send(
    app: &axum::Router,
    roles: &[&str],
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: String,
) -> Result<Answer, String> {
    let mut request = Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", format!("Bearer {}", token(roles)))
        .header("accept", "application/sparql-results+json, */*;q=0.1");
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let request = request.body(Body::from(body)).map_err(|e| e.to_string())?;
    let response = app
        .clone()
        .oneshot(request)
        .await
        .map_err(|e| e.to_string())?;
    let (retry_after, location) = {
        let header = |name: &str| {
            let value = response.headers().get(name)?.to_str().ok()?;
            Some(value.to_owned())
        };
        (header("retry-after"), header("location"))
    };
    let status = response.status();
    let body = body_text(response).await.map_err(|e| e.to_string())?;
    Ok(Answer {
        status,
        retry_after,
        location,
        body,
    })
}

/// `answer`, if it has `status`.
fn expect(answer: Answer, status: StatusCode) -> Result<Answer, String> {
    match answer.status == status {
        true => Ok(answer),
        false => Err(format!(
            "{} where {status} belongs: {}",
            answer.status, answer.body
        )),
    }
}

/// `update` sent by `roles` through `entry`: the answer of the request that commits it.
async fn write(
    app: axum::Router,
    roles: &[&str],
    entry: Entry,
    update: String,
) -> Result<Answer, String> {
    let empty = String::new;
    match entry {
        Entry::SparqlUpdate => {
            send(
                &app,
                roles,
                Method::POST,
                "/dataset/update",
                SPARQL_UPDATE,
                update,
            )
            .await
        }
        Entry::Rdf4jStatements => {
            let uri = "/repositories/nrese/statements";
            send(&app, roles, Method::POST, uri, SPARQL_UPDATE, update).await
        }
        Entry::Rdf4jTransaction => {
            let uri = "/repositories/nrese/transactions";
            let begun = send(&app, roles, Method::POST, uri, None, empty()).await?;
            let tx = expect(begun, StatusCode::CREATED)?
                .location
                .ok_or("no Location")?;
            let form =
                serde_urlencoded::to_string([("update", update)]).map_err(|e| e.to_string())?;
            let form_type = Some("application/x-www-form-urlencoded");
            let uri = format!("{tx}?action=UPDATE");
            expect(
                send(&app, roles, Method::PUT, &uri, form_type, form).await?,
                StatusCode::OK,
            )?;
            let uri = format!("{tx}?action=COMMIT");
            send(&app, roles, Method::PUT, &uri, None, empty()).await
        }
        Entry::Session => {
            let uri = "/api/v1/repositories/nrese/sessions";
            let opened = send(&app, roles, Method::POST, uri, None, empty()).await?;
            let opened: Value = serde_json::from_str(&expect(opened, StatusCode::CREATED)?.body)
                .map_err(|e| e.to_string())?;
            let session = opened["path"].as_str().ok_or("no session path")?.to_owned();
            let uri = format!("{session}/update");
            let added = send(&app, roles, Method::POST, &uri, SPARQL_UPDATE, update).await?;
            expect(added, StatusCode::NO_CONTENT)?;
            let uri = format!("{session}/commit");
            send(&app, roles, Method::POST, &uri, None, empty()).await
        }
    }
}

/// A server under [`POLICY`] whose `SERVICE` calls go to `endpoint`.
fn app(endpoint: &HeldEndpoint, mode: ReasoningMode) -> Fallible<axum::Router> {
    let store = StoreService::new(StoreConfig::in_memory())?;
    store.set_service_client(endpoint.client()?);
    test_app_with_store(store, policy_of(POLICY)?, ReasonerConfig::for_mode(mode))
}

/// An update that writes `<urn:x:a> <urn:x:p> <urn:x:o>` into the target, held in its
/// `SERVICE` call to `endpoint` until the test answers it; `source`: copied from the source
/// graph instead.
fn held_insert(endpoint: &HeldEndpoint, source: bool) -> String {
    let service = format!("SERVICE <{}> {{ ?x ?y ?z }}", endpoint.url());
    match source {
        false => format!(
            "INSERT {{ GRAPH <{TARGET}> {{ <urn:x:a> <urn:x:p> <urn:x:o> }} }} WHERE {{ {service} }}"
        ),
        true => format!(
            "INSERT {{ GRAPH <{TARGET}> {{ ?s ?p ?o }} }} \
             WHERE {{ GRAPH <{SOURCE}> {{ ?s ?p ?o }} {service} }}"
        ),
    }
}

/// Whether the target holds the statement the writes write, as an administrator reads it.
async fn written(app: &axum::Router) -> Fallible<bool> {
    let ask = format!("ASK {{ GRAPH <{TARGET}> {{ <urn:x:a> <urn:x:p> <urn:x:o> }} }}");
    let content_type = Some("application/sparql-query");
    let answer = send(
        app,
        ADMIN,
        Method::POST,
        "/dataset/query",
        content_type,
        ask,
    )
    .await?;
    let answer = expect(answer, StatusCode::OK)?;
    let json: Value = serde_json::from_str(&answer.body)?;
    Ok(json["boolean"].as_bool().ok_or("no boolean")?)
}

/// An administrator's change of the access state: `PUT` of `body` at `uri`.
async fn change(app: &axum::Router, (uri, body): &(&str, Value)) -> Fallible<()> {
    let content_type = Some("application/json");
    let answer = send(app, ADMIN, Method::PUT, uri, content_type, body.to_string()).await?;
    expect(answer, StatusCode::OK)?;
    Ok(())
}

/// The writer's role as the policy has it, writing `write` (empty: nothing).
fn writer_role(write: &[&str]) -> (&'static str, Value) {
    let rule = json!({
        "read": [SOURCE, TARGET], "write": write, "service": true, "reason": "test",
    });
    ("/api/v1/access/roles/writer", rule)
}

/// The writer's `update` through `entry`, held in its `SERVICE` call while `during` applies,
/// then let go on: its answer.
async fn raced(
    app: &axum::Router,
    endpoint: &HeldEndpoint,
    entry: Entry,
    update: String,
    during: (&str, Value),
) -> Fallible<Answer> {
    let mut running = tokio::spawn(write(app.clone(), WRITER, entry, update));
    let call = endpoint.call(&mut running).await?;
    change(app, &during).await?;
    call.answer().await?;
    Ok(running.await??)
}

/// The writer's write right ends while its write through `entry` runs: nothing is published,
/// and the client is told to send it again.
async fn a_write_whose_right_ends_publishes_nothing(
    entry: Entry,
    mode: ReasoningMode,
) -> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let app = app(&endpoint, mode)?;
    let update = held_insert(&endpoint, false);
    let answer = raced(&app, &endpoint, entry, update, writer_role(&[])).await?;
    assert_eq!(
        (answer.status, written(&app).await?),
        (StatusCode::SERVICE_UNAVAILABLE, false),
        "{entry:?} under {mode:?}: refused, nothing written ({})",
        answer.body
    );
    assert_eq!(answer.retry_after.as_deref(), Some("1"), "{entry:?}");
    let problem: Value = serde_json::from_str(&answer.body)?;
    assert_eq!(
        problem["type"], "https://nrese.dev/problems/rights-changed",
        "{entry:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sparql_update_whose_write_right_ends_while_it_runs_publishes_nothing() -> Fallible<()> {
    a_write_whose_right_ends_publishes_nothing(Entry::SparqlUpdate, ReasoningMode::Disabled).await
}

/// The same through the commit that maintains the inferred stack.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reasoning_commit_whose_write_right_ends_while_it_runs_publishes_nothing() -> Fallible<()>
{
    a_write_whose_right_ends_publishes_nothing(Entry::SparqlUpdate, ReasoningMode::Rdfs).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rdf4j_statements_whose_write_right_ends_while_they_run_publish_nothing() -> Fallible<()> {
    a_write_whose_right_ends_publishes_nothing(Entry::Rdf4jStatements, ReasoningMode::Disabled)
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_rdf4j_transaction_whose_write_right_ends_at_its_commit_publishes_nothing()
-> Fallible<()> {
    a_write_whose_right_ends_publishes_nothing(Entry::Rdf4jTransaction, ReasoningMode::Disabled)
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_whose_write_right_ends_at_its_commit_publishes_nothing() -> Fallible<()> {
    a_write_whose_right_ends_publishes_nothing(Entry::Session, ReasoningMode::Disabled).await
}

/// What a write reads is a right too: an update that copies from a graph whose read ends while
/// it runs publishes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_whose_read_right_ends_while_it_runs_publishes_nothing() -> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let app = app(&endpoint, ReasoningMode::Disabled)?;
    let seed = format!("INSERT DATA {{ GRAPH <{SOURCE}> {{ <urn:x:a> <urn:x:p> <urn:x:o> }} }}");
    expect(
        send(
            &app,
            ADMIN,
            Method::POST,
            "/dataset/update",
            SPARQL_UPDATE,
            seed,
        )
        .await?,
        StatusCode::NO_CONTENT,
    )?;
    let read_revoked = json!({
        "read": [TARGET], "write": [TARGET], "service": true, "reason": "test",
    });
    let during = ("/api/v1/access/roles/writer", read_revoked);
    let update = held_insert(&endpoint, true);
    let answer = raced(&app, &endpoint, Entry::SparqlUpdate, update, during).await?;
    assert_eq!(
        (answer.status, written(&app).await?),
        (StatusCode::SERVICE_UNAVAILABLE, false),
        "refused, nothing copied ({})",
        answer.body
    );
    Ok(())
}
