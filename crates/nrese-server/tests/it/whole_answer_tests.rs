//! Spec: an HTTP/1.0 request gets every streamed answer whole, with its `Content-Length`
//! (result_stream's "HTTP/1.0"): on every route that streams, the same answer HTTP/1.1 gets.
//! The answer held is charged to the request's memory envelope (`budgets.query_memory`):
//! past it the request is refused with 413, and what it held is released when it is sent or
//! refused, or when its client leaves.

use std::error::Error;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, Version, header};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::auth::{AuthConfig, StaticBearerConfig};
use nrese_server::policy::PolicyConfig;
use nrese_store::{StoreConfig, StoreService};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::cut_body_wire_tests::{WHOLE, http1_image, image_server, query};
use crate::support::held_service::{HANG, HeldEndpoint};
use crate::support::{minimal_fixture_path, test_app_with_store, test_app_with_store_config};

/// A store whose queries share a budget, so that what a request holds shows in it.
fn shared_store() -> Result<StoreService, Box<dyn Error>> {
    Ok(StoreService::new(StoreConfig {
        total_query_memory_bytes: 1 << 30,
        ..StoreConfig::in_memory()
    })?)
}

/// The bytes the queries' shared budget holds.
fn held(store: &StoreService) -> usize {
    store.query_memory().expect("a shared budget").0
}

/// Waits, in real time, until the queries' shared budget holds `bytes` again.
async fn released_to(store: &StoreService, bytes: usize) {
    let mut waited = Duration::ZERO;
    while held(store) != bytes {
        assert!(waited < HANG, "{} bytes still held", held(store));
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += Duration::from_millis(10);
    }
}

fn query_request(version: Version) -> Result<Request<Body>, axum::http::Error> {
    Request::post("/dataset/query")
        .version(version)
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/csv")
        .body(Body::from(query()))
}

#[tokio::test]
async fn an_http10_answer_is_sent_whole_and_its_memory_released() -> Result<(), Box<dyn Error>> {
    let store = shared_store()?;
    let app = test_app_with_store(
        store.clone(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    let before = held(&store);
    let response = app.oneshot(query_request(Version::HTTP_10)?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_LENGTH),
        Some(&WHOLE.into())
    );
    // All of it held before its first byte goes out, in the request's envelope.
    assert!(held(&store) >= before + WHOLE, "{} held", held(&store));
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.len(), WHOLE);
    assert_eq!(held(&store), before);
    Ok(())
}

#[tokio::test]
async fn an_http10_answer_past_its_envelope_is_refused_and_its_memory_released()
-> Result<(), Box<dyn Error>> {
    let store = shared_store()?;
    let mut policy = PolicyConfig::default();
    policy.limits.max_query_memory_bytes = 1 << 20;
    let app = test_app_with_store(store.clone(), policy, ReasonerConfig::default())?;
    let before = held(&store);
    let response = app.oneshot(query_request(Version::HTTP_10)?).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let problem: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    assert_eq!(
        problem["type"],
        "https://nrese.dev/problems/payload-too-large"
    );
    assert_eq!(
        problem["detail"],
        "This answer is larger than the 1 MiB an HTTP/1.0 request can be answered with: \
         HTTP/1.0 has no way to mark a streamed answer as cut, so the server sends it only \
         whole. Ask over HTTP/1.1 or HTTP/2, which stream it. Behind a reverse proxy, set the \
         proxy's upstream HTTP version to 1.1 (for nginx: `proxy_http_version 1.1;`)."
    );
    released_to(&store, before).await;
    Ok(())
}

