use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::http::StatusCode;
use axum::http::{HeaderMap, header};

use nrese_store::TellRequest;

use crate::error::ApiError;
use crate::http::media::header_value_str;
use crate::http::rdf_payload::{
    ensure_ready, parse_graph_target, parse_rdf_base_iri, parse_tell_content_format,
};
use crate::state::AppState;
use crate::tell_pipeline;

pub async fn execute_tell(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    ensure_ready(&state)?;
    state.policy().enforce_rdf_upload_bytes(body.len())?;

    let request = TellRequest {
        target: parse_graph_target(&raw_query)?,
        format: parse_tell_content_format(header_value_str(headers.get(header::CONTENT_TYPE)))?,
        base_iri: parse_rdf_base_iri(&headers),
        payload: body.to_vec(),
    };

    tokio::time::timeout(
        state.policy().timeouts.update,
        tell_pipeline::execute(state, request),
    )
    .await
    .map_err(|_| ApiError::timeout("tell execution exceeded policy timeout"))??;

    Ok(StatusCode::NO_CONTENT)
}
