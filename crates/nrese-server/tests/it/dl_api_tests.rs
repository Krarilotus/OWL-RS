//! The `owl2-dl` mode over HTTP: every answer's status in its headers and in EXPLAIN,
//! exact answers asked for per query.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store_config};

const ONTOLOGY: &str = r#"
@prefix : <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
:A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . :C rdfs:subClassOf :D .
:x a :A . :y a :B . :w a [ owl:unionOf ( :B :C ) ] .
"#;

fn get(query: &str, extra: &str) -> Result<Request<Body>, axum::http::Error> {
    let encoded = serde_urlencoded::to_string([("query", query)]).expect("encoded");
    Request::builder()
        .uri(format!("/dataset/query?{encoded}{extra}"))
        .method(Method::GET)
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
}

#[tokio::test]
async fn answers_carry_their_status() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("union.ttl");
    std::fs::write(&path, ONTOLOGY)?;
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(path),
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl),
    )?;
    let query = "SELECT ?x { ?x a <http://example.com/D> }";
    let response = app.clone().oneshot(get(query, "")?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    let status = headers["nrese-completeness"].to_str()?;
    assert_eq!(
        status,
        "complete; regime=owl2-dl; lower=2; upper=3; unresolved=0"
    );
    let text = body_text(response).await?;
    assert!(text.contains("http://example.com/x") && text.contains("http://example.com/y"));

    // EXPLAIN reports the status with its reasons and the paths that decided.
    let response = app.clone().oneshot(get(query, "&explain=true")?).await?;
    let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(json["completeness"]["status"], "complete");
    assert_eq!(json["completeness"]["bounds"]["lower"], 2);
    assert_eq!(json["completeness"]["regime"], "owl2-dl");
    // The candidates the exact services decided: diagnostics, in EXPLAIN only.
    assert_eq!(json["candidates"]["proved"], 1);
    assert_eq!(json["candidates"]["refuted"], 0);
    assert!(
        json["completeness"]["decided_by"]
            .as_array()
            .is_some_and(|p| p.contains(&serde_json::json!("exact-ground-entailment")))
    );

    // Exact answers a union query can't get: 422 with a problem document saying why,
    // never a partial answer.
    let union =
        "SELECT ?x { { ?x a <http://example.com/D> } UNION { ?x a <http://example.com/C> } }";
    let response = app
        .clone()
        .oneshot(get(union, "&dl-answers=exact")?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    let problem: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(
        problem["type"],
        "https://nrese.dev/problems/incomplete-answer"
    );
    assert_eq!(problem["status"], 422);
    assert_eq!(problem["regime"], "owl2-dl");
    assert!(
        problem["reasons"]
            .as_array()
            .is_some_and(|r| !r.is_empty() && r.iter().all(|r| r["source"] == "dl")),
        "{problem}"
    );
    assert!(problem["unresolved"].is_array(), "{problem}");
    let response = app.clone().oneshot(get(union, "")?).await?;
    let status = response.headers()["nrese-completeness"].to_str()?;
    assert!(
        status.starts_with("sound-only; regime=owl2-dl; "),
        "{status}"
    );
    // Why w is a D: not derived by the rules, explained by OWL 2 DL's axioms.
    let explain = serde_urlencoded::to_string([
        ("subj", "<http://example.com/w>"),
        ("pred", "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"),
        ("obj", "<http://example.com/D>"),
    ])?;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/repositories/nrese/explain?{explain}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(json["owl2_dl"]["minimal"], true);
    assert_eq!(json["owl2_dl"]["axioms"].as_array().map(Vec::len), Some(3));
    let response = app.oneshot(get(query, "&dl-answers=everything")?).await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

/// The status of an answer, and its body, for `request`.
async fn answer(
    app: &axum::Router,
    request: Request<Body>,
) -> Result<(Option<String>, String), Box<dyn std::error::Error>> {
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let status = response
        .headers()
        .get("nrese-completeness")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    Ok((status, body_text(response).await?))
}

/// SYN-72 slice 2 review r1 F1: under `owl2-dl`, an inferred-only read (GraphDB's
/// `onto:implicit`, in `FROM` or as `default-graph-uri`) reads the rules' closure without
/// the DL bounds: x, a D only through the union, is missing, so the answers are never
/// certified complete. The same from the result cache and inside a transaction (nothing
/// pending, or an addition pending). Asserted statements alone stay `complete`.
#[tokio::test]
async fn inferred_only_reads_are_never_certified_complete() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("union.ttl");
    std::fs::write(&path, ONTOLOGY)?;
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(path),
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl),
    )?;
    const IMPLICIT: &str = "http://www.ontotext.com/implicit";
    let from = format!("SELECT ?x FROM <{IMPLICIT}> {{ ?x a <http://example.com/D> }}");
    let plain = "SELECT ?x { ?x a <http://example.com/D> }";
    let by_param = format!(
        "&{}",
        serde_urlencoded::to_string([("default-graph-uri", IMPLICIT)])?
    );
    let not_complete = |label: &str, (status, text): (Option<String>, String)| {
        assert!(
            text.contains("http://example.com/y") && !text.contains("http://example.com/x"),
            "{label}: the rules' closure, without x: {text}"
        );
        let status = status.unwrap_or_default();
        assert!(
            status.starts_with("sound-only; regime=owl2-dl"),
            "{label}: NRESE-Completeness {status:?}"
        );
    };
    // The second round's answers come from the result cache.
    for round in ["first", "cached"] {
        not_complete(
            &format!("FROM, {round}"),
            answer(&app, get(&from, "")?).await?,
        );
        not_complete(
            &format!("default-graph-uri, {round}"),
            answer(&app, get(plain, &by_param)?).await?,
        );
    }
    let explicit =
        "SELECT ?x FROM <http://www.ontotext.com/explicit> { ?x a <http://example.com/D> }";
    let (status, _) = answer(&app, get(explicit, "")?).await?;
    assert_eq!(
        status.as_deref(),
        Some("complete"),
        "asserted statements alone"
    );

    // Inside an RDF4J transaction: nothing pending, then an addition pending.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/repositories/nrese/transactions")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response.headers()["location"].to_str()?.to_owned();
    let tx = &location[location.find("/repositories/").expect("a transaction path")..];
    let in_tx = |extra: String| {
        Request::builder()
            .method(Method::PUT)
            .uri(format!("{tx}?action={extra}"))
            .header("accept", "application/sparql-results+json")
            .header("content-type", "text/turtle")
    };
    let query = format!("QUERY&{}", serde_urlencoded::to_string([("query", &from)])?);
    not_complete(
        "transaction, nothing pending",
        answer(&app, in_tx(query.clone()).body(Body::empty())?).await?,
    );
    let response = app
        .clone()
        .oneshot(in_tx("ADD".to_owned()).body(Body::from(
            "<http://example.com/z> a <http://example.com/B> .",
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    not_complete(
        "transaction, an addition pending",
        answer(&app, in_tx(query).body(Body::empty())?).await?,
    );
    Ok(())
}

/// Spec (C-501): an OWL 2 DL explanation whose first entailment test a budget stops is
/// undecided: 422, the incomplete-answer problem under owl2-dl with the budget as its
/// reason, never a 404 (w a D holds: `answers_carry_their_status`).
#[tokio::test]
async fn an_undecided_dl_explanation_is_an_incomplete_answer()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("union.ttl");
    std::fs::write(&path, ONTOLOGY)?;
    let app = test_app_with_store_config(
        StoreConfig {
            dl: nrese_store::DlConfig {
                explanation_tests: Some(0),
                ..Default::default()
            },
            ..StoreConfig::in_memory().with_ontology(path)
        },
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl),
    )?;
    let explain = serde_urlencoded::to_string([
        ("subj", "<http://example.com/w>"),
        ("pred", "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"),
        ("obj", "<http://example.com/D>"),
    ])?;
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/repositories/nrese/explain?{explain}"))
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let body = body_text(response).await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let problem: serde_json::Value = serde_json::from_str(&body)?;
    assert_eq!(
        problem["type"],
        "https://nrese.dev/problems/incomplete-answer"
    );
    assert_eq!(problem["regime"], "owl2-dl");
    let reasons = problem["reasons"].as_array().cloned().unwrap_or_default();
    assert!(
        reasons.iter().any(|r| r["source"] == "dl"
            && r["text"]
                .as_str()
                .is_some_and(|text| text.contains("dl.explanation_tests"))),
        "{body}"
    );
    Ok(())
}