/// A store whose queries share `total` bytes, and its app.
fn sharing(total: usize) -> Result<(StoreService, axum::Router), Box<dyn Error>> {
    let store = StoreService::new(StoreConfig {
        total_query_memory_bytes: total,
        ..StoreConfig::in_memory()
    })?;
    let app = test_app_with_store(
        store.clone(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    Ok((store, app))
}

/// The status and `Retry-After` of the HTTP/1.0 query's answer, its body read.
async fn refusal(app: &axum::Router) -> Result<(StatusCode, Option<HeaderValue>), Box<dyn Error>> {
    let response = app
        .clone()
        .oneshot(query_request(Version::HTTP_10)?)
        .await?;
    let status = response.status();
    let retry = response.headers().get(header::RETRY_AFTER).cloned();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    eprintln!(
        "{status}; retry-after {retry:?}; {}",
        String::from_utf8_lossy(&body)
    );
    Ok((status, retry))
}

/// The HTTP/1.0 query answered whole, as the same request is once there is room.
async fn answered(app: &axum::Router) -> Result<(), Box<dyn Error>> {
    let response = app
        .clone()
        .oneshot(query_request(Version::HTTP_10)?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX).await?.len(),
        WHOLE
    );
    Ok(())
}

/// An HTTP/1.0 query whose result table (256 KiB) finds no room in the memory all queries
/// share because others hold it (all but 128 KiB of 8 MiB), refused before any of its
/// answer is held: 503 without `Retry-After`, as every memory refusal is. Once the others
/// let go, the same request is answered.
#[tokio::test]
async fn an_http10_query_refused_early_while_others_hold_the_shared_memory_is_not_told_when_to_retry()
-> Result<(), Box<dyn Error>> {
    let (store, app) = sharing(8 << 20)?;
    let others = store.envelope(None);
    others.charge((8 << 20) - (128 << 10) - held(&store))?;
    let before = held(&store);
    assert_eq!(
        refusal(&app).await?,
        (StatusCode::SERVICE_UNAVAILABLE, None)
    );
    released_to(&store, before).await;
    drop(others);
    answered(&app).await
}

/// Readers of the access policy: `one` reads `urn:graph:g1` and the default graph only.
const READERS: &str = r#"
default = "deny"
inferred = "hidden"

[[role]]
name = "one"
read = ["urn:graph:g1"]
default_graph = "read"
"#;

/// What the HTTP/1.0 query told the requester with `roles`: its status, its `Retry-After`,
/// and its problem's `type`, `title` and `detail` (not its request id).
async fn told(
    app: &axum::Router,
    roles: &[&str],
) -> Result<(StatusCode, Option<HeaderValue>, serde_json::Value), Box<dyn Error>> {
    let request = Request::post("/dataset/query")
        .version(Version::HTTP_10)
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/csv")
        .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)))
        .body(Body::from(query()))?;
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let retry = response.headers().get(header::RETRY_AFTER).cloned();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    eprintln!("{roles:?}: {status}; retry-after {retry:?}; {problem}");
    let problem = serde_json::json!({
        "type": problem["type"],
        "title": problem["title"],
        "detail": problem["detail"],
    });
    Ok((status, retry, problem))
}

/// An HTTP/1.0 answer held whole past the memory all queries share (others hold all but
/// 1 MiB of 8 MiB; the answer is 2 MiB) is refused with 503 and no `Retry-After`, to a reader
/// of every graph and to a reader of some, as every memory refusal is. The reader of some
/// graphs is told the same whether or not work it can't see holds 256 KiB more of that
/// memory: nothing it is told depends on hidden work.
#[tokio::test]
async fn an_http10_answer_past_the_shared_memory_is_told_alike_and_not_when_to_retry()
-> Result<(), Box<dyn Error>> {
    let store = StoreService::new(StoreConfig {
        total_query_memory_bytes: 8 << 20,
        ..StoreConfig::in_memory()
    })?;
    let app = test_app_with_store(
        store.clone(),
        policy_of(READERS)?,
        ReasonerConfig::default(),
    )?;
    let others = store.envelope(None);
    others.charge((7 << 20) - held(&store))?;
    let before = held(&store);
    let reader = ["nrese.read", "one"];
    let alone = told(&app, &reader).await?;
    released_to(&store, before).await;
    let hidden = store.envelope(None);
    hidden.charge(256 << 10)?;
    let beside_hidden = told(&app, &reader).await?;
    let every = told(&app, &["nrese.admin"]).await?;
    for (status, retry, _) in [&alone, &beside_hidden, &every] {
        assert_eq!(*status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(*retry, None);
    }
    assert_eq!(
        alone.2, beside_hidden.2,
        "the reader is told of work it can't see"
    );
    Ok(())
}

/// The memory all queries share in the table of a reader of some graphs' refusals.
const SHARED: usize = 8 << 20;

/// A status and its problem's `type`, `title` and `detail` (not its request id).
type Told = (StatusCode, serde_json::Value);

/// What `request` is told.
async fn told_to(app: &axum::Router, request: Request<Body>) -> Result<Told, Box<dyn Error>> {
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    eprintln!("{status}: {}", String::from_utf8_lossy(&body));
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let problem = serde_json::json!({
        "type": problem["type"],
        "title": problem["title"],
        "detail": problem["detail"],
    });
    Ok((status, problem))
}

/// What `request` is told alone, then while work its reader can't see holds all but `room`
/// of the memory all queries share.
async fn alone_and_beside_hidden(
    store: &StoreService,
    app: &axum::Router,
    request: &dyn Fn() -> Result<Request<Body>, axum::http::Error>,
    room: usize,
) -> Result<(Told, Told), Box<dyn Error>> {
    let before = held(store);
    let alone = told_to(app, request()?).await?;
    released_to(store, before).await;
    let hidden = store.envelope(None);
    hidden.charge(SHARED - room - held(store))?;
    let beside = told_to(app, request()?).await?;
    drop(hidden);
    released_to(store, before).await;
    Ok((alone, beside))
}

/// The HTTP/1.0 query (`cut_body_wire_tests::query`: a 256 KiB result table, a 2 MiB answer)
/// of the requester with `roles`.
fn query_as<'a>(
    roles: &'a [&'a str],
) -> impl Fn() -> Result<Request<Body>, axum::http::Error> + 'a {
    move || {
        Request::post("/dataset/query")
            .version(Version::HTTP_10)
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "text/csv")
            .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)))
            .body(Body::from(query()))
    }
}

