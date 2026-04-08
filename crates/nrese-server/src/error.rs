use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_reasoner::RejectExplanation;
use serde::Serialize;
use thiserror::Error;

use crate::reject_attribution::RejectAttribution;
use crate::reject_view::RejectExplanationView;

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
        let (status, problem_type, title, detail, reasoner_reject) = match self {
            Self::BadRequest(detail) => (
                StatusCode::BAD_REQUEST,
                "https://nrese.dev/problems/bad-request",
                "Bad Request",
                detail,
                None,
            ),
            Self::BadRequestPlainText(detail) => {
                let mut response = (StatusCode::BAD_REQUEST, detail).into_response();
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                return response;
            }
            Self::ReasonerReject { detail, reject } => (
                StatusCode::BAD_REQUEST,
                "https://nrese.dev/problems/reasoner-reject",
                "Reasoner Reject",
                detail,
                *reject,
            ),
            Self::Unauthorized(detail) => (
                StatusCode::UNAUTHORIZED,
                "https://nrese.dev/problems/unauthorized",
                "Unauthorized",
                detail,
                None,
            ),
            Self::Forbidden(detail) => (
                StatusCode::FORBIDDEN,
                "https://nrese.dev/problems/forbidden",
                "Forbidden",
                detail,
                None,
            ),
            Self::NotFound(detail) => (
                StatusCode::NOT_FOUND,
                "https://nrese.dev/problems/not-found",
                "Not Found",
                detail,
                None,
            ),
            Self::PayloadTooLarge(detail) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "https://nrese.dev/problems/payload-too-large",
                "Payload Too Large",
                detail,
                None,
            ),
            Self::TooManyRequests(detail) => (
                StatusCode::TOO_MANY_REQUESTS,
                "https://nrese.dev/problems/too-many-requests",
                "Too Many Requests",
                detail,
                None,
            ),
            Self::Timeout(detail) => (
                StatusCode::REQUEST_TIMEOUT,
                "https://nrese.dev/problems/timeout",
                "Request Timeout",
                detail,
                None,
            ),
            Self::ServiceUnavailable(detail) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "https://nrese.dev/problems/service-unavailable",
                "Service Unavailable",
                detail,
                None,
            ),
            Self::Internal(detail) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "https://nrese.dev/problems/internal-error",
                "Internal Error",
                detail,
                None,
            ),
        };

        let body = ProblemJson {
            r#type: problem_type,
            title,
            status: status.as_u16(),
            detail,
            instance: None,
            trace_id: None,
            reasoner_reject,
        };

        let mut response = (status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

#[derive(Debug, Serialize)]
struct ProblemJson {
    r#type: &'static str,
    title: &'static str,
    status: u16,
    detail: String,
    instance: Option<String>,
    trace_id: Option<String>,
    reasoner_reject: Option<RejectExplanationView>,
}
