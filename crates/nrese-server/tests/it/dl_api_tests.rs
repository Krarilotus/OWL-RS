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