/// An HTTP/1.0 `GET` of `uri` by the requester with `roles`.
fn get_as<'a>(
    uri: &'a str,
    roles: &'a [&'a str],
) -> impl Fn() -> Result<Request<Body>, axum::http::Error> + 'a {
    move || {
        Request::get(uri)
            .version(Version::HTTP_10)
            .header(header::ACCEPT, "application/n-triples")
            .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)))
            .body(Body::empty())
    }
}

/// `policy` with a query memory of `bytes` (`budgets.query_memory`).
fn limited(policy: &str, bytes: usize) -> Result<PolicyConfig, Box<dyn Error>> {
    let mut policy = policy_of(policy)?;
    policy.limits.max_query_memory_bytes = bytes;
    Ok(policy)
}

/// A store whose queries share [`SHARED`], with the public and the secret graph of
/// [`SUPPORTED`]'s readers, reasoned over under OWL 2 RL: a public reader's view of what
/// its graphs support is built over the secret graph's 10,000 statements.
async fn supported_store() -> Result<StoreService, Box<dyn Error>> {
    let store = StoreService::new(StoreConfig {
        total_query_memory_bytes: SHARED,
        ..StoreConfig::in_memory()
    })?;
    let setup = test_app_with_store(
        store.clone(),
        policy_of(SUPPORTED)?,
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    let secret: String = (0..10_000)
        .map(|i| format!("<urn:hidden{i}> <urn:p> {i} . "))
        .collect();
    let insert = Request::post("/dataset/update")
        .header(header::CONTENT_TYPE, "application/sparql-update")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token(&["nrese.admin"])),
        )
        .body(Body::from(format!(
            "INSERT DATA {{ GRAPH <urn:g:public> {{ <urn:visible> <urn:p> <urn:o> . \
             <urn:p> <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <urn:q> }} \
             GRAPH <urn:g:secret> {{ {secret} }} }}"
        )))?;
    let (status, body) = answer(&setup, insert).await?;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    Ok(store)
}

