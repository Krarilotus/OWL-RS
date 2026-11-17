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
use nrese_server::auth::AuthConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::{StoreConfig, StoreService};
use serde_json::{Value, json};
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::held_service::HeldEndpoint;
use crate::support::{body_text, test_app_with_store};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

const TARGET: &str = "urn:x:target";
const SOURCE: &str = "urn:x:source";
const WRITER: Who = Who::Roles(&["writer"]);
const ADMIN: Who = Who::Roles(&["nrese.admin"]);
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

/// Who sends a request: a token's roles, a token's user (no roles), or a local login.
#[derive(Clone, Copy, Debug)]
enum Who {
    Roles(&'static [&'static str]),
    User(&'static str),
    Login(&'static str, &'static str),
}

impl Who {
    fn authorization(self) -> String {
        #[derive(serde::Serialize)]
        struct Claims {
            exp: usize,
            sub: &'static str,
            roles: [&'static str; 0],
        }
        let secret = jsonwebtoken::EncodingKey::from_secret(b"test-secret");
        let header = jsonwebtoken::Header::default();
        match self {
            Self::Roles(roles) => format!("Bearer {}", token(roles)),
            Self::User(sub) => {
                let claims = Claims {
                    exp: 4_102_444_800,
                    sub,
                    roles: [],
                };
                let token = jsonwebtoken::encode(&header, &claims, &secret).expect("jwt");
                format!("Bearer {token}")
            }
            Self::Login(user, password) => {
                use base64::Engine;
                let pair =
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
                format!("Basic {pair}")
            }
        }
    }
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
    who: Who,
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: String,
) -> Result<Answer, String> {
    let mut request = Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", who.authorization())
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
    roles: Who,
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
    app_under(POLICY, endpoint, mode)
}

/// [`app`] under the access policy `policy`.
fn app_under(policy: &str, endpoint: &HeldEndpoint, mode: ReasoningMode) -> Fallible<axum::Router> {
    let store = StoreService::new(StoreConfig::in_memory())?;
    store.set_service_client(endpoint.client()?);
    test_app_with_store(store, policy_of(policy)?, ReasonerConfig::for_mode(mode))
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
    who: Who,
    entry: Entry,
    update: String,
    during: (&str, Value),
) -> Fallible<Answer> {
    let mut running = tokio::spawn(write(app.clone(), who, entry, update));
    let call = endpoint.call(&mut running).await?;
    change(app, &during).await?;
    call.answer().await?;
    Ok(running.await??)
}

/// The writer's write right ends while its write through `entry` runs: nothing is published.
/// A single write may be sent again (503, `Retry-After: 1`); a client transaction's commit has
/// ended it, so the client begins a new one (409).
async fn a_write_whose_right_ends_publishes_nothing(
    entry: Entry,
    mode: ReasoningMode,
) -> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let app = app(&endpoint, mode)?;
    let update = held_insert(&endpoint, false);
    let answer = raced(&app, &endpoint, WRITER, entry, update, writer_role(&[])).await?;
    let transaction = matches!(entry, Entry::Rdf4jTransaction | Entry::Session);
    let refused = match transaction {
        true => StatusCode::CONFLICT,
        false => StatusCode::SERVICE_UNAVAILABLE,
    };
    assert_eq!(
        (answer.status, written(&app).await?),
        (refused, false),
        "{entry:?} under {mode:?}: refused, nothing written ({})",
        answer.body
    );
    if transaction {
        assert!(
            answer.body.contains("begin a new transaction"),
            "{}",
            answer.body
        );
        return Ok(());
    }
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
    let answer = raced(&app, &endpoint, WRITER, Entry::SparqlUpdate, update, during).await?;
    assert_eq!(
        (answer.status, written(&app).await?),
        (StatusCode::SERVICE_UNAVAILABLE, false),
        "refused, nothing copied ({})",
        answer.body
    );
    Ok(())
}
/// A change of the access state that leaves the writer's rights as they were refuses nothing:
/// another role's rule, another user, the writer's own rule written again unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_that_leaves_the_writers_rights_refuses_nothing() -> Fallible<()> {
    let other_role = json!({ "read": [SOURCE, TARGET], "reason": "test" });
    let other_user = json!({ "roles": ["other"], "reason": "test" });
    for during in [
        ("/api/v1/access/roles/other", other_role),
        ("/api/v1/access/users/carol", other_user),
        writer_role(&[TARGET]),
    ] {
        let endpoint = HeldEndpoint::bind().await?;
        let app = app(&endpoint, ReasoningMode::Disabled)?;
        let update = held_insert(&endpoint, false);
        let uri = during.0;
        let answer = raced(&app, &endpoint, WRITER, Entry::SparqlUpdate, update, during).await?;
        assert_eq!(
            (answer.status, written(&app).await?),
            (StatusCode::NO_CONTENT, true),
            "{uri}: committed ({})",
            answer.body
        );
    }
    Ok(())
}