/// `request`'s answer must be the documented refusal of exact answers (`docs/ops/http-api.md`,
/// `dl-answers`), given before any answer is evaluated: 422, an `application/problem+json`
/// incomplete-answer problem under owl2-dl, with no bounds and no unresolved candidate. Its
/// reasons are those the same read reports without exact (`without`, its
/// NRESE-Completeness), and one of them is `want` (its source, words of its text). What
/// came instead is in the failure: the HTTP status, NRESE-Completeness and the body.
async fn refused(
    app: &axum::Router,
    label: &str,
    request: Request<Body>,
    without: &str,
    want: (&str, &str),
) -> Result<(), Box<dyn std::error::Error>> {
    let response = app.clone().oneshot(request).await?;
    let code = response.status();
    let status = response.headers().get("nrese-completeness").cloned();
    let media = response.headers().get("content-type").cloned();
    let body = body_text(response).await?;
    let got = format!("{label}: got {code}, NRESE-Completeness {status:?}, body {body}");
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{got}");
    assert_eq!(
        media.as_ref().and_then(|m| m.to_str().ok()),
        Some("application/problem+json"),
        "{got}"
    );
    let problem: serde_json::Value = serde_json::from_str(&body)?;
    assert_eq!(
        problem["type"], "https://nrese.dev/problems/incomplete-answer",
        "{got}"
    );
    assert_eq!(problem["status"], 422, "{got}");
    assert_eq!(problem["regime"], "owl2-dl", "{got}");
    assert_eq!(problem["bounds"], serde_json::Value::Null, "{got}");
    assert_eq!(problem["unresolved"], serde_json::json!([]), "{got}");
    let reasons = problem["reasons"].as_array().cloned().unwrap_or_default();
    let line = |r: &serde_json::Value| {
        let part = |key: &str| r[key].as_str().unwrap_or_default().to_owned();
        format!("{}: {}", part("source"), part("text"))
    };
    assert!(
        reasons
            .iter()
            .any(|r| r["source"] == want.0 && line(r).contains(want.1)),
        "{got}"
    );
    // The same reasons, in the same order, as the read without exact: none dropped, none added.
    let all = reasons.iter().map(line).collect::<Vec<_>>().join("; ");
    assert!(
        without.contains(&format!("reasons=\"{all}\"")),
        "{label}: the reasons {all:?} aren't those without exact, {without}"
    );
    Ok(())
}

