//! `GET /dataset/classification`: the OWL 2 EL class hierarchy of the asserted ontology,
//! computed on request. JSON by default; N-Triples (`rdfs:subClassOf` statements, and
//! `owl:Nothing` as the superclass of unsatisfiable classes) on request.

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

pub async fn classify(
    state: AppState,
    headers: &HeaderMap,
    scope: nrese_store::ReadScope,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let triples = accept.contains("application/n-triples") || accept.contains("text/plain");
    let store = state.store();
    let report = tokio::time::timeout(
        state.policy().timeouts.query,
        tokio::task::spawn_blocking(move || store.classify(&scope)),
    )
    .await
    .map_err(|_| ApiError::timeout("classification exceeded policy timeout"))?
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let (media_type, body) = if triples {
        let mut text = String::new();
        for (sub, sup) in &report.subsumptions {
            text.push_str(&format!(
                "<{sub}> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{sup}> .\n"
            ));
        }
        for class in &report.unsatisfiable {
            text.push_str(&format!(
                "<{class}> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://www.w3.org/2002/07/owl#Nothing> .\n"
            ));
        }
        ("application/n-triples", text.into_bytes())
    } else {
        let json = serde_json::json!({
            "profile": "OWL 2 EL",
            "micros": report.micros,
            "subsumptions": report.subsumptions,
            "unsatisfiable": report.unsatisfiable,
            "skipped": report.skipped,
        });
        (
            "application/json",
            serde_json::to_vec(&json).map_err(|error| ApiError::internal(error.to_string()))?,
        )
    };
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, media_type)],
        Body::from(body),
    )
        .into_response())
}
