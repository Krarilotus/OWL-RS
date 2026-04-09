use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_store::{GraphReadRequest, GraphWriteRequest};

use crate::error::ApiError;
use crate::http::guard;
use crate::http::media::{header_value_str, media_type_matches};
use crate::http::rdf_payload::{
    ensure_ready, parse_graph_content_format, parse_graph_target, parse_rdf_base_iri,
};
use crate::mutation_pipeline;
use crate::state::AppState;

pub async fn get_graph(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_graph_read(&state, &headers).await?;
    let result = read_graph(state, raw_query, headers).await?;

    let mut response = (StatusCode::OK, result.payload).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(result.media_type)
            .map_err(|error| ApiError::internal(error.to_string()))?,
    );

    Ok(response)
}

pub async fn head_graph(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    guard::enforce_graph_read(&state, &headers).await?;
    let result = read_graph(state, raw_query, headers).await?;
    let mut response = StatusCode::OK.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(result.media_type)
            .map_err(|error| ApiError::internal(error.to_string()))?,
    );

    Ok(response)
}

async fn read_graph(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<nrese_store::GraphReadResult, ApiError> {
    ensure_ready(&state)?;

    let target = parse_graph_target(&raw_query)?;
    let format = parse_graph_accept_format(header_value_str(headers.get(header::ACCEPT)));
    let request = GraphReadRequest { target, format };
    let store = state.store();
    tokio::time::timeout(
        state.policy().timeouts.graph_read,
        tokio::task::spawn_blocking(move || store.execute_graph_read(&request)),
    )
    .await
    .map_err(|_| ApiError::timeout("graph read exceeded policy timeout"))?
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::bad_request(error.to_string()))
}

pub async fn put_graph(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    write_graph(state, raw_query, headers, body, true).await
}

pub async fn post_graph(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    write_graph(state, raw_query, headers, body, false).await
}

pub async fn delete_graph(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    ensure_ready(&state)?;
    guard::enforce_graph_write(&state, &headers).await?;
    let target = parse_graph_target(&raw_query)?;
    tokio::time::timeout(
        state.policy().timeouts.graph_write,
        mutation_pipeline::execute_graph_delete(state, target),
    )
    .await
    .map_err(|_| ApiError::timeout("graph delete exceeded policy timeout"))??;

    Ok(StatusCode::NO_CONTENT)
}

async fn write_graph(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
    replace: bool,
) -> Result<StatusCode, ApiError> {
    ensure_ready(&state)?;
    guard::enforce_graph_write(&state, &headers).await?;
    state.policy().enforce_rdf_upload_bytes(body.len())?;
    let target = parse_graph_target(&raw_query)?;
    let format = parse_graph_content_format(header_value_str(headers.get(header::CONTENT_TYPE)))?;

    let request = GraphWriteRequest {
        target,
        format,
        base_iri: parse_rdf_base_iri(&headers),
        payload: body.to_vec(),
        replace,
    };
    let report = tokio::time::timeout(
        state.policy().timeouts.graph_write,
        mutation_pipeline::execute_graph_write(state, request),
    )
    .await
    .map_err(|_| ApiError::timeout("graph write exceeded policy timeout"))??;

    Ok(write_graph_status(&report))
}

fn write_graph_status(report: &nrese_store::GraphWriteReport) -> StatusCode {
    match &report.target {
        nrese_store::GraphTarget::NamedGraph(_) if report.created => StatusCode::CREATED,
        nrese_store::GraphTarget::NamedGraph(_) => StatusCode::OK,
        nrese_store::GraphTarget::DefaultGraph => StatusCode::NO_CONTENT,
    }
}

fn parse_graph_accept_format(accept: Option<&str>) -> nrese_store::GraphResultFormat {
    if media_type_matches(accept, "application/rdf+xml") {
        nrese_store::GraphResultFormat::RdfXml
    } else if media_type_matches(accept, "text/turtle")
        || media_type_matches(accept, "application/x-turtle")
    {
        nrese_store::GraphResultFormat::Turtle
    } else {
        nrese_store::GraphResultFormat::NTriples
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::RawQuery;
    use axum::http::StatusCode;
    use nrese_store::GraphResultFormat;
    use nrese_store::GraphWriteReport;

    use crate::http::rdf_payload::parse_graph_target;

    use super::{parse_graph_accept_format, write_graph_status};

    #[test]
    fn graph_target_defaults_to_default_graph() {
        let target = parse_graph_target(&RawQuery(None)).expect("target should parse");
        assert_eq!(target, nrese_store::GraphTarget::DefaultGraph);
    }

    #[test]
    fn graph_target_accepts_named_graph_parameter() {
        let target = parse_graph_target(&RawQuery(Some(
            "graph=http%3A%2F%2Fexample.com%2Fg".to_owned(),
        )))
        .expect("target should parse");
        assert_eq!(
            target,
            nrese_store::GraphTarget::NamedGraph("http://example.com/g".to_owned())
        );
    }

    #[test]
    fn graph_target_rejects_conflicting_default_and_graph() {
        let result = parse_graph_target(&RawQuery(Some(
            "default=&graph=http%3A%2F%2Fexample.com%2Fg".to_owned(),
        )));
        assert!(result.is_err());
    }

    #[test]
    fn graph_accept_format_handles_parameterized_accept_values() {
        let format = parse_graph_accept_format(Some("application/n-triples, text/turtle; q=0.9"));
        assert_eq!(format, GraphResultFormat::Turtle);
    }

    #[test]
    fn graph_accept_format_prefers_rdf_xml_when_present() {
        let format =
            parse_graph_accept_format(Some("application/n-triples, application/rdf+xml; q=0.9"));
        assert_eq!(format, GraphResultFormat::RdfXml);
    }

    #[test]
    fn write_graph_status_returns_created_for_new_named_graphs() {
        let status = write_graph_status(&GraphWriteReport {
            target: nrese_store::GraphTarget::NamedGraph("http://example.com/g".to_owned()),
            modified: true,
            created: true,
            revision: 1,
        });

        assert_eq!(status, StatusCode::CREATED);
    }

    #[test]
    fn write_graph_status_returns_ok_for_existing_named_graphs() {
        let status = write_graph_status(&GraphWriteReport {
            target: nrese_store::GraphTarget::NamedGraph("http://example.com/g".to_owned()),
            modified: true,
            created: false,
            revision: 2,
        });

        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn write_graph_status_returns_no_content_for_default_graph() {
        let status = write_graph_status(&GraphWriteReport {
            target: nrese_store::GraphTarget::DefaultGraph,
            modified: true,
            created: false,
            revision: 3,
        });

        assert_eq!(status, StatusCode::NO_CONTENT);
    }
}
