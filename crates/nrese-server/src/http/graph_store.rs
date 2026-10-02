use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_store::{GraphReadRequest, GraphWriteRequest, MutationCommand, MutationCommitReport};

use crate::access::AccessView;
use crate::error::ApiError;
use crate::http::guard;
use crate::http::media::{GRAPHS, header_value_str, negotiated};
use crate::http::mutation;
use crate::http::rdf_payload::{
    parse_graph_content_format, parse_graph_target, parse_rdf_base_iri,
};
use crate::state::AppState;

pub async fn get_graph(
    state: AppState,
    authenticated: crate::auth::Authenticated,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let access = guard::graph_read_access(&state, &authenticated).await?;
    let result = read_graph(state, raw_query, headers, &access).await?;

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
    authenticated: crate::auth::Authenticated,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let access = guard::graph_read_access(&state, &authenticated).await?;
    let result = read_graph(state, raw_query, headers, &access).await?;
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
    access: &AccessView,
) -> Result<nrese_store::GraphReadResult, ApiError> {
    state.ensure_serving()?;

    let target = parse_graph_target(&raw_query)?;
    // A graph the requester may not read is absent.
    if !access.can_read(&guard::target_graph(&target)?) {
        return Err(ApiError::not_found("the graph does not exist"));
    }
    let format = negotiated(header_value_str(headers.get(header::ACCEPT)), GRAPHS)?;
    let request = GraphReadRequest { target, format };
    let store = state.store();
    let read = nrese_store::ReadContext::new(access.read_scope());
    let result = tokio::time::timeout(
        state.policy().timeouts.graph_read,
        tokio::task::spawn_blocking(move || store.execute_graph_read(&read, &request)),
    )
    .await
    .map_err(|_| ApiError::timeout("graph read exceeded policy timeout"))?
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::bad_request(error.to_string()))?;
    // Graph Store Protocol §5.2: a graph that doesn't exist is 404, not an empty document.
    if result.exists {
        Ok(result)
    } else {
        Err(ApiError::not_found("the graph does not exist"))
    }
}

pub async fn put_graph(
    state: AppState,
    authenticated: crate::auth::Authenticated,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    write_graph(state, authenticated, raw_query, headers, body, true).await
}

pub async fn post_graph(
    state: AppState,
    authenticated: crate::auth::Authenticated,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    write_graph(state, authenticated, raw_query, headers, body, false).await
}

pub async fn delete_graph(
    state: AppState,
    authenticated: crate::auth::Authenticated,
    raw_query: RawQuery,
) -> Result<StatusCode, ApiError> {
    state.ensure_serving()?;
    let access = guard::graph_write_access(&state, &authenticated).await?;
    let target = parse_graph_target(&raw_query)?;
    guard::check_writable(&access, &guard::target_graph(&target)?)?;
    let report = mutation::run(
        &state,
        MutationCommand::GraphDelete(target),
        access.requester(),
        state.policy().timeouts.graph_write,
        "graph delete exceeded policy timeout",
    )
    .await?;
    // Graph Store Protocol §5.4: deleting a named graph that doesn't exist is 404.
    match report {
        MutationCommitReport::GraphDelete(report)
            if !report.modified
                && matches!(report.target, nrese_store::GraphTarget::NamedGraph(_)) =>
        {
            Err(ApiError::not_found("the graph does not exist"))
        }
        _ => Ok(StatusCode::NO_CONTENT),
    }
}

async fn write_graph(
    state: AppState,
    authenticated: crate::auth::Authenticated,
    raw_query: RawQuery,
    headers: HeaderMap,
    body: Bytes,
    replace: bool,
) -> Result<StatusCode, ApiError> {
    state.ensure_serving()?;
    let access = guard::graph_write_access(&state, &authenticated).await?;
    state.policy().enforce_rdf_upload_bytes(body.len())?;
    let target = parse_graph_target(&raw_query)?;
    guard::check_writable(&access, &guard::target_graph(&target)?)?;
    let format = parse_graph_content_format(header_value_str(headers.get(header::CONTENT_TYPE)))?;

    let request = GraphWriteRequest {
        target,
        format,
        base_iri: parse_rdf_base_iri(&headers),
        payload: body.to_vec(),
        replace,
    };
    let report = match mutation::run(
        &state,
        MutationCommand::GraphWrite(request),
        access.requester(),
        state.policy().timeouts.graph_write,
        "graph write exceeded policy timeout",
    )
    .await?
    {
        MutationCommitReport::GraphWrite(report) => report,
        other => {
            return Err(ApiError::internal(format!(
                "unexpected graph write result: {other:?}"
            )));
        }
    };

    Ok(write_graph_status(&report))
}

fn write_graph_status(report: &nrese_store::GraphWriteReport) -> StatusCode {
    match &report.target {
        nrese_store::GraphTarget::NamedGraph(_) if report.created => StatusCode::CREATED,
        nrese_store::GraphTarget::NamedGraph(_) => StatusCode::OK,
        nrese_store::GraphTarget::DefaultGraph => StatusCode::NO_CONTENT,
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::RawQuery;
    use axum::http::StatusCode;
    use nrese_store::GraphWriteReport;

    use crate::http::rdf_payload::parse_graph_target;

    use super::write_graph_status;

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