/// Each site where a reader of some graphs' HTTP/1.0 answer, held whole, is refused for
/// memory, on its real route, alone and while work it can't see holds all but `room` of the
/// memory all queries share (which then refuses first): one status, 503, and one text at
/// every site, whichever limit refused it, so nothing it is told depends on hidden work.
/// The sites:
/// - a query's evaluation, its 256 KiB result table past its own 128 KiB;
/// - a query's answer held whole, 2 MiB past its own 1 MiB;
/// - a graph read's view, built over a secret graph, past its own 64 KiB;
/// - an RDF4J statements read's view, the same.
///
/// A reader of every graph keeps the distinction where it has one (the query's rows): its
/// own limit's 413, the query's or the HTTP/1.0 answer's, alone; the shared memory's 503
/// beside the others.
#[tokio::test]
async fn a_restricted_readers_held_answer_is_told_one_memory_refusal_at_every_site()
-> Result<(), Box<dyn Error>> {
    const ONE: &[&str] = &["nrese.read", "one"];
    const PUBLIC: &[&str] = &["nrese.read", "public"];
    const EVERY: &[&str] = &["nrese.admin"];
    let queries = StoreService::new(StoreConfig {
        total_query_memory_bytes: SHARED,
        ..StoreConfig::in_memory()
    })?;
    let app = |policy| test_app_with_store(queries.clone(), policy, ReasonerConfig::default());
    let evaluation = app(limited(READERS, 128 << 10)?)?;
    let holding = app(limited(READERS, 1 << 20)?)?;
    let graphs = supported_store().await?;
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl);
    let views = test_app_with_store(graphs.clone(), limited(SUPPORTED, 64 << 10)?, reasoner)?;
    let queried = query_as(ONE);
    let graph = get_as("/dataset/data?graph=urn:g:public", PUBLIC);
    let statements = get_as(
        "/repositories/nrese/statements?context=%3Curn%3Ag%3Apublic%3E",
        PUBLIC,
    );
    type Ask<'a> = &'a dyn Fn() -> Result<Request<Body>, axum::http::Error>;
    let rows: [(&str, &StoreService, &axum::Router, Ask<'_>, usize); 4] = [
        (
            "a query's evaluation",
            &queries,
            &evaluation,
            &queried,
            32 << 10,
        ),
        (
            "a query's held answer",
            &queries,
            &holding,
            &queried,
            512 << 10,
        ),
        ("a graph read's view", &graphs, &views, &graph, 32 << 10),
        (
            "a statements read's view",
            &graphs,
            &views,
            &statements,
            32 << 10,
        ),
    ];
    let mut texts = Vec::new();
    for (site, store, app, request, room) in rows {
        let (alone, beside) = alone_and_beside_hidden(store, app, request, room).await?;
        assert_eq!(
            alone.0,
            StatusCode::SERVICE_UNAVAILABLE,
            "{site}: {alone:?}"
        );
        assert_eq!(
            alone, beside,
            "{site}: the reader is told of work it can't see"
        );
        texts.push(alone.1);
    }
    assert!(
        texts.windows(2).all(|pair| pair[0] == pair[1]),
        "the sites tell different texts: {texts:?}"
    );

    let (alone, beside) =
        alone_and_beside_hidden(&queries, &evaluation, &query_as(EVERY), 32 << 10).await?;
    assert_eq!(alone.0, StatusCode::PAYLOAD_TOO_LARGE, "{alone:?}");
    let own = alone.1["detail"].as_str().unwrap_or_default();
    assert!(own.contains("budgets.query_memory"), "{own}");
    assert_eq!(beside.0, StatusCode::SERVICE_UNAVAILABLE, "{beside:?}");
    let (alone, beside) =
        alone_and_beside_hidden(&queries, &holding, &query_as(EVERY), 512 << 10).await?;
    assert_eq!(alone.0, StatusCode::PAYLOAD_TOO_LARGE, "{alone:?}");
    let own = alone.1["detail"].as_str().unwrap_or_default();
    assert!(
        own.starts_with("This answer is larger than the 1 MiB"),
        "{own}"
    );
    assert_eq!(beside.0, StatusCode::SERVICE_UNAVAILABLE, "{beside:?}");
    Ok(())
}

/// An HTTP/1.0 query whose own work can't fit the memory all queries share even with nothing
/// else held (64 KiB; its result table needs 256 KiB) is refused for that memory without
/// being told to retry: 503, no `Retry-After`. Trying again couldn't help.
#[tokio::test]
async fn an_http10_query_that_cannot_fit_the_shared_memory_alone_is_not_told_to_retry()
-> Result<(), Box<dyn Error>> {
    let (store, app) = sharing(64 << 10)?;
    let before = held(&store);
    assert_eq!(
        refusal(&app).await?,
        (StatusCode::SERVICE_UNAVAILABLE, None)
    );
    released_to(&store, before).await;
    Ok(())
}

