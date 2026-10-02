use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::http::StatusCode;
use axum::http::{HeaderMap, header};

use nrese_store::{MutationCommand, TellRequest};

use crate::access::AccessView;
use crate::error::ApiError;
use crate::http::guard;
use crate::http::media::header_value_str;
use crate::http::mutation;
use crate::http::rdf_payload::{parse_graph_target, parse_rdf_base_iri, parse_tell_content_format};
use crate::state::AppState;

pub async fn execute_tell(
    state: AppState,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
    access: &AccessView,
) -> Result<StatusCode, ApiError> {
    state.ensure_serving()?;
    state.policy().enforce_rdf_upload_bytes(body.len())?;
    let target = parse_graph_target(&raw_query)?;
    guard::check_writable(access, &guard::target_graph(&target)?)?;

    let request = TellRequest {
        target,
        format: parse_tell_content_format(header_value_str(headers.get(header::CONTENT_TYPE)))?,
        base_iri: parse_rdf_base_iri(&headers),
        payload: body.to_vec(),
    };

    mutation::run(
        &state,
        MutationCommand::Tell(request),
        access.requester(),
        state.policy().timeouts.update,
        "tell execution exceeded policy timeout",
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}
