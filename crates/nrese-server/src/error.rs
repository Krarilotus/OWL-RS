use std::borrow::Cow;

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_reasoner::RejectExplanation;
use serde::Serialize;
use thiserror::Error;

use crate::reject_view::RejectExplanationView;
use nrese_store::RejectAttribution;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("bad request: {0}")]
    BadRequestPlainText(String),
    #[error("reasoner reject: {detail}")]
    ReasonerReject {
        detail: String,
        reject: Box<Option<RejectExplanationView>>,
    },
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// What was asked for is gone for good (a replica's log a checkpoint covered).
    #[error("gone: {0}")]
    Gone(String),
    #[error("not acceptable: {0}")]
    NotAcceptable(String),
    #[error("unsupported media type: {0}")]
    UnsupportedMediaType(String),
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),
    #[error("too many requests: {0}")]
    TooManyRequests(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    #[error("internal server error: {0}")]
    Internal(String),
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub fn bad_request_plain_text(message: impl Into<String>) -> Self {
        Self::BadRequestPlainText(message.into())
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }

    pub fn reasoner_reject(
        message: impl Into<String>,
        reject: Option<RejectExplanation>,
        commit_attribution: Option<RejectAttribution>,
    ) -> Self {
        Self::ReasonerReject {
            detail: message.into(),
            reject: Box::new(reject.as_ref().map(|reject| {
                crate::reject_view::reject_view(reject, commit_attribution.as_ref())
            })),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    /// The request conflicts with the resource's state (it exists already, say).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    /// What was asked for is gone for good (410).
    pub fn gone(message: impl Into<String>) -> Self {
        Self::Gone(message.into())
    }

    /// The client accepts no media type the endpoint can produce.
    pub fn not_acceptable(message: impl Into<String>) -> Self {
        Self::NotAcceptable(message.into())
    }

    /// The request body's media type isn't one the endpoint reads.
    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::UnsupportedMediaType(message.into())
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::PayloadTooLarge(message.into())
    }

    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::TooManyRequests(message.into())
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout(message.into())
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::ServiceUnavailable(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Server faults are logged once, here; the request span carries the request id,
        // which the response returns in `x-request-id` and the problem in `request_id`.
        if let Self::Internal(detail) = &self {
            tracing::error!(%detail, "request failed on the server side");
        }
        let rejected = matches!(self, Self::ReasonerReject { .. });
        let (status, detail, reasoner_reject) = match self {
            Self::BadRequest(detail) => (StatusCode::BAD_REQUEST, detail, None),
            Self::BadRequestPlainText(detail) => {
                let mut response = (StatusCode::BAD_REQUEST, detail).into_response();
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                response.extensions_mut().insert(Verbatim);
                return response;
            }
            Self::ReasonerReject { detail, reject } => (StatusCode::BAD_REQUEST, detail, *reject),
            Self::Unauthorized(detail) => (StatusCode::UNAUTHORIZED, detail, None),
            Self::Forbidden(detail) => (StatusCode::FORBIDDEN, detail, None),
            Self::NotFound(detail) => (StatusCode::NOT_FOUND, detail, None),
            Self::Conflict(detail) => (StatusCode::CONFLICT, detail, None),
            Self::Gone(detail) => (StatusCode::GONE, detail, None),
            Self::NotAcceptable(detail) => (StatusCode::NOT_ACCEPTABLE, detail, None),
            Self::UnsupportedMediaType(detail) => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, detail, None)
            }
            Self::PayloadTooLarge(detail) => (StatusCode::PAYLOAD_TOO_LARGE, detail, None),
            Self::TooManyRequests(detail) => (StatusCode::TOO_MANY_REQUESTS, detail, None),
            Self::Timeout(detail) => (StatusCode::REQUEST_TIMEOUT, detail, None),
            Self::ServiceUnavailable(detail) => (StatusCode::SERVICE_UNAVAILABLE, detail, None),
            Self::Internal(detail) => (StatusCode::INTERNAL_SERVER_ERROR, detail, None),
        };
        let (problem_type, title) = if rejected {
            (
                Cow::Borrowed("https://nrese.dev/problems/reasoner-reject"),
                Cow::Borrowed("Reasoner Reject"),
            )
        } else {
            kind(status)
        };
        problem(
            status,
            &ProblemJson {
                r#type: problem_type,
                title,
                status: status.as_u16(),
                detail,
                reasoner_reject,
            },
        )
    }
}

/// Marks an error status whose body must stay as written: the plain-text profile for
/// SPARQL parse errors, for clients that show the message as text, and the readiness
/// documents a 503 carries (`/readyz`, the extended health), which probes read. The
/// envelope layer (`http::problems`) leaves it alone.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Verbatim;

/// A status's problem type and title: the one place they are named, for the handlers'
/// errors and for what the framework answers on its own.
pub(crate) fn kind(status: StatusCode) -> (Cow<'static, str>, Cow<'static, str>) {
    let named = |slug: &'static str, title: &'static str| {
        (
            Cow::Owned(format!("https://nrese.dev/problems/{slug}")),
            Cow::Borrowed(title),
        )
    };
    match status {
        StatusCode::BAD_REQUEST => named("bad-request", "Bad Request"),
        StatusCode::UNAUTHORIZED => named("unauthorized", "Unauthorized"),
        StatusCode::FORBIDDEN => named("forbidden", "Forbidden"),
        StatusCode::NOT_FOUND => named("not-found", "Not Found"),
        StatusCode::CONFLICT => named("conflict", "Conflict"),
        StatusCode::GONE => named("gone", "Gone"),
        StatusCode::NOT_ACCEPTABLE => named("not-acceptable", "Not Acceptable"),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            named("unsupported-media-type", "Unsupported Media Type")
        }
        StatusCode::PAYLOAD_TOO_LARGE => named("payload-too-large", "Payload Too Large"),
        StatusCode::TOO_MANY_REQUESTS => named("too-many-requests", "Too Many Requests"),
        StatusCode::REQUEST_TIMEOUT => named("timeout", "Request Timeout"),
        StatusCode::SERVICE_UNAVAILABLE => named("service-unavailable", "Service Unavailable"),
        StatusCode::INTERNAL_SERVER_ERROR => named("internal-error", "Internal Error"),
        other => {
            let title = other.canonical_reason().unwrap_or("Error");
            let slug = title.to_ascii_lowercase().replace([' ', '\''], "-");
            (
                Cow::Owned(format!("https://nrese.dev/problems/{slug}")),
                Cow::Borrowed(title),
            )
        }
    }
}

/// A problem document as the answer, with its media type.
pub(crate) fn problem(status: StatusCode, body: &impl Serialize) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
    response
}

pub(crate) const PROBLEM_JSON: &str = "application/problem+json";

#[derive(Debug, Serialize)]
struct ProblemJson {
    r#type: Cow<'static, str>,
    title: Cow<'static, str>,
    status: u16,
    detail: String,
    reasoner_reject: Option<RejectExplanationView>,
}
