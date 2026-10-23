//! SHACL validation over HTTP: `GET` validates against the repository's shapes graph,
//! `POST` against the shapes in the body. The answer is the validation report, as RDF or
//! as JSON, whether the data conforms or not; only ill-formed shapes are an error.

use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_exec::{ChargedVec, Reservation};
use nrese_store::{
    CancellationToken, GraphResultFormat, ReadModel, ShaclValidationRequest, ShapesSource,
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
    scope: nrese_store::ReadScope,
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
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let (media_type, body) =
        super::cancellable::blocking(state.policy().timeouts.query, cancellation, move || {
            let validation = store
                .validate_shacl(&scope, &request, &token)
                .map_err(|error| {
                    if error.is_request_error() {
                        ApiError::bad_request(error.to_string())
                    } else {
                        ApiError::internal(error.to_string())
                    }
                })?;
            // Encoding is part of the same cancellable worker, not async runtime work.
            match format {
                ReportFormat::Json => validation
                    .serialize_json()
                    .and_then(report_bytes)
                    .map(|body| ("application/json", body)),
                // RDF serializer-internal admission remains its owner's separate slice.
                ReportFormat::Rdf(format) => validation
                    .serialize(format)
                    .map(|body| (format.media_type(), Bytes::from(body))),
            }
            .map_err(|error| ApiError::internal(error.to_string()))
        })
        .await
        .map_err(|_| ApiError::timeout("validation exceeded policy timeout"))?
        .map_err(|error| ApiError::internal(error.to_string()))??;
    let mut response = (StatusCode::OK, body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    Ok(response)
}

// Bytes owns this backing, including its lease, through every slice/clone/frame. A
// response extension would release the lease before detached body data is consumed.
struct ReportBytes {
    bytes: Vec<u8>,
    _payload: Reservation,
    _backing: Reservation,
}

impl AsRef<[u8]> for ReportBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

fn report_bytes(bytes: ChargedVec<u8>) -> nrese_store::StoreResult<Bytes> {
    let (bytes, payload) = bytes.into_parts();
    let backing = Reservation::new(payload.handle().clone());
    // Keep the payload and its lease together on header-admission refusal too.
    let mut owner = ReportBytes {
        bytes,
        _payload: payload,
        _backing: backing,
    };
    // Pinned bytes 1.11.1 uses repr(C) Owned<T> { AtomicUsize, owner: T }.
    // Admit its one backing allocation before Bytes::from_owner allocates it.
    let (layout, _) = std::alloc::Layout::new::<std::sync::atomic::AtomicUsize>()
        .extend(std::alloc::Layout::new::<ReportBytes>())
        .expect("fixed report backing layout");
    owner._backing.charge(layout.pad_to_align().size())?;
    Ok(Bytes::from_owner(owner))
}

#[cfg(test)]
mod tests {
    use nrese_store::{ReadModel, ShapesSource, ValidatedGraphs};

    use super::request_from;

    /// The output lease follows detached frames and slices; a failed backing admission
    /// frees the original payload before releasing its charge.
    #[tokio::test]
    async fn report_body_holds_its_bytes_until_the_last_slice_drops() {
        use axum::body::Body;
        use http_body_util::BodyExt;
        use nrese_exec::{
            Budget, ChargedVec, Envelope,
            measure::{alone, measure_within},
        };

        if !alone() {
            return;
        }
        let memory = Envelope::new(Some(1024), None);
        let mut bytes = ChargedVec::new(std::sync::Arc::new(memory.part()));
        bytes.extend_from_slice(b"report").unwrap();
        let mut body = Body::from(super::report_bytes(bytes).unwrap());
        assert!(memory.used() >= 6);
        let data = body.frame().await.unwrap().unwrap().into_data().unwrap();
        drop(body);
        assert!(memory.used() >= 6, "a detached frame still owns its bytes");
        let slice = data.slice(1..);
        let copy = slice.clone();
        drop(data);
        drop(slice);
        assert_eq!(&copy[..], b"eport");
        assert!(memory.used() >= 6, "the last slice still owns the backing");
        drop(copy);
        assert_eq!(memory.used(), 0);

        let budget = std::sync::Arc::new(Budget::new(6));
        let (error, uncovered) = measure_within(&budget, || {
            let mut bytes = ChargedVec::new(std::sync::Arc::clone(&budget));
            bytes.extend_from_slice(b"report").unwrap();
            super::report_bytes(bytes).unwrap_err()
        });
        assert!(error.is_memory_limit());
        assert_eq!(budget.used(), 0, "partial output was freed after refusal");
        assert_eq!(
            uncovered, 0,
            "refusal must free the payload before releasing its reservation"
        );
    }

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
