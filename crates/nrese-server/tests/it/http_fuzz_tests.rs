//! HTTP fuzzing (G7): seeded random requests against every route the router serves. Each
//! answer must come in time, never be a server error (500 or 502), a JSON body must be
//! JSON, and an error must be a problem document with its status and request id (one
//! envelope, whoever answered); a handler that panics fails the test where it runs. Afterwards the server
//! still answers `/readyz` and a query.
//!
//! The routes are read from the router's source (`src/http/routes.rs`), templates filled
//! with plausible and broken values. Methods, headers, query strings and bodies come from
//! small seeds (SPARQL, RDF in each syntax, JSON, forms) and their mutations. No seed
//! reaches the network: no `SERVICE`, no `LOAD`, no URLs to import.
//!
//! `NRESE_FUZZ_CASES` (default 1,500) and `NRESE_FUZZ_SEED` (`scripts/fuzz-campaign.sh`
//! varies it) replay a run.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode};
use tower::util::ServiceExt;

use crate::support::test_app_with_store_config;

const ROUTES: &str = include_str!("../../src/http/routes.rs");

/// The paths the router serves (templates such as `/repositories/{id}`), each with the
/// methods it serves.
fn paths() -> Vec<(String, Vec<Method>)> {
    let nested_from = ROUTES
        .find("fn repository_routes")
        .expect("repository routes");
    let nested_to = ROUTES.find("const DEPRECATED_SINCE").expect("their end");
    let mut out: Vec<(String, Vec<Method>)> = Vec::new();
    for (at, _) in ROUTES.match_indices(".route(") {
        let rest = &ROUTES[at + ".route(".len()..];
        let Some(literal) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        let path = &literal[..literal.find('"').expect("closed literal")];
        // The route's handlers: up to the next route or nest.
        let chunk = rest.split(".route(").next().unwrap_or(rest);
        let chunk = chunk.split(".nest(").next().unwrap_or(chunk);
        let mut methods = Vec::new();
        for (name, method) in [
            ("get(", Method::GET),
            ("post(", Method::POST),
            ("put(", Method::PUT),
            ("patch(", Method::PATCH),
            ("delete(", Method::DELETE),
        ] {
            let called = chunk.match_indices(name).any(|(i, _)| {
                i == 0 || {
                    let before = chunk.as_bytes()[i - 1];
                    !before.is_ascii_alphanumeric() && before != b'_'
                }
            });
            if called {
                methods.push(method);
            }
        }
        let path = if (nested_from..nested_to).contains(&at) {
            format!("/api/v1/repositories/{{id}}{path}")
        } else {
            path.to_owned()
        };
        out.push((path, methods));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// SplitMix64: small, seeded, the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const QUERIES: &[&str] = &[
    "SELECT * WHERE { ?s ?p ?o } LIMIT 5",
    "ASK { ?s ?p ?o }",
    "CONSTRUCT WHERE { ?s ?p ?o }",
    "DESCRIBE <http://example.org/a>",
    "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
    "PREFIX e: <http://example.org/> SELECT ?x WHERE { ?x e:p+ ?y . FILTER(?y > 1) } ORDER BY DESC(?x) LIMIT 3",
    "SELECT ?g (SAMPLE(?s) AS ?x) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g",
    "SELECT * WHERE { << ?s ?p ?o >> ?q ?r }",
    "SELECT * WHERE { ?s ?p \"x\"@en . OPTIONAL { ?s ?q ?o } MINUS { ?s a ?c } }",
];

const UPDATES: &[&str] = &[
    "INSERT DATA { <http://example.org/a> <http://example.org/p> 1 }",
    "INSERT DATA { GRAPH <http://example.org/g> { <http://example.org/a> <http://example.org/p> \"v\"@en } }",
    "DELETE WHERE { ?s <http://example.org/p> ?o }",
    "PREFIX e: <http://example.org/> DELETE { ?s e:p ?o } INSERT { ?s e:q ?o } WHERE { ?s e:p ?o }",
    "CLEAR GRAPH <http://example.org/g>",
    "DROP SILENT GRAPH <http://example.org/none>",
    "INSERT DATA { <http://example.org/a> a <http://www.w3.org/2002/07/owl#Class> }",
];

const RDF: &[(&str, &str)] = &[
    (
        "text/turtle",
        "@prefix e: <http://example.org/> . e:a e:p e:b ; e:q \"x\"@en, 2 .",
    ),
    (
        "application/n-triples",
        "<http://example.org/a> <http://example.org/p> <http://example.org/b> .\n",
    ),
    (
        "application/n-quads",
        "<http://example.org/a> <http://example.org/p> \"1\" <http://example.org/g> .\n",
    ),
    (
        "application/trig",
        "@prefix e: <http://example.org/> . e:g { e:a e:p e:b }",
    ),
    (
        "application/ld+json",
        r#"{"@id": "http://example.org/a", "http://example.org/p": [{"@value": "x"}]}"#,
    ),
    (
        "application/rdf+xml",
        r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="http://example.org/"><rdf:Description rdf:about="http://example.org/a"><e:p>1</e:p></rdf:Description></rdf:RDF>"#,
    ),
];

const JSON: &[&str] = &[
    "{}",
    "[]",
    "null",
    r#"{"name": "x", "title": "y", "ruleset": "owl2-rl"}"#,
    r#"{"query": "SELECT * WHERE { ?s ?p ?o }", "limit": 3}"#,
    r#"{"prefix": "e", "namespace": "http://example.org/"}"#,
    r#"{"enabled": true, "mode": "strict", "users": [{"name": "a", "roles": ["reader"]}]}"#,
    r#"{"format": "text/turtle", "data": "<http://example.org/a> <http://example.org/p> 1 ."}"#,
];

const CONTENT_TYPES: &[&str] = &[
    "application/sparql-query",
    "application/sparql-update",
    "application/x-www-form-urlencoded",
    "application/json",
    "text/turtle",
    "application/n-triples",
    "application/n-quads",
    "application/trig",
    "application/ld+json",
    "application/rdf+xml",
    "text/plain",
    "multipart/form-data; boundary=x",
    "application/octet-stream",
    "text/turtle; charset=latin1",
    "application/sparql-query; charset=utf-16",
    "x/y",
    "",
];

const ACCEPTS: &[&str] = &[
    "*/*",
    "application/sparql-results+json",
    "application/sparql-results+xml",
    "text/csv",
    "text/tab-separated-values",
    "text/turtle",
    "application/n-triples",
    "application/ld+json",
    "application/rdf+xml",
    "application/json",
    "text/html",
    "application/x-binary-rdf-results-table",
    "image/png",
    "application/sparql-results+json;q=0.1, text/csv;q=0.9",
    "text/*;q=0",
    ";;;",
];

const PARAMETERS: &[&str] = &[
    "query",
    "update",
    "graph",
    "default",
    "default-graph-uri",
    "named-graph-uri",
    "using-graph-uri",
    "timeout",
    "limit",
    "offset",
    "infer",
    "reasoning",
    "explain",
    "format",
    "context",
    "subj",
    "pred",
    "obj",
    "baseURI",
    "prefix",
    "q",
    "name",
    "space",
    "revision",
];

/// A value for a path segment.
fn segment(rng: &mut Rng) -> String {
    match rng.below(14) {
        9..=13 | 0..=2 => "nrese".into(),
        3 => "default".into(),
        4 => format!("r{}", rng.below(1000)),
        5 => "%00".into(),
        6 => "..".into(),
        7 => "a".repeat(1 + rng.below(3000)),
        _ => percent(&random_text(rng, 12)),
    }
}

fn random_text(rng: &mut Rng, max: usize) -> String {
    (0..rng.below(max + 1))
        .map(|_| char::from_u32(rng.below(0x2FF) as u32 + 1).unwrap_or('x'))
        .collect()
}

fn percent(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Bytes changed at random: flipped, cut, repeated, spliced with another seed.
fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut out = seed.to_vec();
    for _ in 0..rng.below(4) {
        if out.is_empty() {
            out.push(rng.next() as u8);
            continue;
        }
        let at = rng.below(out.len());
        match rng.below(6) {
            0 => out[at] = rng.next() as u8,
            1 => out.truncate(at),
            2 => {
                let end = (at + 1 + rng.below(16)).min(out.len());
                let chunk = out[at..end].to_vec();
                out.splice(at..at, chunk);
            }
            3 => {
                out.remove(at);
            }
            4 => {
                let other = rng.pick(QUERIES).as_bytes();
                let cut = rng.below(other.len().max(1));
                out.splice(at..at, other[cut..].iter().copied());
            }
            _ => out.insert(at, *rng.pick(b"{}()<>\"'\\@#?:;,. \n\x00\xff")),
        }
    }
    out.truncate(64 * 1024);
    out
}

/// One random request.
fn request(rng: &mut Rng, paths: &[(String, Vec<Method>)]) -> Request<Body> {
    let (template, served) = rng.pick(paths).clone();
    let mut path = String::new();
    for part in template.split('/').skip(1) {
        path.push('/');
        if part.starts_with('{') {
            if part.starts_with("{*") {
                path.push_str(&format!("{}/{}", segment(rng), segment(rng)));
            } else {
                path.push_str(&segment(rng));
            }
        } else {
            path.push_str(part);
        }
    }
    if rng.chance(3) {
        path.push_str(&percent(&random_text(rng, 8)));
    }
    let mut query = Vec::new();
    for _ in 0..rng.below(4) {
        let key = if rng.chance(85) {
            rng.pick(PARAMETERS).to_string()
        } else {
            random_text(rng, 6)
        };
        let value: Vec<u8> = match rng.below(5) {
            0 => {
                let seed = *rng.pick(QUERIES);
                mutate(rng, seed.as_bytes())
            }
            1 => rng.pick(QUERIES).as_bytes().to_vec(),
            2 => format!("{}", rng.next() as i64 % 100_000).into_bytes(),
            3 => b"http://example.org/g".to_vec(),
            _ => random_text(rng, 20).into_bytes(),
        };
        query.push(format!(
            "{}={}",
            percent(&key),
            percent(&String::from_utf8_lossy(&value))
        ));
    }
    let uri = if query.is_empty() {
        path
    } else {
        format!("{path}?{}", query.join("&"))
    };
    let method = match rng.below(10) {
        _ if !served.is_empty() && rng.chance(75) => rng.pick(&served).clone(),
        0..=3 => Method::GET,
        4..=6 => Method::POST,
        7 => Method::PUT,
        8 => Method::DELETE,
        _ => rng
            .pick(&[Method::PATCH, Method::HEAD, Method::OPTIONS])
            .clone(),
    };
    let content_type = *rng.pick(CONTENT_TYPES);
    let body: Vec<u8> = match rng.below(8) {
        0 => Vec::new(),
        1 => {
            let seed = *rng.pick(QUERIES);
            mutate(rng, seed.as_bytes())
        }
        2 => {
            let seed = *rng.pick(UPDATES);
            mutate(rng, seed.as_bytes())
        }
        3 => {
            let (_, doc) = *rng.pick(RDF);
            mutate(rng, doc.as_bytes())
        }
        4 => {
            let seed = *rng.pick(JSON);
            mutate(rng, seed.as_bytes())
        }
        5 => format!(
            "{}={}",
            rng.pick(&["query", "update"]),
            percent(rng.pick(UPDATES))
        )
        .into_bytes(),
        6 => rng.pick(UPDATES).as_bytes().to_vec(),
        _ => (0..rng.below(512)).map(|_| rng.next() as u8).collect(),
    };
    let mut builder = Request::builder().method(method).uri(uri);
    if !content_type.is_empty() && rng.chance(90) {
        builder = builder.header("content-type", content_type);
    }
    if rng.chance(70) {
        builder = builder.header("accept", *rng.pick(ACCEPTS));
    }
    if rng.chance(5)
        && let Ok(value) = HeaderValue::from_bytes(random_text(rng, 30).as_bytes())
    {
        builder = builder.header("authorization", value);
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| Request::new(Body::empty()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_requests_get_answers_not_server_errors() {
    fuzz(nrese_server::policy::PolicyConfig::default(), &[], 0).await;
}

/// With bearer tokens: an admin's, a reader's, broken ones and none, so that every route's
/// authentication and authorisation are fuzzed too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_requests_with_tokens_get_answers_not_server_errors() {
    use nrese_server::auth::{AuthConfig, StaticBearerConfig};
    let policy = nrese_server::policy::PolicyConfig {
        auth: AuthConfig::BearerStatic(StaticBearerConfig {
            read_token: Some("reader".to_owned()),
            admin_token: "admin".to_owned(),
        }),
        ..nrese_server::policy::PolicyConfig::default()
    };
    fuzz(policy, &["admin", "admin", "reader", "admin2", ""], 1).await;
}

/// `NRESE_FUZZ_CASES` random requests to an app with `policy`, each with one of `tokens`
/// (as a bearer token) where there are any.
async fn fuzz(policy: nrese_server::policy::PolicyConfig, tokens: &[&str], stream: u64) {
    let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(1500);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x2026_1005_1700);
    let mut rng = Rng(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let paths = paths();
    assert!(paths.len() > 60, "{paths:?}");
    // Backups and images go to a directory of the test's own.
    let data = tempfile::tempdir().unwrap();
    let app = test_app_with_store_config(
        nrese_store::StoreConfig {
            data_dir: data.path().to_owned(),
            ..nrese_store::StoreConfig::default()
        },
        policy,
        nrese_reasoner::ReasonerConfig::default(),
    )
    .unwrap();
    let mut by_status: BTreeMap<u16, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for case in 0..cases {
        let mut request = request(&mut rng, &paths);
        if !tokens.is_empty() {
            let token = *rng.pick(tokens);
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
                request.headers_mut().insert("authorization", value);
            }
        }
        let head = request.method() == Method::HEAD;
        let line = format!("case {case}: {} {}", request.method(), request.uri());
        let answer =
            tokio::time::timeout(Duration::from_secs(30), app.clone().oneshot(request)).await;
        let Ok(response) = answer else {
            failures.push(format!("{line}: no answer in 30 s"));
            continue;
        };
        let response = response.unwrap();
        let status = response.status();
        *by_status.entry(status.as_u16()).or_default() += 1;
        let media = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let json = media.starts_with("application/json");
        let problem = media.starts_with("application/problem+json");
        let body = axum::body::to_bytes(response.into_body(), 256 << 20).await;
        let Ok(body) = body else {
            failures.push(format!("{line}: {status}, the body couldn't be read"));
            continue;
        };
        if matches!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR | StatusCode::BAD_GATEWAY
        ) {
            failures.push(format!(
                "{line}: {status}: {}",
                String::from_utf8_lossy(&body[..body.len().min(300)])
            ));
        } else if json && !head && serde_json::from_slice::<serde_json::Value>(&body).is_err() {
            failures.push(format!(
                "{line}: {status}, a JSON content type over {}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            ));
        } else if (status.is_client_error() || status.is_server_error()) && !head {
            // One error envelope: a problem document with its status and request id.
            let document = serde_json::from_slice::<serde_json::Value>(&body).ok();
            let enveloped = problem
                && document.as_ref().is_some_and(|document| {
                    document["status"] == status.as_u16() && document["request_id"].is_string()
                });
            if !enveloped {
                failures.push(format!(
                    "{line}: {status}, not a problem document ({media}): {}",
                    String::from_utf8_lossy(&body[..body.len().min(200)])
                ));
            }
        }
    }
    eprintln!("{cases} requests (seed {seed:#x}), by status: {by_status:?}");
    // Still serving.
    for uri in [
        "/readyz",
        "/dataset/query?query=ASK%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("authorization", "Bearer admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri} after the run");
    }
    assert!(
        failures.is_empty(),
        "{} of {cases} requests failed (seed {seed:#x}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