/// A write refused because its rights changed, sent again, is judged under the new rights:
/// refused (403) without the right, committed once it is given back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_refused_for_changed_rights_is_judged_anew_when_sent_again() -> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let app = app(&endpoint, ReasoningMode::Disabled)?;
    let update = held_insert(&endpoint, false);
    let answer = raced(
        &app,
        &endpoint,
        WRITER,
        Entry::SparqlUpdate,
        update.clone(),
        writer_role(&[]),
    )
    .await?;
    expect(answer, StatusCode::SERVICE_UNAVAILABLE)?;
    let again = write(app.clone(), WRITER, Entry::SparqlUpdate, update.clone()).await?;
    assert_eq!(again.status, StatusCode::FORBIDDEN, "{}", again.body);
    change(&app, &writer_role(&[TARGET])).await?;
    let mut running = tokio::spawn(write(app.clone(), WRITER, Entry::SparqlUpdate, update));
    endpoint.call(&mut running).await?.answer().await?;
    let answer = running.await??;
    assert_eq!(
        (answer.status, written(&app).await?),
        (StatusCode::NO_CONTENT, true),
        "{}",
        answer.body
    );
    Ok(())
}

const PASSWORD: &str = "correct horse battery";

/// An access policy that lets every named user write what no rule names.
const OPEN: &str = r#"
default = "allow"

[[role]]
name = "other"
read = ["urn:x:source"]
"#;

/// The right of a user record to administer is the access state's where a write publishes,
/// a local login's too: an administrator demoted while its write runs publishes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_local_administrator_demoted_while_its_write_runs_publishes_nothing() -> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let app = app(&endpoint, ReasoningMode::Disabled)?;
    let alice = json!({ "admin": true, "password": PASSWORD, "reason": "test" });
    change(&app, &("/api/v1/access/users/alice", alice)).await?;
    let demoted = (
        "/api/v1/access/users/alice",
        json!({ "admin": false, "reason": "test" }),
    );
    let (who, update) = (Who::Login("alice", PASSWORD), held_insert(&endpoint, false));
    let answer = raced(&app, &endpoint, who, Entry::SparqlUpdate, update, demoted).await?;
    assert_eq!(
        (answer.status, written(&app).await?),
        (StatusCode::SERVICE_UNAVAILABLE, false),
        "refused, nothing written ({})",
        answer.body
    );
    Ok(())
}

/// A write the access state admitted (an open policy's grant to a named user) publishes only
/// while the state still admits it: the grant ends with enforcement, and the user's graph
/// rights (every graph, before and after) don't show it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_whose_admitting_grant_ends_while_it_runs_publishes_nothing() -> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let app = app_under(OPEN, &endpoint, ReasoningMode::Disabled)?;
    let off = (
        "/api/v1/access/settings",
        json!({ "enforced": false, "reason": "test" }),
    );
    let update = held_insert(&endpoint, false);
    let answer = raced(
        &app,
        &endpoint,
        Who::User("bob"),
        Entry::SparqlUpdate,
        update,
        off,
    )
    .await?;
    assert_eq!(
        (answer.status, written(&app).await?),
        (StatusCode::SERVICE_UNAVAILABLE, false),
        "refused, nothing written ({})",
        answer.body
    );
    Ok(())
}

