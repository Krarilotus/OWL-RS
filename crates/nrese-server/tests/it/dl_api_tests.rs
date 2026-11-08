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
