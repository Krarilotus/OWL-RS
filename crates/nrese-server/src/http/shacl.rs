//! SHACL validation over HTTP: `GET` validates against the repository's shapes graph,
//! `POST` against the shapes in the body. The answer is the validation report, as RDF or
//! as JSON, whether the data conforms or not; only ill-formed shapes are an error.

use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_store::{
    GraphResultFormat, ReadModel, ShaclValidation, ShaclValidationRequest, ShapesSource,
    ValidatedGraphs,
};

use crate::error::ApiError;
use crate::http::media::{GRAPHS, Offers, content_format, header_value_str, negotiated};
use crate::http::rdf_payload::parse_rdf_base_iri;
use crate::http::requests::accept_header_value;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportFormat {
    Rdf(GraphResultFormat),
    Json,
}

/// The report's formats: Turtle unless the client prefers another.
const REPORTS: Offers<ReportFormat> = &[
    ("text/turtle", ReportFormat::Rdf(GraphResultFormat::Turtle)),
    ("application/json", ReportFormat::Json),
    (
        "application/n-triples",
        ReportFormat::Rdf(GraphResultFormat::NTriples),
    ),
    (
        "application/rdf+xml",
        ReportFormat::Rdf(GraphResultFormat::RdfXml),
    ),
    (
        "application/ld+json",
        ReportFormat::Rdf(GraphResultFormat::JsonLd),
    ),
    (
        "application/n-quads",
        ReportFormat::Rdf(GraphResultFormat::NQuads),
    ),
    (
        "application/trig",
        ReportFormat::Rdf(GraphResultFormat::TriG),
    ),
];

/// `graph=<IRI>` or `default` (else every graph but the shapes), `shapes-graph=<IRI>`
/// (stored shapes other than the configured graph), `infer=false` (asserted statements
/// only).
fn request_from(raw_query: Option<&str>) -> Result<ShaclValidationRequest, ApiError> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw_query.unwrap_or_default())
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let mut request = ShaclValidationRequest::default();
    for (key, value) in pairs {
        match key.as_str() {
            "graph" | "default" if request.graphs != ValidatedGraphs::AllData => {
                return Err(ApiError::bad_request(
                    "give one graph, or default, or neither",
                ));
            }
            "graph" => request.graphs = ValidatedGraphs::Named(value),
            "default" => request.graphs = ValidatedGraphs::Default,
            "shapes-graph" => request.shapes = ShapesSource::Graph(value),
            "infer" => match value.as_str() {
                "true" => request.read_model = ReadModel::Materialised,
                "false" => request.read_model = ReadModel::Asserted,
                other => {
                    return Err(ApiError::bad_request(format!(
                        "infer must be true or false, not '{other}'"
                    )));
                }
            },
            _ => {}
        }
    }
    Ok(request)
}

pub async fn validate(
    state: AppState,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    shapes: Option<Bytes>,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let format = negotiated(accept_header_value(&headers), REPORTS)?;
    let mut request = request_from(raw_query.as_deref())?;
    if let Some(payload) = shapes {
        state.policy().enforce_rdf_upload_bytes(payload.len())?;
        let content_type = header_value_str(headers.get(header::CONTENT_TYPE));
        let shapes_format = content_format(content_type, GRAPHS)
            .filter(|_| !crate::http::media::media_type_matches(content_type, "text/plain"))
            .ok_or_else(|| {
                ApiError::unsupported_media_type(format!(
                    "send the shapes as an RDF graph, not '{}'",
                    content_type.unwrap_or_default()
                ))
            })?;
        request.shapes = ShapesSource::Payload {
            format: shapes_format,
            base_iri: parse_rdf_base_iri(&headers),
            payload: payload.to_vec(),
        };
    }

    let store = state.store();
    let validation = tokio::time::timeout(
        state.policy().timeouts.query,
        tokio::task::spawn_blocking(move || store.validate_shacl(&request)),
    )
    .await
    .map_err(|_| ApiError::timeout("validation exceeded policy timeout"))?
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| {
        if error.is_request_error() {
            ApiError::bad_request(error.to_string())
        } else {
            ApiError::internal(error.to_string())
        }
    })?;

    let (media_type, body) = match format {
        ReportFormat::Json => (
            "application/json",
            serde_json::to_vec(&report_json(&validation))
                .map_err(|error| ApiError::internal(error.to_string()))?,
        ),
        ReportFormat::Rdf(format) => (
            format.media_type(),
            validation
                .serialize(format)
                .map_err(|error| ApiError::internal(error.to_string()))?,
        ),
    };
    let mut response = (StatusCode::OK, body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    Ok(response)
}

/// The JSON report: `conforms`, what was validated, one object per result with SHACL's
/// property names, and the failures (SPARQL-based constraints that couldn't be
/// evaluated; `conforms` is then false).
fn report_json(validation: &ShaclValidation) -> serde_json::Value {
    let results: Vec<serde_json::Value> = validation
        .results_text()
        .into_iter()
        .map(|result| {
            serde_json::json!({
                "focusNode": result.focus_node,
                "resultPath": result.path,
                "value": result.value,
                "sourceShape": result.source_shape,
                "sourceConstraintComponent": result.component,
                "resultSeverity": result.severity,
                "resultMessage": result.messages,
            })
        })
        .collect();
    serde_json::json!({
        "conforms": validation.report.conforms(),
        "revision": validation.revision,
        "shapes": validation.shapes,
        "results": results,
        "failures": validation.report.failures,
    })
}

#[cfg(test)]
mod tests {
    use nrese_store::{ReadModel, ShapesSource, ValidatedGraphs};

    use super::request_from;

    #[test]
    fn parameters_select_what_is_validated() {
        let request = request_from(None).expect("default");
        assert_eq!(request.graphs, ValidatedGraphs::AllData);
        assert_eq!(request.shapes, ShapesSource::Stored);
        assert_eq!(request.read_model, ReadModel::Materialised);

        let request = request_from(Some(
            "graph=http%3A%2F%2Fex%2Fg&shapes-graph=http%3A%2F%2Fex%2Fshapes&infer=false",
        ))
        .expect("parameters");
        assert_eq!(
            request.graphs,
            ValidatedGraphs::Named("http://ex/g".to_owned())
        );
        assert_eq!(
            request.shapes,
            ShapesSource::Graph("http://ex/shapes".to_owned())
        );
        assert_eq!(request.read_model, ReadModel::Asserted);

        assert_eq!(
            request_from(Some("default")).expect("default graph").graphs,
            ValidatedGraphs::Default
        );
        assert!(request_from(Some("default&graph=http%3A%2F%2Fex%2Fg")).is_err());
        assert!(request_from(Some("infer=maybe")).is_err());
    }
}
