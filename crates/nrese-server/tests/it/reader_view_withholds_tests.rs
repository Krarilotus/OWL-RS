//! What a reader by support is told of a property chain its own graphs close, when a graph
//! it can't read equates the chain's properties: never `complete` with an answer the reader
//! of every graph contradicts.

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
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";
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
const ASK: &str = "ASK { ex:x0 ex:q ex:x7 }";
const REMATERIALISE: &str = "/api/v1/repositories/nrese/reasoning/rematerialise";

/// `body` by `roles` to `uri` with `method`: an update to the update endpoint, a query to
/// the query endpoint, nothing to any other.
async fn send(
    app: &Router,
    roles: &[&str],
    method: Method,
    uri: &str,
    body: String,
) -> TestResult<(StatusCode, Option<String>, String)> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::ACCEPT, "application/sparql-results+json")
        .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)));
    let body = match uri {
        "/dataset/update" => {
            request = request.header(header::CONTENT_TYPE, "application/sparql-update");
            Body::from(format!("{PREFIXES} {body}"))
        }
        u if u.starts_with("/dataset/query") => {
            request = request.header(header::CONTENT_TYPE, "application/sparql-query");
            Body::from(format!("{PREFIXES} {body}"))
        }
        _ => Body::empty(),
    };
    let response = app.clone().oneshot(request.body(body)?).await?;
    let completeness = response
        .headers()
        .get("nrese-completeness")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    Ok((response.status(), completeness, body_text(response).await?))
}

async fn insert(app: &Router, data: &str) -> TestResult {
    let update = format!("INSERT DATA {{ {data} }}");
    let (code, _, body) = send(app, ADMIN, Method::POST, "/dataset/update", update).await?;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    Ok(())
}

/// What `roles` are told of [`ASK`]: the answer and the whole header, with EXPLAIN's
/// `completeness` and `ql` printed as `SEEN` under `label`.
async fn told(app: &Router, roles: &[&str], label: &str) -> TestResult<(bool, String)> {
    let (code, header, body) =
        send(app, roles, Method::POST, "/dataset/query", ASK.to_owned()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    let answer = serde_json::from_str::<serde_json::Value>(&body)?["boolean"]
        .as_bool()
        .ok_or("no boolean")?;
    let uri = "/dataset/query?explain=true";
    let (code, _, body) = send(app, roles, Method::POST, uri, ASK.to_owned()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    let explanation: serde_json::Value = serde_json::from_str(&body)?;
    let member = |name: &str| {
        explanation
            .get(name)
            .map_or("absent".to_owned(), ToString::to_string)
    };
    let header = header.unwrap_or_default();
    println!(
        "SEEN {label}: answer {answer}, header {header:?}, explain completeness {}, ql {}",
        member("completeness"),
        member("ql")
    );
    Ok((answer, header))
}

/// `ex:q owl:propertyChainAxiom (ex:p1 … ex:p7)` on blank cells (as the OWL mapping reads a
/// list), and the chain `ex:x0 ex:p1 ex:x1 … ex:x6 ex:p7 ex:x7`, in `:g1`.
fn chain() -> String {
    let mut data = "ex:q owl:propertyChainAxiom _:l1 .".to_owned();
    for i in 1..=7 {
        let rest = match i {
            7 => "rdf:nil".to_owned(),
            i => format!("_:l{}", i + 1),
        };
        data += &format!(
            " _:l{i} rdf:first ex:p{i} . _:l{i} rdf:rest {rest} . ex:x{} ex:p{i} ex:x{i} .",
            i - 1
        );
    }
    format!("GRAPH :g1 {{ {data} }}")
}

/// Spec: docs/spec/reasoning-semantics.md: `complete` says no entailed row is missing; a
/// reader by support reads the closure of the graphs it reads (crates/nrese-store/src/
/// support.rs, module docs). Under `owl2-rl`, `representatives` and the QL rewriting on, with
/// the default graph the union of every graph (a query with no dataset reads `:g1`), `:g1`
/// holds a seven-property chain axiom and a chain it closes. Revision B adds
/// `ex:pi owl:sameAs ex:ai` (i = 1…7) in `:g2`, which the reader by support can't read; B is
/// read after the commit and again after a rematerialisation. Wherever the reader of every
/// graph answers `ASK { ex:x0 ex:q ex:x7 }` `true` and the reader by support `false`, the
/// reader by support isn't told `complete`. Each header and EXPLAIN's `completeness` and `ql`
/// are printed (`SEEN …`).
#[tokio::test]
async fn a_reader_by_support_missing_a_chain_its_graphs_close_is_never_told_complete() -> TestResult
{
    let app = test_app_with_store_config(
        StoreConfig {
            ql_rewriting: QlRewritingMode::On,
            union_default_graph: true,
            equality_by_representatives: true,
            equality_compact: false,
            ..StoreConfig::default()
        },
        policy_of(POLICY)?,
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    insert(&app, &chain()).await?;
    let mut seen = vec![(
        "A",
        told(&app, ADMIN, "A every graph").await?,
        told(&app, READER, "A reader").await?,
    )];
    let same: String = (1..=7)
        .map(|i| format!(" ex:p{i} owl:sameAs ex:a{i} ."))
        .collect();
    insert(&app, &format!("GRAPH :g2 {{ {same} }}")).await?;
    seen.push((
        "B (commit)",
        told(&app, ADMIN, "B (commit) every graph").await?,
        told(&app, READER, "B (commit) reader").await?,
    ));
    let (code, _, body) = send(&app, ADMIN, Method::POST, REMATERIALISE, String::new()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    seen.push((
        "B (rematerialised)",
        told(&app, ADMIN, "B (rematerialised) every graph").await?,
        told(&app, READER, "B (rematerialised) reader").await?,
    ));
    let wrong: Vec<String> = seen
        .iter()
        .filter(|(_, whole, reader)| {
            let first = reader.1.split(';').next().unwrap_or_default();
            whole.0 && !reader.0 && first == "complete"
        })
        .map(|(at, _, reader)| format!("{at}: reader false, told {:?}", reader.1))
        .collect();
    assert!(wrong.is_empty(), "told complete:\n{}", wrong.join("\n"));
    Ok(())
}
