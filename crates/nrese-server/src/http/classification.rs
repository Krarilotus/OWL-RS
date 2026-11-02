//! `GET /dataset/classification` and `GET /dataset/realisation`: the class hierarchy and
//! the individuals' types of the asserted ontology under OWL 2 DL, computed on request by
//! the DL engines (kept per revision). JSON by default, with the consistency verdict
//! (`consistent`: `true`, `false` or `null` where undecided) and whether the result is
//! complete; N-Triples on request (`rdfs:subClassOf` statements with `owl:Nothing` as the
//! superclass of unsatisfiable classes; `rdf:type` statements; an inconsistent ontology
//! as the one statement `owl:Thing rdfs:subClassOf owl:Nothing`). Both forms carry the
//! `NRESE-Completeness` header, so an undecided or incomplete answer is never unmarked.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_sparql::{Completeness, Regime};
use nrese_store::dl::Verdict;

use crate::error::ApiError;
use crate::http::sparql::COMPLETENESS;
use crate::state::AppState;

const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The answer's status under OWL 2 DL: what it lists is entailed either way, so it is
/// sound; complete only with nothing left out and the consistency decided.
fn completeness(consistency: &Verdict, incomplete: &[String]) -> Completeness {
    let mut status = Completeness::under(Regime::Owl2Dl);
    for why in incomplete {
        status.incomplete("dl", why.clone());
    }
    // The engines list an undecided verdict among the reasons; never complete without one.
    if let Some(why) = consistency.reason()
        && incomplete.is_empty()
    {
        status.incomplete("dl", format!("consistency not decided: {why}"));
    }
    status
}

/// The verdict as JSON: `consistent` (`null` where undecided) and `consistency`.
fn verdict_json(consistency: &Verdict) -> (serde_json::Value, &'static str) {
    (
        serde_json::json!(consistency.decided()),
        consistency.as_str(),
    )
}

/// What the N-Triples form starts with: for an inconsistency, its one statement (every
/// class is below `owl:Nothing`; the report lists nothing else).
fn triples_head(consistency: &Verdict) -> String {
    match consistency {
        Verdict::Inconsistent => format!("<{THING}> <{SUB_CLASS_OF}> <{NOTHING}> .\n"),
        _ => String::new(),
    }
}

/// Whether the client asked for N-Triples.
fn wants_triples(headers: &HeaderMap) -> bool {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    accept.contains("application/n-triples") || accept.contains("text/plain")
}

/// Runs `work` off the async threads within the query timeout.
async fn blocking<T: Send + 'static>(
    state: &AppState,
    what: &str,
    work: impl FnOnce() -> nrese_store::StoreResult<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::time::timeout(
        state.policy().timeouts.query,
        tokio::task::spawn_blocking(work),
    )
    .await
    .map_err(|_| ApiError::timeout(format!("{what} exceeded policy timeout")))?
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::internal(error.to_string()))
}

fn respond(
    triples: bool,
    text: String,
    json: serde_json::Value,
    status: &Completeness,
) -> Result<Response, ApiError> {
    let (media_type, body) = if triples {
        ("application/n-triples", text.into_bytes())
    } else {
        (
            "application/json",
            serde_json::to_vec(&json).map_err(|error| ApiError::internal(error.to_string()))?,
        )
    };
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, media_type)],
        Body::from(body),
    )
        .into_response();
    // As a query's answers carry it (`http::sparql`), readable across origins too.
    let headers = response.headers_mut();
    headers.insert(
        COMPLETENESS,
        HeaderValue::from_str(&status.header())
            .unwrap_or_else(|_| HeaderValue::from_static(status.as_str())),
    );
    headers.append(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(COMPLETENESS),
    );
    Ok(response)
}

pub async fn classify(
    state: AppState,
    headers: &HeaderMap,
    scope: nrese_store::ReadScope,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let store = state.store();
    let report = blocking(&state, "classification", move || store.classify(&scope)).await?;
    let mut text = triples_head(&report.consistency);
    for (sub, sup) in &report.subsumptions {
        text.push_str(&format!("<{sub}> <{SUB_CLASS_OF}> <{sup}> .\n"));
    }
    for class in &report.unsatisfiable {
        text.push_str(&format!("<{class}> <{SUB_CLASS_OF}> <{NOTHING}> .\n"));
    }
    let (consistent, consistency) = verdict_json(&report.consistency);
    let json = serde_json::json!({
        "profile": "OWL 2 DL",
        "engine": report.engine,
        "complete": report.complete(),
        "incomplete": report.incomplete,
        "consistent": consistent,
        "consistency": consistency,
        "micros": report.micros,
        "subsumptions": report.subsumptions,
        "unsatisfiable": report.unsatisfiable,
        "equivalent_to_thing": report.equivalent_to_thing,
    });
    let status = completeness(&report.consistency, &report.incomplete);
    respond(wants_triples(headers), text, json, &status)
}

pub async fn realise(
    state: AppState,
    headers: &HeaderMap,
    scope: nrese_store::ReadScope,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let store = state.store();
    let report = blocking(&state, "realisation", move || store.realise(&scope)).await?;
    let mut text = triples_head(&report.consistency);
    for (individual, types) in &report.types {
        for class in types {
            text.push_str(&format!("<{individual}> <{RDF_TYPE}> <{class}> .\n"));
        }
    }
    let types: serde_json::Map<String, serde_json::Value> = report
        .types
        .iter()
        .map(|(individual, types)| (individual.clone(), serde_json::json!(types)))
        .collect();
    let (consistent, consistency) = verdict_json(&report.consistency);
    let json = serde_json::json!({
        "profile": "OWL 2 DL",
        "engine": report.engine,
        "complete": report.complete(),
        "incomplete": report.incomplete,
        "consistent": consistent,
        "consistency": consistency,
        "micros": report.micros,
        "types": types,
    });
    let status = completeness(&report.consistency, &report.incomplete);
    respond(wants_triples(headers), text, json, &status)
}