/// A client that leaves while its HTTP/1.0 answer is being made stops the request: hyper
/// drops it once the connection reads the client's end, which cancels the query and
/// releases its envelope. Pins, for HTTP/1.0, what the hold's own exit relies on.
#[tokio::test]
async fn an_http10_client_that_leaves_stops_its_request() -> Result<(), Box<dyn Error>> {
    let endpoint = HeldEndpoint::bind().await?;
    let store = shared_store()?;
    store.set_service_client(endpoint.client()?);
    let app = test_app_with_store(
        store.clone(),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let before = held(&store);
    let query = format!(
        "SELECT * WHERE {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        endpoint.url()
    );
    let request = format!(
        "POST /dataset/query HTTP/1.0\r\nHost: {addr}\r\n\
         Content-Type: application/sparql-query\r\nAccept: text/csv\r\n\
         Content-Length: {}\r\n\r\n{query}",
        query.len()
    );
    let mut client = TcpStream::connect(addr).await?;
    client.write_all(request.as_bytes()).await?;
    let mut answer = tokio::spawn(async move {
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.map(|_| response)
    });
    let call = endpoint.call(&mut answer).await?;
    // The client leaves: its connection closes with the task that holds it.
    answer.abort();
    call.closed().await?;
    released_to(&store, before).await;
    server.abort();
    Ok(())
}

fn admin_policy() -> PolicyConfig {
    PolicyConfig {
        auth: AuthConfig::BearerStatic(StaticBearerConfig {
            read_token: None,
            admin_token: "admin".to_owned(),
        }),
        ..PolicyConfig::default()
    }
}

/// `uri` asked over HTTP/1.1 and over HTTP/1.0: the same answer, over HTTP/1.0 with its
/// length.
async fn the_same_answer_whole(uri: &str) -> Result<(), Box<dyn Error>> {
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(minimal_fixture_path()),
        admin_policy(),
        ReasonerConfig::default(),
    )?;
    let get = |version| {
        Request::get(uri)
            .version(version)
            .header(header::AUTHORIZATION, "Bearer admin")
            .body(Body::empty())
    };
    let streamed = app.clone().oneshot(get(Version::HTTP_11)?).await?;
    let whole = app.oneshot(get(Version::HTTP_10)?).await?;
    assert_eq!(streamed.status(), StatusCode::OK, "{uri}");
    assert_eq!(whole.status(), StatusCode::OK, "{uri}");
    let length = whole.headers().get(header::CONTENT_LENGTH).cloned();
    let streamed = to_bytes(streamed.into_body(), usize::MAX).await?;
    let whole = to_bytes(whole.into_body(), usize::MAX).await?;
    assert!(!whole.is_empty(), "{uri}");
    assert_eq!(length, Some(whole.len().into()), "{uri}");
    assert_eq!(whole, streamed, "{uri}");
    Ok(())
}

#[tokio::test]
async fn an_http10_graph_store_read_is_sent_whole() -> Result<(), Box<dyn Error>> {
    the_same_answer_whole("/dataset/data?default").await
}

#[tokio::test]
async fn an_http10_rdf4j_statements_read_is_sent_whole() -> Result<(), Box<dyn Error>> {
    the_same_answer_whole("/repositories/nrese/statements").await
}

#[tokio::test]
async fn an_http10_export_is_sent_whole() -> Result<(), Box<dyn Error>> {
    the_same_answer_whole("/api/v1/repositories/nrese/backup").await
}

