use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::http::responses::{
    build_extended_health_response, build_operator_capabilities_response, build_ready_response,
};
use crate::state::AppState;

pub fn capabilities(state: AppState) -> Response {
    (
        StatusCode::OK,
        Json(build_operator_capabilities_response(&state)),
    )
        .into_response()
}

pub fn dataset_summary(
    state: AppState,
    identity: &crate::auth::Identity,
) -> Result<Response, ApiError> {
    Ok((
        StatusCode::OK,
        Json(build_ready_response(&state, identity)?),
    )
        .into_response())
}

pub fn extended_health(
    state: AppState,
    identity: &crate::auth::Identity,
) -> Result<Response, ApiError> {
    let ready = build_ready_response(&state, identity)?;
    let status = if state.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    Ok((
        status,
        axum::Extension(crate::error::Verbatim),
        Json(build_extended_health_response(&state, ready)),
    )
        .into_response())
}
