//! What a reader of some graphs is told of an answer read from its own view: never
//! `complete` where the view isn't its closure under a certificate, so that an entailed row
//! the view leaves out is never hidden behind `complete`.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::{QlRewritingMode, StoreConfig};
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::{body_text, test_app_with_store_config};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PREFIXES: &str = "PREFIX ex: <urn:ex:> PREFIX : <urn:graph:>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";
const POLICY: &str = r#"
default = "deny"
inferred = "supported"

[[role]]
name = "one"
read = ["urn:graph:g1"]
write = ["urn:graph:g1"]
default_graph = "read"
"#;
const READER: &[&str] = &["nrese.read", "one"];
const ADMIN: &[&str] = &["nrese.admin"];
/// What the reader reads: `ex:a ex:q ex:b` follows from it alone (`prp-spo1`).
const DATA: &str = "GRAPH :g1 { ex:p rdfs:subPropertyOf ex:q . ex:a ex:p ex:b }";
const ENTAILED: &str = "ASK { ex:a ex:q ex:b }";

/// How the reader reads [`ENTAILED`].
#[derive(Debug, Clone, Copy)]
enum Route {
    Query,
    /// Inside an RDF4J transaction with no operation pending.
    Transaction,
    /// Inside an RDF4J transaction after it inserted an unrelated statement into `:g1`.
    Pending,
    /// `explain=true`: its status and whether it shows the QL rewriting's report.
    Explain,
}

/// What a reader is told: the answer, the status's first word (`complete`, `sound-only`,
/// `unsound`, `not-certified`) and, from EXPLAIN, whether the QL report is shown.
#[derive(Debug, PartialEq)]
struct Told {
    answer: bool,
    status: String,
    report: Option<bool>,
}

fn told(answer: bool, status: &str, report: Option<bool>) -> Told {
    Told {
        answer,
        status: status.to_owned(),
        report,
    }
}

async fn send(
    app: &Router,
    roles: &[&str],
    method: Method,
    uri: &str,
    body: String,
) -> TestResult<axum::response::Response> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "application/sparql-results+json")
        .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)))
        .body(Body::from(body))?;
    Ok(app.clone().oneshot(request).await?)
}

/// The JSON body of `response` (200 asserted) and its `NRESE-Completeness`'s first word.
async fn read(response: axum::response::Response) -> TestResult<(serde_json::Value, String)> {
    let code = response.status();
    let header = response.headers().get("nrese-completeness").cloned();
    let body = body_text(response).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    let status = match header {
        Some(value) => value
            .to_str()?
            .split(';')
            .next()
            .unwrap_or_default()
            .to_owned(),
        None => String::new(),
    };
    Ok((serde_json::from_str(&body)?, status))
}

fn boolean(json: &serde_json::Value) -> TestResult<bool> {
    Ok(json["boolean"].as_bool().ok_or("no boolean")?)
}

/// An RDF4J transaction begun by `roles`, `update` pending in it if any; its location.
async fn transaction(app: &Router, roles: &[&str], update: Option<&str>) -> TestResult<String> {
    let begin = Request::builder()
        .method(Method::POST)
        .uri("/repositories/nrese/transactions")
        .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)))
        .body(Body::empty())?;
    let begin = app.clone().oneshot(begin).await?;
    assert_eq!(begin.status(), StatusCode::CREATED);
    let tx = begin.headers()[header::LOCATION].to_str()?.to_owned();
    if let Some(update) = update {
        let form = serde_urlencoded::to_string([("update", format!("{PREFIXES} {update}"))])?;
        let request = Request::builder()
            .method(Method::PUT)
            .uri(format!("{tx}?action=UPDATE"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)))
            .body(Body::from(form))?;
        let response = app.clone().oneshot(request).await?;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            body_text(response).await?
        );
    }
    Ok(tx)
}

