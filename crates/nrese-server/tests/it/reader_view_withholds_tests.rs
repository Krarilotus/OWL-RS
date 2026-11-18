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
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
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

/// What `roles` are told of `ask`: the answer and the whole header, with EXPLAIN's
/// `completeness` and `ql` printed as `SEEN` under `label`.
async fn told(app: &Router, roles: &[&str], ask: &str, label: &str) -> TestResult<(bool, String)> {
    let (code, header, body) =
        send(app, roles, Method::POST, "/dataset/query", ask.to_owned()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    let answer = serde_json::from_str::<serde_json::Value>(&body)?["boolean"]
        .as_bool()
        .ok_or("no boolean")?;
    let uri = "/dataset/query?explain=true";
    let (code, _, body) = send(app, roles, Method::POST, uri, ask.to_owned()).await?;
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
        told(&app, ADMIN, ASK, "A every graph").await?,
        told(&app, READER, ASK, "A reader").await?,
    )];
    let same: String = (1..=7)
        .map(|i| format!(" ex:p{i} owl:sameAs ex:a{i} ."))
        .collect();
    insert(&app, &format!("GRAPH :g2 {{ {same} }}")).await?;
    seen.push((
        "B (commit)",
        told(&app, ADMIN, ASK, "B (commit) every graph").await?,
        told(&app, READER, ASK, "B (commit) reader").await?,
    ));
    let (code, _, body) = send(&app, ADMIN, Method::POST, REMATERIALISE, String::new()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    seen.push((
        "B (rematerialised)",
        told(&app, ADMIN, ASK, "B (rematerialised) every graph").await?,
        told(&app, READER, ASK, "B (rematerialised) reader").await?,
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

/// Spec: docs/spec/reasoning-semantics.md (access): "adding or removing an assertion the
/// reader can't see changes none of its answers (its status may change)". Under `owl2-rl`,
/// `representatives`, the QL rewriting on and the default graph the union of every graph,
/// `:g1` holds `ex:x a ex:A` and `ex:A rdfs:subClassOf [ owl:onProperty ex:p ;
/// owl:someValuesFrom ex:B ]`, which entail `ASK { ex:x ex:p ?y }`. Store 2 also holds, in
/// `:g2`, which a reader by support of `:g1` can't read, a chain axiom on a list with no
/// `rdf:rest`. That reader answers `true` in both stores. Each header and EXPLAIN's
/// `completeness` and `ql` are printed (`SEEN …`).
#[tokio::test]
async fn a_list_left_out_in_a_graph_the_reader_cant_read_changes_none_of_its_answers() -> TestResult
{
    let readable = "GRAPH :g1 { ex:x a ex:A . ex:p a owl:ObjectProperty . ex:A rdfs:subClassOf \
                    [ a owl:Restriction ; owl:onProperty ex:p ; owl:someValuesFrom ex:B ] }";
    let hidden = "GRAPH :g2 { ex:q owl:propertyChainAxiom _:l . _:l rdf:first ex:r }";
    let mut answers = Vec::new();
    for (store, data) in [
        ("store 1", readable.to_owned()),
        ("store 2", format!("{readable} {hidden}")),
    ] {
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
        insert(&app, &data).await?;
        let ask = "ASK { ex:x ex:p ?y }";
        let (answer, _) = told(&app, READER, ask, &format!("{store} reader")).await?;
        answers.push(format!("{store}: {answer}"));
    }
    let expected = ["store 1: true", "store 2: true"];
    assert_eq!(answers, expected, "the reader's answers");
    Ok(())
}

/// The reader of the pending rows: it reads and writes `:g1` and reads the default graph.
const WRITER_POLICY: &str = r#"
default = "deny"
inferred = "supported"

[[role]]
name = "one"
read = ["urn:graph:g1"]
write = ["urn:graph:g1"]
default_graph = "read"
"#;

/// The status `roles` are told of `ask` inside an RDF4J transaction with `update` pending.
async fn pending_status(
    app: &Router,
    roles: &[&str],
    update: &str,
    ask: &str,
) -> TestResult<String> {
    let bearer = format!("Bearer {}", token(roles));
    let begin = Request::builder()
        .method(Method::POST)
        .uri("/repositories/nrese/transactions")
        .header(header::AUTHORIZATION, &bearer)
        .body(Body::empty())?;
    let begin = app.clone().oneshot(begin).await?;
    assert_eq!(begin.status(), StatusCode::CREATED);
    let tx = begin.headers()[header::LOCATION].to_str()?.to_owned();
    let form = serde_urlencoded::to_string([("update", format!("{PREFIXES} {update}"))])?;
    let request = Request::builder()
        .method(Method::PUT)
        .uri(format!("{tx}?action=UPDATE"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::AUTHORIZATION, &bearer)
        .body(Body::from(form))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        body_text(response).await?
    );
    let query = serde_urlencoded::to_string([("query", format!("{PREFIXES} {ask}"))])?;
    let request = Request::builder()
        .method(Method::PUT)
        .uri(format!("{tx}?action=QUERY&{query}"))
        .header(header::ACCEPT, "application/sparql-results+json")
        .header(header::AUTHORIZATION, &bearer)
        .body(Body::empty())?;
    let response = app.clone().oneshot(request).await?;
    let header = response.headers().get("nrese-completeness").cloned();
    let body = body_text(response).await?;
    let header = header.ok_or_else(|| format!("no status: {body}"))?;
    Ok(header.to_str()?.to_owned())
}

/// Spec: docs/spec/reasoning-semantics.md (transactions): a read inside a transaction whose
/// pending operations may delete is `unsound`, as they may delete what an inference rests
/// on, when its view is certified; under compact equality a reader by support's view isn't
/// its closure, so no certificate stands and it is `not-certified`, the pending reasons
/// absorbed. So a reader by support of `:g1` asking for `ex:a ex:q ex:b` (which `:g1`
/// entails) after a pending `DELETE DATA` of `ex:a ex:p ex:b` is told `unsound` with the
/// delete reason under `representatives`, and `not-certified` under compact; after a pending
/// insert of an unrelated statement, under compact, `not-certified`.
#[tokio::test]
async fn a_pending_delete_is_unsound_on_a_certified_view_and_a_compact_pending_read_not_certified()
-> TestResult {
    let delete = "DELETE DATA { GRAPH :g1 { ex:a ex:p ex:b } }";
    let add = "INSERT DATA { GRAPH :g1 { ex:c ex:r ex:d } }";
    let rows = [
        ("representatives, pending delete", false, delete),
        ("compact, pending delete", true, delete),
        ("compact, pending insert", true, add),
    ];
    let mut wrong = Vec::new();
    for (name, compact, update) in rows {
        let app = test_app_with_store_config(
            StoreConfig {
                equality_compact: compact,
                ql_rewriting: QlRewritingMode::On,
                ..StoreConfig::default()
            },
            policy_of(WRITER_POLICY)?,
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
        )?;
        let data = "GRAPH :g1 { ex:p <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> ex:q . \
                    ex:a ex:p ex:b }";
        insert(&app, data).await?;
        let status = pending_status(&app, READER, update, "ASK { ex:a ex:q ex:b }").await?;
        println!("SEEN {name}: {status:?}");
        let right = match compact {
            false => status.starts_with("unsound;") && status.contains("may delete"),
            true => status == "not-certified",
        };
        if !right {
            wrong.push(format!("{name}: {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "wrong rows:\n{}", wrong.join("\n"));
    Ok(())
}