/// The status and body of `request`'s answer.
async fn answer(
    app: &axum::Router,
    request: Request<Body>,
) -> Result<(StatusCode, String), Box<dyn Error>> {
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Over HTTP/1.0 a read's working memory draws on the envelope its answer is held in, as a
/// query run's does: a legacy RDF4J transaction's read of one statement, whose replay of the
/// transaction's 20,000 added statements needs more than the request's 64 KiB
/// (`budgets.query_memory`), is refused for its memory, 413, though its answer is one line.
/// Streamed (HTTP/1.1), the read holds no answer and its replay draws on the queries'
/// memory: answered.
#[tokio::test]
async fn an_http10_transaction_read_replays_within_its_answers_envelope()
-> Result<(), Box<dyn Error>> {
    let mut policy = PolicyConfig::default();
    policy.limits.max_query_memory_bytes = 64 << 10;
    let app = test_app_with_store(shared_store()?, policy, ReasonerConfig::default())?;
    let begun = app
        .clone()
        .oneshot(Request::post("/repositories/nrese/transactions").body(Body::empty())?)
        .await?;
    assert_eq!(begun.status(), StatusCode::CREATED);
    let tx = begun.headers()[header::LOCATION].to_str()?.to_owned();
    let added: String = (0..20_000)
        .map(|i| format!("<urn:s{i}> <urn:p> \"{i}\" .\n"))
        .collect();
    let add = Request::put(format!("{tx}?action=ADD"))
        .header(header::CONTENT_TYPE, "application/n-triples")
        .body(Body::from(added))?;
    let (status, body) = answer(&app, add).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let read = |version| {
        Request::put(format!("{tx}?action=GET&subj=%3Curn%3As0%3E"))
            .version(version)
            .header(header::ACCEPT, "application/n-triples")
            .body(Body::empty())
    };
    // Whole first: a streamed read keeps the transaction's view for the reads after it.
    let (status, body) = answer(&app, read(Version::HTTP_10)?).await?;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert!(body.contains("budgets.query_memory"), "{body}");
    let (status, body) = answer(&app, read(Version::HTTP_11)?).await?;
    assert_eq!(
        (status, body.lines().count()),
        (StatusCode::OK, 1),
        "{body}"
    );
    Ok(())
}

/// Readers of the access policy: `public` reads `urn:g:public` and the default graph, and the
/// inferred statements its graphs support.
const SUPPORTED: &str = r#"
default = "deny"
inferred = "supported"

[[role]]
name = "public"
read = ["urn:g:public"]
default_graph = "read"
"#;

/// Over HTTP/1.0 a reader of some graphs' graph read builds its view within the envelope its
/// answer is held in: a read of the public graph (a few statements) whose view of what the
/// graphs support, built over a secret graph's 10,000 statements, needs more than the
/// request's 64 KiB (`budgets.query_memory`), is refused for its memory. Streamed
/// (HTTP/1.1), its view draws on the queries' memory: answered.
#[tokio::test]
async fn an_http10_graph_read_builds_its_view_within_its_answers_envelope()
-> Result<(), Box<dyn Error>> {
    let store = shared_store()?;
    let reasoner = || ReasonerConfig::for_mode(ReasoningMode::Owl2Rl);
    let setup = test_app_with_store(store.clone(), policy_of(SUPPORTED)?, reasoner())?;
    let secret: String = (0..10_000)
        .map(|i| format!("<urn:hidden{i}> <urn:p> {i} . "))
        .collect();
    let insert = Request::post("/dataset/update")
        .header(header::CONTENT_TYPE, "application/sparql-update")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token(&["nrese.admin"])),
        )
        .body(Body::from(format!(
            "INSERT DATA {{ GRAPH <urn:g:public> {{ <urn:visible> <urn:p> <urn:o> . \
             <urn:p> <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <urn:q> }} \
             GRAPH <urn:g:secret> {{ {secret} }} }}"
        )))?;
    let (status, body) = answer(&setup, insert).await?;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let mut policy = policy_of(SUPPORTED)?;
    policy.limits.max_query_memory_bytes = 64 << 10;
    let app = test_app_with_store(store, policy, reasoner())?;
    let read = |version| {
        Request::get("/dataset/data?graph=urn:g:public")
            .version(version)
            .header(header::ACCEPT, "application/n-triples")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", token(&["nrese.read", "public"])),
            )
            .body(Body::empty())
    };
    let (status, body) = answer(&app, read(Version::HTTP_10)?).await?;
    assert!(
        matches!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE | StatusCode::SERVICE_UNAVAILABLE
        ) && body.contains("memory"),
        "{status}: {body}"
    );
    let (status, body) = answer(&app, read(Version::HTTP_11)?).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(())
}

/// A primary's replication image is a file, its size known before it is sent: over HTTP/1.0
/// it goes out with its `Content-Length`, as on the wire, all of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_http10_replication_image_arrives_with_its_length() -> Result<(), Box<dyn Error>> {
    let (addr, server, _dir) = image_server().await?;
    let (head, body) = http1_image(addr, "HTTP/1.0", "GET").await?;
    server.abort();
    let length = head.split("\r\n").find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().to_owned())
    });
    assert!(!body.is_empty());
    assert_eq!(length, Some(body.len().to_string()), "{head}");
    Ok(())
}
