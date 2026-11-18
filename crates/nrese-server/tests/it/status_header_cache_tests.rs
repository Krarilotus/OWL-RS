//! The status header of an answer the result cache gives (`nrese-completeness`): a request
//! is told with the cache on what it is told with the cache off, request by request, for
//! every status kind (`complete`, sound only with its reasons, `unsound`, `not-certified`)
//! and across each input the header is made from (the status on the snapshot read, the
//! reader, whether the QL rewriting ran); and a repeated request is answered from the cache.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use jsonwebtoken::{EncodingKey, Header, encode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::AppState;
use nrese_server::auth::{AuthConfig, JwtBearerConfig};
use nrese_server::policy::PolicyConfig;
use nrese_store::{QlRewritingMode, StoreConfig};
use serde::Serialize;
use tower::util::ServiceExt;

use crate::support::{body_text, test_state_with_store_config};

/// An analyst reads the default graph and sees its inferences, not by support: where the
/// rewriting ran, it is told `not-certified`. An administrator is unrestricted.
const POLICY: &str = r#"
default = "deny"
inferred = "visible"

[[role]]
name = "analyst"
read = ["http://example.com/g/*"]
default_graph = "read"
"#;

const ADMIN: &[&str] = &["nrese.read", "nrese.admin"];
const ANALYST: &[&str] = &["nrese.read", "analyst"];

/// Who works for someone: answers through an existential under owl2-ql.
const QUERY: &str = "SELECT ?x WHERE { ?x <http://example.com/worksFor> ?y }";

#[derive(Serialize)]
struct Claims {
    exp: usize,
    roles: Vec<String>,
}

fn bearer(roles: &[&str]) -> String {
    let claims = Claims {
        exp: 4_102_444_800,
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
    };
    let key = EncodingKey::from_secret(b"test-secret");
    format!(
        "Bearer {}",
        encode(&Header::default(), &claims, &key).expect("jwt")
    )
}

/// A server under owl2-ql with the access policy, its result cache `cache_bytes` large.
fn server(cache_bytes: usize) -> Result<(axum::Router, AppState), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("access.toml");
    std::fs::write(&path, POLICY)?;
    let policy = PolicyConfig {
        auth: AuthConfig::BearerJwt(JwtBearerConfig {
            shared_secret: "test-secret".to_owned(),
            issuer: None,
            audience: None,
            read_role: "nrese.read".to_owned(),
            admin_role: "nrese.admin".to_owned(),
            leeway_seconds: 0,
        }),
        access: Some(Arc::new(nrese_server::access::load(&path)?)),
        ..PolicyConfig::default()
    };
    let state = test_state_with_store_config(
        StoreConfig {
            query_cache_bytes: cache_bytes,
            ..StoreConfig::default()
        },
        policy,
        ReasonerConfig::for_mode(ReasoningMode::Owl2Ql),
    )?;
    Ok((nrese_server::build_app(state.clone()), state))
}

async fn update(app: &axum::Router, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let request = Request::builder()
        .uri("/dataset/update")
        .method(Method::POST)
        .header("authorization", bearer(ADMIN))
        .header("content-type", "application/sparql-update")
        .body(Body::from(text.to_owned()))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "{text}");
    Ok(())
}

/// The status header and the body of `query` asked by `roles`.
async fn ask(
    app: &axum::Router,
    roles: &[&str],
    query: &str,
) -> Result<(Option<Vec<u8>>, String), Box<dyn std::error::Error>> {
    let request = Request::builder()
        .uri("/dataset/query")
        .method(Method::POST)
        .header("authorization", bearer(roles))
        .header("content-type", "application/sparql-query")
        .header("accept", "text/tab-separated-values")
        .body(Body::from(query.to_owned()))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK, "{query}");
    let header = response
        .headers()
        .get("nrese-completeness")
        .map(|value| value.as_bytes().to_vec());
    Ok((header, body_text(response).await?))
}