/// A token's user whose record administers is admitted to write by the access state's write
/// grant, which ends with enforcement: its write publishes nothing then, though the user still
/// administers and reads every graph, and a fresh write of it is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_users_write_admitted_by_its_record_ends_with_enforcement() -> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let app = app(&endpoint, ReasoningMode::Disabled)?;
    let bob = json!({ "admin": true, "reason": "test" });
    change(&app, &("/api/v1/access/users/bob", bob)).await?;
    let off = (
        "/api/v1/access/settings",
        json!({ "enforced": false, "reason": "test" }),
    );
    let (who, update) = (Who::User("bob"), held_insert(&endpoint, false));
    let answer = raced(
        &app,
        &endpoint,
        who,
        Entry::SparqlUpdate,
        update.clone(),
        off,
    )
    .await?;
    let again = write(app.clone(), who, Entry::SparqlUpdate, update).await?;
    assert_eq!(
        (answer.status, written(&app).await?, again.status),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            false,
            StatusCode::FORBIDDEN
        ),
        "refused, nothing written ({})",
        answer.body
    );
    Ok(())
}

/// Without authentication a local login is still the access state's to admit: an administrator
/// demoted while its write runs (enforcement off: every graph, before and after) publishes
/// nothing, and a fresh write of that login is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_local_login_demoted_while_its_write_runs_publishes_nothing_without_authentication()
-> Fallible<()> {
    let endpoint = HeldEndpoint::bind().await?;
    let store = StoreService::new(StoreConfig::in_memory())?;
    store.set_service_client(endpoint.client()?);
    let policy = PolicyConfig {
        auth: AuthConfig::None,
        ..policy_of(POLICY)?
    };
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Disabled);
    let app = test_app_with_store(store, policy, reasoner)?;
    let off = json!({ "enforced": false, "reason": "test" });
    change(&app, &("/api/v1/access/settings", off)).await?;
    let alice = json!({ "admin": true, "password": PASSWORD, "reason": "test" });
    change(&app, &("/api/v1/access/users/alice", alice)).await?;
    let demoted = (
        "/api/v1/access/users/alice",
        json!({ "admin": false, "reason": "test" }),
    );
    let (who, update) = (Who::Login("alice", PASSWORD), held_insert(&endpoint, false));
    let answer = raced(
        &app,
        &endpoint,
        who,
        Entry::SparqlUpdate,
        update.clone(),
        demoted,
    )
    .await?;
    let again = write(app.clone(), who, Entry::SparqlUpdate, update).await?;
    assert_eq!(
        (answer.status, written(&app).await?, again.status),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            false,
            StatusCode::FORBIDDEN
        ),
        "refused, nothing written ({})",
        answer.body
    );
    Ok(())
}

/// A restore is an administrator's: one whose user record stops administering while it waits
/// for the writer slot (held by an administrator's update in its `SERVICE` call) restores
/// nothing. The restore's handler waits on nothing before it asks for the slot, so the half
/// second it is given to get there is a bound, not a race: earlier, it would be refused 403.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_whose_administrator_is_demoted_while_it_waits_restores_nothing() -> Fallible<()>
{
    let endpoint = HeldEndpoint::bind().await?;
    let app = app(&endpoint, ReasoningMode::Disabled)?;
    let bob = "/api/v1/access/users/bob";
    change(&app, &(bob, json!({ "admin": true, "reason": "test" }))).await?;
    let update = held_insert(&endpoint, false);
    let mut holding = tokio::spawn(write(app.clone(), ADMIN, Entry::SparqlUpdate, update));
    let call = endpoint.call(&mut holding).await?;
    let restoring = tokio::spawn({
        let (app, dump) = (app.clone(), "<urn:x:r> <urn:x:p> <urn:x:o> .\n".to_owned());
        let uri = "/ops/api/admin/dataset/restore";
        async move {
            send(
                &app,
                Who::User("bob"),
                Method::POST,
                uri,
                Some("application/n-quads"),
                dump,
            )
            .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    change(&app, &(bob, json!({ "admin": false, "reason": "test" }))).await?;
    call.answer().await?;
    expect(holding.await??, StatusCode::NO_CONTENT)?;
    let answer = restoring.await??;
    let ask = "ASK { <urn:x:r> <urn:x:p> <urn:x:o> }".to_owned();
    let query = Some("application/sparql-query");
    let restored = send(&app, ADMIN, Method::POST, "/dataset/query", query, ask).await?;
    let restored: Value = serde_json::from_str(&expect(restored, StatusCode::OK)?.body)?;
    assert_eq!(
        (answer.status, restored["boolean"].as_bool()),
        (StatusCode::SERVICE_UNAVAILABLE, Some(false)),
        "refused, nothing restored ({})",
        answer.body
    );
    Ok(())
}