/// A new transaction's path, from `/repositories/`.
async fn begin(app: &axum::Router) -> Result<String, Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/repositories/nrese/transactions")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response.headers()["location"].to_str()?.to_owned();
    let at = location.find("/repositories/").expect("a transaction path");
    Ok(location[at..].to_owned())
}

/// Spec (`dl-answers=exact`, `docs/spec/reasoning-semantics.md`): under owl2-dl, exact answers
/// that can't be proven complete are refused. A read inside a transaction with an addition
/// pending is never proven complete (it reads the rules' closure of the pending state), so
/// exact refuses it; without exact it answers with its honest status. With nothing pending
/// the same read is answered as outside a transaction.
#[tokio::test]
async fn exact_refuses_a_read_with_changes_pending() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("union.ttl");
    std::fs::write(&path, ONTOLOGY)?;
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(path),
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl),
    )?;
    let tx = begin(&app).await?;
    let in_tx = |extra: &str| {
        Request::builder()
            .method(Method::PUT)
            .uri(format!("{tx}?action={extra}"))
            .header("accept", "application/sparql-results+json")
            .header("content-type", "text/turtle")
    };
    let query = format!(
        "QUERY&{}",
        serde_urlencoded::to_string([("query", "SELECT ?x { ?x a <http://example.com/D> }")])?
    );
    let exact = format!("{query}&dl-answers=exact");
    // Nothing pending: answered as outside a transaction (`answers_carry_their_status`).
    let (status, _) = answer(&app, in_tx(&exact).body(Body::empty())?).await?;
    let status = status.unwrap_or_default();
    assert!(
        status.starts_with("complete; regime=owl2-dl"),
        "nothing pending: {status}"
    );
    let response = app
        .clone()
        .oneshot(in_tx("ADD").body(Body::from(
            "<http://example.com/z> a <http://example.com/B> .",
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let (status, _) = answer(&app, in_tx(&query).body(Body::empty())?).await?;
    let status = status.unwrap_or_default();
    assert!(
        status.starts_with("sound-only; regime=owl2-dl"),
        "an addition pending, without exact: {status}"
    );
    refused(
        &app,
        "an addition pending, exact",
        in_tx(&exact).body(Body::empty())?,
        &status,
        ("dl", "a read inside a transaction"),
    )
    .await?;
    // A deletion alone pending: the same refusal, and the deletion's reason in it.
    let tx = begin(&app).await?;
    let in_tx = |extra: &str| {
        Request::builder()
            .method(Method::PUT)
            .uri(format!("{tx}?action={extra}"))
            .header("accept", "application/sparql-results+json")
            .header("content-type", "text/turtle")
    };
    let response = app
        .clone()
        .oneshot(in_tx("DELETE").body(Body::from(
            "<http://example.com/y> a <http://example.com/B> .",
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let (status, _) = answer(&app, in_tx(&query).body(Body::empty())?).await?;
    let status = status.unwrap_or_default();
    assert!(
        status.starts_with("unsound; regime=owl2-dl"),
        "a deletion pending, without exact: {status}"
    );
    refused(
        &app,
        "a deletion pending, exact",
        in_tx(&exact).body(Body::empty())?,
        &status,
        ("transaction", "may delete"),
    )
    .await?;
    // An annotation property declared only by the pending operations: the pending state's
    // ontology marks the answers `unsound`, and the refusal carries that mark too.
    let tx = begin(&app).await?;
    let in_tx = |extra: &str| {
        Request::builder()
            .method(Method::PUT)
            .uri(format!("{tx}?action={extra}"))
            .header("accept", "application/sparql-results+json")
            .header("content-type", "text/turtle")
    };
    let response = app
        .clone()
        .oneshot(in_tx("ADD").body(Body::from(
            "<http://example.com/note> a <http://www.w3.org/2002/07/owl#AnnotationProperty> ; \
             <http://www.w3.org/2000/01/rdf-schema#domain> <http://example.com/Q> .",
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let (status, _) = answer(&app, in_tx(&query).body(Body::empty())?).await?;
    let status = status.unwrap_or_default();
    assert!(
        status.starts_with("unsound; regime=owl2-dl") && status.contains("example.com/note"),
        "an annotation property pending, without exact: {status}"
    );
    refused(
        &app,
        "an annotation property pending, exact",
        in_tx(&exact).body(Body::empty())?,
        &status,
        ("dl", "example.com/note> has annotation-property axioms"),
    )
    .await
}

/// Spec (`dl-answers=exact`): an inferred-only read (`onto:implicit`, in `FROM` or as
/// `default-graph-uri`) reads the rules' closure without the DL bounds, so it is never
/// proven complete (`inferred_only_reads_are_never_certified_complete`) and exact refuses it.
/// The same query over the whole store is answered as before.
#[tokio::test]
async fn exact_refuses_an_inferred_only_read() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("union.ttl");
    std::fs::write(&path, ONTOLOGY)?;
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(path),
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl),
    )?;
    const IMPLICIT: &str = "http://www.ontotext.com/implicit";
    let plain = "SELECT ?x { ?x a <http://example.com/D> }";
    let from = format!("SELECT ?x FROM <{IMPLICIT}> {{ ?x a <http://example.com/D> }}");
    let by_param = format!(
        "&{}",
        serde_urlencoded::to_string([("default-graph-uri", IMPLICIT)])?
    );
    let (inferred, _) = answer(&app, get(&from, "")?).await?;
    let inferred = inferred.unwrap_or_default();
    assert!(
        inferred.starts_with("sound-only; regime=owl2-dl"),
        "inferred only, without exact: {inferred}"
    );
    let (status, _) = answer(&app, get(plain, "&dl-answers=exact")?).await?;
    let status = status.unwrap_or_default();
    assert!(
        status.starts_with("complete; regime=owl2-dl"),
        "the whole store, exact: {status}"
    );
    let want = ("dl", "the inferred statements alone");
    refused(
        &app,
        "FROM onto:implicit, exact",
        get(&from, "&dl-answers=exact")?,
        &inferred,
        want,
    )
    .await?;
    refused(
        &app,
        "default-graph-uri onto:implicit, exact",
        get(plain, &format!("{by_param}&dl-answers=exact"))?,
        &inferred,
        want,
    )
    .await?;
    // Exact as the repository's default (`dl.answers = "exact"`), no parameter: the same.
    let mut config = StoreConfig::in_memory().with_ontology(dir.path().join("union.ttl"));
    config.dl.answers = nrese_store::DlAnswers::Exact;
    let by_default = test_app_with_store_config(
        config,
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl),
    )?;
    refused(
        &by_default,
        "FROM onto:implicit, exact by default",
        get(&from, "")?,
        &inferred,
        want,
    )
    .await
}