/// One step of the run: a request, or a change between requests.
enum Step {
    Ask(&'static str, &'static [&'static str], &'static str),
    Update(&'static str),
    Rewriting(QlRewritingMode),
}

/// Each request twice: what the first and the second were told, and, with the cache on,
/// whether the first stored the answer and the second was answered from it.
async fn run(
    cache_bytes: usize,
    steps: &[Step],
) -> Result<Vec<(&'static str, Vec<(Option<Vec<u8>>, String)>, bool)>, Box<dyn std::error::Error>> {
    let (app, state) = server(cache_bytes)?;
    let mut told = Vec::new();
    for step in steps {
        match step {
            Step::Update(text) => update(&app, text).await?,
            Step::Rewriting(mode) => state.store().set_ql_rewriting(*mode),
            Step::Ask(label, roles, query) => {
                let stats = || state.store().query_cache_stats();
                let before = stats();
                let first = ask(&app, roles, query).await?;
                let between = stats();
                let second = ask(&app, roles, query).await?;
                let after = stats();
                // The first stores (a commit before it drops the older snapshot's answers,
                // so their number needn't grow); the second computes, stores and refuses
                // nothing and is a hit.
                let work = |s: &nrese_sparql::ResultCacheStats| (s.stored, s.rejected, s.misses);
                let stored_then_hit = between.stored > before.stored
                    && between.answers > 0
                    && work(&after) == work(&between)
                    && after.answers == between.answers
                    && after.hits > between.hits;
                told.push((*label, vec![first, second], stored_then_hit));
            }
        }
    }
    Ok(told)
}

/// With the cache on, every request is told the header (and the answer) it is told with
/// the cache off, byte for byte: the same request asked again is answered from the cache
/// with the same status as when it was computed, for each status kind; and requests that
/// differ only in the reader, in whether the rewriting ran, or in the snapshot (a commit
/// between them) are each told their own.
#[tokio::test]
async fn an_answer_from_the_cache_says_the_status_its_request_is_told_without_it()
-> Result<(), Box<dyn std::error::Error>> {
    // The first row of an ordered answer: a missing row can make it wrong (`unsound`); an
    // unordered slice only drops rows.
    let limited = "SELECT ?x WHERE { ?x <http://example.com/worksFor> ?y } ORDER BY ?x LIMIT 1";
    let steps = [
        Step::Update(
            "PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {
                <http://example.com/Employee> rdfs:subClassOf [ a owl:Restriction ;
                    owl:onProperty <http://example.com/worksFor> ;
                    owl:someValuesFrom owl:Thing ] .
                <http://example.com/bob> a <http://example.com/Employee> .
             }",
        ),
        // The same query, reader and snapshot, the rewriting not run, then run.
        Step::Rewriting(QlRewritingMode::Off),
        Step::Ask("rewriting off", ADMIN, QUERY),
        Step::Rewriting(QlRewritingMode::Auto),
        Step::Ask("complete", ADMIN, QUERY),
        // The same query and snapshot, another reader.
        Step::Ask("not-certified", ANALYST, QUERY),
        // The same query and reader, another snapshot: a transitive worksFor meets the
        // existential.
        Step::Update(
            "INSERT DATA { <http://example.com/worksFor> a <http://www.w3.org/2002/07/owl#TransitiveProperty> }",
        ),
        Step::Ask("sound-only", ADMIN, QUERY),
        Step::Ask("unsound", ADMIN, limited),
        Step::Ask("not-certified after the commit", ANALYST, QUERY),
    ];
    let cached = run(DEFAULT_CACHE, &steps).await?;
    let uncached = run(0, &steps).await?;
    let text = |header: &Option<Vec<u8>>| {
        String::from_utf8_lossy(header.as_deref().unwrap_or(b"(none)")).into_owned()
    };
    for ((label, with, stored_then_hit), (_, without, _)) in cached.iter().zip(&uncached) {
        assert!(
            *stored_then_hit,
            "{label}: not stored by the first request and hit by the second"
        );
        for (i, ((header, body), (expected, expected_body))) in with.iter().zip(without).enumerate()
        {
            assert_eq!(
                (text(header), body),
                (text(expected), expected_body),
                "{label}, request {i}: told with the cache what isn't told without it"
            );
        }
    }
    // Each kind the server writes is among them.
    let first =
        |label: &str| text(&cached.iter().find(|(l, _, _)| *l == label).expect(label).1[0].0);
    assert_eq!(first("complete"), "complete; regime=owl2-ql");
    assert_eq!(first("not-certified"), "not-certified");
    for kind in ["sound-only", "unsound"] {
        let told = first(kind);
        let expected = format!("{kind}; regime=owl2-ql; reasons=\"ql: ");
        assert!(told.starts_with(&expected), "{kind}: told {told}");
    }
    assert_eq!(first("not-certified after the commit"), "not-certified");
    Ok(())
}

/// The store's default result cache.
const DEFAULT_CACHE: usize = nrese_store::DEFAULT_QUERY_CACHE_BYTES;