/// What `roles` are told of [`ENTAILED`] by `route`.
async fn ask(app: &Router, roles: &[&str], route: Route) -> TestResult<Told> {
    let query = format!("{PREFIXES} {ENTAILED}");
    let in_transaction = |tx: String| -> TestResult<String> {
        Ok(format!(
            "{tx}?action=QUERY&{}",
            serde_urlencoded::to_string([("query", &query)])?
        ))
    };
    let (uri, method, body) = match route {
        Route::Query | Route::Explain => ("/dataset/query".to_owned(), Method::POST, query.clone()),
        Route::Transaction => (
            in_transaction(transaction(app, roles, None).await?)?,
            Method::PUT,
            String::new(),
        ),
        Route::Pending => {
            let insert = "INSERT DATA { GRAPH :g1 { ex:c ex:r ex:d } }";
            let tx = transaction(app, roles, Some(insert)).await?;
            (in_transaction(tx)?, Method::PUT, String::new())
        }
    };
    let (json, status) = read(send(app, roles, method, &uri, body).await?).await?;
    let answer = boolean(&json)?;
    if !matches!(route, Route::Explain) {
        return Ok(told(answer, &status, None));
    }
    let explain = send(
        app,
        roles,
        Method::POST,
        "/dataset/query?explain=true",
        query,
    )
    .await?;
    let (explanation, _) = read(explain).await?;
    let status = explanation["completeness"]["status"]
        .as_str()
        .unwrap_or_default();
    Ok(told(answer, status, Some(!explanation["ql"].is_null())))
}

/// Spec: docs/ops/config-reference.md (access policy): under `reasoner.equality =
/// "compact"` a reader by support sees no inferred statements; docs/spec/reasoning-semantics.md:
/// `complete` says no entailed row is missing, and a regime's status speaks of the closure the
/// reader reads. So a reader of `:g1` asking for `ex:a ex:q ex:b`, which `:g1` alone entails,
/// is answered `false` under compact and told `not-certified`, on every route, with no QL
/// report; never `complete` or a regime's `sound-only`. Under `representatives` its view is its
/// closure: `true`, with the status a reader of every graph gets (the control). The reader of
/// every graph is answered `true` either way (the row is entailed).
#[tokio::test]
async fn a_reader_by_support_is_never_told_complete_for_a_view_without_its_closure() -> TestResult {
    use QlRewritingMode::{Auto, Off, On};
    use ReasoningMode::{Owl2Ql, Owl2Rl};
    // Each family: what the reader is told under `representatives`, then under `compact`.
    let families = [
        (
            "owl2-rl ql=on",
            Owl2Rl,
            On,
            Route::Query,
            told(true, "complete", None),
        ),
        (
            "owl2-rl ql=off",
            Owl2Rl,
            Off,
            Route::Query,
            told(true, "sound-only", None),
        ),
        (
            "owl2-ql",
            Owl2Ql,
            Auto,
            Route::Query,
            told(true, "complete", None),
        ),
        (
            "transaction",
            Owl2Rl,
            On,
            Route::Transaction,
            told(true, "complete", None),
        ),
        (
            "pending",
            Owl2Rl,
            On,
            Route::Pending,
            told(true, "sound-only", None),
        ),
        (
            "explain",
            Owl2Rl,
            On,
            Route::Explain,
            told(true, "complete", Some(true)),
        ),
    ];
    let mut wrong = Vec::new();
    for (name, mode, ql, route, control) in families {
        let without = told(
            false,
            "not-certified",
            matches!(route, Route::Explain).then_some(false),
        );
        for (compact, want) in [(false, control), (true, without)] {
            let app = test_app_with_store_config(
                StoreConfig {
                    equality_compact: compact,
                    ql_rewriting: ql,
                    ..StoreConfig::default()
                },
                policy_of(POLICY)?,
                ReasonerConfig::for_mode(mode),
            )?;
            let insert = format!("{PREFIXES} INSERT DATA {{ {DATA} }}");
            let inserted = send(&app, ADMIN, Method::POST, "/dataset/update", insert).await?;
            assert_eq!(inserted.status(), StatusCode::NO_CONTENT);
            let whole = ask(&app, ADMIN, Route::Query).await?.answer;
            let got = ask(&app, READER, route).await?;
            if !whole || got != want {
                wrong.push(format!(
                    "{name} compact={compact}: every graph {whole}, reader {got:?}, want {want:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
    Ok(())
}
