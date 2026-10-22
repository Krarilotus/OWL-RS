//! `GET /dataset/classification` and `GET /dataset/realisation`: the class hierarchy and
//! the individuals' types of the asserted ontology under OWL 2 DL, computed on request by
//! the DL engines (kept per revision). JSON by default, with whether the result is
//! complete; N-Triples on request (`rdfs:subClassOf` statements with `owl:Nothing` as the
//! superclass of unsatisfiable classes; `rdf:type` statements).

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

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
    text: impl FnOnce() -> String,
    json: impl FnOnce() -> serde_json::Value,
) -> Result<Response, ApiError> {
    let (media_type, body) = if triples {
        ("application/n-triples", text().into_bytes())
    } else {
        (
            "application/json",
            serde_json::to_vec(&json()).map_err(|error| ApiError::internal(error.to_string()))?,
        )
    };
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, media_type)],
        Body::from(body),
    )
        .into_response())
}

pub async fn classify(
    state: AppState,
    headers: &HeaderMap,
    scope: nrese_store::ReadScope,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let store = state.store();
    let report = blocking(&state, "classification", move || store.classify(&scope)).await?;
    respond(
        wants_triples(headers),
        || {
            let mut text = String::new();
            for (sub, sup) in &report.subsumptions {
                text.push_str(&format!("<{sub}> <{SUB_CLASS_OF}> <{sup}> .\n"));
            }
            for class in &report.unsatisfiable {
                text.push_str(&format!("<{class}> <{SUB_CLASS_OF}> <{NOTHING}> .\n"));
            }
            text
        },
        || {
            serde_json::json!({
                "profile": "OWL 2 DL",
                "engine": report.engine,
                "complete": report.complete(),
                "incomplete": report.incomplete,
                "consistent": report.consistent,
                "micros": report.micros,
                "subsumptions": report.subsumptions,
                "unsatisfiable": report.unsatisfiable,
                "equivalent_to_thing": report.equivalent_to_thing,
            })
        },
    )
}

pub async fn realise(
    state: AppState,
    headers: &HeaderMap,
    scope: nrese_store::ReadScope,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let store = state.store();
    let report = blocking(&state, "realisation", move || store.realise(&scope)).await?;
    respond(
        wants_triples(headers),
        || {
            let mut text = String::new();
            for (individual, types) in &report.types {
                for class in types {
                    text.push_str(&format!("<{individual}> <{RDF_TYPE}> <{class}> .\n"));
                }
            }
            text
        },
        || {
            let types: serde_json::Map<String, serde_json::Value> = report
                .types
                .iter()
                .map(|(individual, types)| (individual.clone(), serde_json::json!(types)))
                .collect();
            serde_json::json!({
                "profile": "OWL 2 DL",
                "engine": report.engine,
                "complete": report.complete(),
                "incomplete": report.incomplete,
                "consistent": report.consistent,
                "micros": report.micros,
                "types": types,
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn respond_invokes_only_the_selected_renderer() -> Result<(), Box<dyn std::error::Error>>
    {
        let text = "<urn:s> <urn:p> <urn:o> .\n".to_owned();
        let response = respond(
            true,
            move || text,
            || panic!("JSON renderer invoked for N-Triples"),
        )?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/n-triples"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        assert_eq!(body.as_ref(), b"<urn:s> <urn:p> <urn:o> .\n");

        let json = serde_json::json!({"complete": false, "incomplete": ["budget"]});
        let response = respond(
            false,
            || panic!("N-Triples renderer invoked for JSON"),
            move || json,
        )?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        assert_eq!(
            body.as_ref(),
            br#"{"complete":false,"incomplete":["budget"]}"#
        );
        Ok(())
    }
}
