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
default_graph = "read"
"#;
const READER: &[&str] = &["nrese.read", "one"];
const ADMIN: &[&str] = &["nrese.admin"];
/// What the reader reads: `ex:a ex:q ex:b` follows from it alone (`prp-spo1`).
const DATA: &str = "GRAPH :g1 { ex:p rdfs:subPropertyOf ex:q . ex:a ex:p ex:b }";
const ENTAILED: &str = "ASK { ex:a ex:q ex:b }";

/// The answer to `ask` and its `NRESE-Completeness`, as `roles` are told them.
async fn ask(app: &Router, roles: &[&str], ask: &str) -> TestResult<(bool, Option<String>)> {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/dataset/query")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "application/sparql-results+json")
        .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)))
        .body(Body::from(format!("{PREFIXES} {ask}")))?;
    let response = app.clone().oneshot(request).await?;
    let code = response.status();
    let status = response
        .headers()
        .get("nrese-completeness")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()?;
    let body = body_text(response).await?;
    assert_eq!(code, StatusCode::OK, "{ask}: {body}");
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let answer = json["boolean"].as_bool().ok_or("no boolean")?;
    Ok((answer, status))
}

/// Spec: docs/ops/config-reference.md (access policy): under `reasoner.equality =
/// "compact"` a reader by support sees no inferred statements, and
/// docs/spec/reasoning-semantics.md: `complete` says no entailed row is missing. So a reader
/// of `:g1` asking for `ex:a ex:q ex:b`, which `:g1` alone entails, under `owl2-rl` with the
/// QL rewriting `on`, is answered `false` and told `not-certified`, never `complete`. Under
/// `representatives` its view is its closure: `true`, `complete`. The reader of every graph
/// is answered `true` either way (the row is entailed).
#[tokio::test]
async fn a_reader_by_support_is_never_told_complete_for_a_view_without_its_closure() -> TestResult {
    let mut seen = Vec::new();
    for compact in [false, true] {
        let app = test_app_with_store_config(
            StoreConfig {
                equality_compact: compact,
                ql_rewriting: QlRewritingMode::On,
                ..StoreConfig::default()
            },
            policy_of(POLICY)?,
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
        )?;
        let insert = Request::builder()
            .method(Method::POST)
            .uri("/dataset/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .header(header::AUTHORIZATION, format!("Bearer {}", token(ADMIN)))
            .body(Body::from(format!("{PREFIXES} INSERT DATA {{ {DATA} }}")))?;
        assert_eq!(
            app.clone().oneshot(insert).await?.status(),
            StatusCode::NO_CONTENT
        );
        let (whole, _) = ask(&app, ADMIN, ENTAILED).await?;
        let (answer, status) = ask(&app, READER, ENTAILED).await?;
        seen.push((compact, whole, answer, status));
    }
    let complete = Some("complete; regime=owl2-rl".to_owned());
    let not_certified = Some("not-certified".to_owned());
    assert_eq!(
        seen,
        [
            (false, true, true, complete),
            (true, true, false, not_certified)
        ],
        "(compact, the reader of every graph's answer, the reader's answer, its status)"
    );
    Ok(())
}
