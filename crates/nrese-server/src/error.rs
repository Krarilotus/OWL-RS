use std::borrow::Cow;
use std::io;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_exec::{BudgetExceeded, ChargedVec};
use serde::Serialize;
use thiserror::Error;

use crate::reject_view::RejectExplanationView;
use nrese_store::{MutationReject, RejectKind, ShaclRefusal};

#[derive(Clone, Debug, Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("bad request: {0}")]
    BadRequestPlainText(String),
    /// A commit a gate rejected, labelled by the kind of reason the committer is shown.
    #[error("rejected: {detail}")]
    Rejected {
        kind: RejectKind,
        detail: String,
        reject: Box<Option<RejectExplanationView>>,
        shacl: Option<Arc<ShaclRefusal>>,
    },
    #[error("unauthorized: {0}")]
    Unauthorized(String, Option<&'static str>),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// Exact answers asked for (`dl-answers=exact`) that can't be proven complete (422).
    #[error("incomplete answer: {0}")]
    IncompleteAnswer(Box<nrese_store::IncompleteAnswer>),
    #[error("{1}")]
    NotImplemented(&'static str, String),
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
    /// Busy for now: 503 with `Retry-After`, to be tried again shortly.
    #[error("busy: {0}")]
    Busy(String),
    /// The requester's rights changed while its write waited: 503, `Retry-After`, `rights-changed`.
    #[error("rights changed: {0}")]
    RightsChanged(String),
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
        Self::Unauthorized(message.into(), Some("Bearer"))
    }

    pub fn with_challenge(mut self, challenge: Option<&'static str>) -> Self {
        if let Self::Unauthorized(_, value) = &mut self {
            *value = challenge;
        }
        self
    }

    pub fn rejected(reject: MutationReject) -> Self {
        Self::Rejected {
            kind: reject.kind,
            detail: reject.detail,
            reject: Box::new(reject.explanation.as_ref().map(|explanation| {
                crate::reject_view::reject_view(explanation, reject.attribution.as_ref())
            })),
            shacl: reject.shacl,
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

    /// 503 with `Retry-After: 1`: the request may succeed when tried again.
    pub fn busy(message: impl Into<String>) -> Self {
        Self::Busy(message.into())
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
        if let Self::Rejected {
            kind,
            detail,
            reject,
            shacl,
        } = self
        {
            let (slug, title) = reject_label(kind);
            let body = ProblemJson {
                r#type: Cow::Owned(format!("https://nrese.dev/problems/{slug}")),
                title: Cow::Borrowed(title),
                status: StatusCode::BAD_REQUEST.as_u16(),
                detail,
                reasoner_reject: *reject,
                shacl: shacl.as_deref().map(crate::http::refusal_json),
            };
            return match shacl.as_deref() {
                Some(refusal) => shacl_problem(&body, refusal),
                None => problem(StatusCode::BAD_REQUEST, &body),
            };
        }
        if let Self::IncompleteAnswer(answer) = self {
            return incomplete_answer(&answer);
        }
        if let Self::NotImplemented(setting, detail) = self {
            return problem(
                StatusCode::NOT_IMPLEMENTED,
                &serde_json::json!({
                    "type": format!("https://nrese.dev/problems/{setting}"),
                    "title": "Not Implemented",
                    "status": 501,
                    "detail": detail,
                }),
            );
        }
        let rights_changed = matches!(self, Self::RightsChanged(_));
        if let Self::Busy(detail) = self {
            let mut response = Self::ServiceUnavailable(detail).into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return response;
        }
        let challenge = match &self {
            Self::Unauthorized(_, challenge) => *challenge,
            _ => None,
        };
        let (status, detail) = match self {
            Self::BadRequest(detail) => (StatusCode::BAD_REQUEST, detail),
            Self::BadRequestPlainText(detail) => {
                let mut response = (StatusCode::BAD_REQUEST, detail).into_response();
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                response.extensions_mut().insert(Verbatim);
                return response;
            }
            Self::Unauthorized(detail, _) => (StatusCode::UNAUTHORIZED, detail),
            Self::Forbidden(detail) => (StatusCode::FORBIDDEN, detail),
            Self::NotFound(detail) => (StatusCode::NOT_FOUND, detail),
            Self::Conflict(detail) => (StatusCode::CONFLICT, detail),
            Self::Rejected { .. }
            | Self::IncompleteAnswer(_)
            | Self::Busy(_)
            | Self::NotImplemented(..) => {
                unreachable!("answered above")
            }
            Self::Gone(detail) => (StatusCode::GONE, detail),
            Self::NotAcceptable(detail) => (StatusCode::NOT_ACCEPTABLE, detail),
            Self::UnsupportedMediaType(detail) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, detail),
            Self::PayloadTooLarge(detail) => (StatusCode::PAYLOAD_TOO_LARGE, detail),
            Self::TooManyRequests(detail) => (StatusCode::TOO_MANY_REQUESTS, detail),
            Self::Timeout(detail) => (StatusCode::REQUEST_TIMEOUT, detail),
            Self::ServiceUnavailable(detail) | Self::RightsChanged(detail) => {
                (StatusCode::SERVICE_UNAVAILABLE, detail)
            }
            Self::Internal(detail) => (StatusCode::INTERNAL_SERVER_ERROR, detail),
        };
        let (problem_type, title) = if rights_changed {
            (
                Cow::Borrowed("https://nrese.dev/problems/rights-changed"),
                Cow::Borrowed("Rights Changed"),
            )
        } else {
            kind(status)
        };
        let mut response = problem(
            status,
            &ProblemJson {
                r#type: problem_type,
                title,
                status: status.as_u16(),
                detail,
                reasoner_reject: None,
                shacl: None,
            },
        );
        if let Some(challenge) = challenge {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static(challenge),
            );
        }
        if rights_changed {
            let retry = HeaderValue::from_static("1");
            response.headers_mut().insert(header::RETRY_AFTER, retry);
        }
        response
    }
}

/// A rejected commit's problem type (its slug) and title, by the kind of reason the committer
/// is shown. One whose graphs show no reason gets a label that names no gate: which gate
/// rejected depends on graphs it may not read.
fn reject_label(kind: RejectKind) -> (&'static str, &'static str) {
    match kind {
        RejectKind::Inconsistent => ("reasoner-reject", "Reasoner Reject"),
        RejectKind::Undecided => ("undecided-reject", "Undecided Reject"),
        RejectKind::ShapesViolated => ("shacl-reject", "SHACL Reject"),
        RejectKind::NotShown => ("commit-reject", "Commit Reject"),
    }
}

/// The problem document of exact answers that can't be proven complete: 422, type
/// `incomplete-answer`, with the status's reasons, its bounds and the first unresolved
/// candidates.
fn incomplete_answer(answer: &nrese_store::IncompleteAnswer) -> Response {
    let status = StatusCode::UNPROCESSABLE_ENTITY;
    let completeness = &answer.status;
    problem(
        status,
        &serde_json::json!({
            "type": "https://nrese.dev/problems/incomplete-answer",
            "title": "Incomplete Answer",
            "status": status.as_u16(),
            "detail": format!(
                "the answers can't be proven complete under OWL 2 DL: {answer}"
            ),
            "regime": completeness.regime.map(|r| r.as_str()),
            "reasons": completeness
                .reasons
                .iter()
                .map(|r| serde_json::json!({"source": r.source, "text": r.text}))
                .collect::<Vec<_>>(),
            "bounds": completeness.bounds.map(|b| serde_json::json!({
                "lower": b.lower,
                "upper": b.upper,
                "unresolved": b.unresolved,
            })),
            "unresolved": answer.unresolved,
        }),
    )
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

/// A problem document as the answer, with its media type, marked as the server's own
/// ([`Own`]). `body` writes a JSON object with at least one member and none named
/// `request_id`: the envelope appends the request's id after its last member, unread.
pub(crate) fn problem(status: StatusCode, body: &impl Serialize) -> Response {
    problem_media((status, Json(body)).into_response())
}

fn problem_media(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
    response.extensions_mut().insert(Own);
    response
}

/// The first serialization borrows the refusal's strings, with the store's charge
/// live. The encoded bytes carry their own charge while they are held by the response.
fn shacl_problem(body: &ProblemJson<'_>, refusal: &ShaclRefusal) -> Response {
    let mut encoded = ChargedJson {
        bytes: ChargedVec::new(Arc::clone(refusal.memory())),
        refused: None,
    };
    if let Err(error) = serde_json::to_writer(&mut encoded, body) {
        let refused = encoded.refused.take();
        drop(encoded);
        let error = match refused {
            Some(error) if error.unread => ApiError::unavailable(format!(
                "{error} (budgets.total_query_memory, NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES)"
            )),
            Some(error) if error.shared => ApiError::unavailable(
                "the server's query memory is in use (budgets.total_query_memory, NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES)",
            ),
            Some(error) => ApiError::payload_too_large(format!(
                "{error} (budgets.query_memory, NRESE_MAX_QUERY_MEMORY_BYTES)"
            )),
            None => ApiError::internal(error.to_string()),
        };
        return error.into_response();
    }
    let bytes = Bytes::from_owner(encoded);
    problem_media((StatusCode::BAD_REQUEST, bytes).into_response())
}

struct ChargedJson {
    bytes: ChargedVec<u8>,
    refused: Option<BudgetExceeded>,
}

impl AsRef<[u8]> for ChargedJson {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl io::Write for ChargedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.bytes.extend_from_slice(bytes) {
            self.refused = Some(error);
            return Err(io::Error::other("problem document memory"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Marks a problem document the server wrote itself ([`problem`]): the envelope layer
/// (`http::problems`) reads it whole, at any size, where it reads other bodies only up to
/// the framework's short messages, and passes it as written, as [`problem`] states.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Own;

pub(crate) const PROBLEM_JSON: &str = "application/problem+json";

#[derive(Debug, Serialize)]
struct ProblemJson<'a> {
    r#type: Cow<'static, str>,
    title: Cow<'static, str>,
    status: u16,
    detail: String,
    reasoner_reject: Option<RejectExplanationView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shacl: Option<crate::http::RefusalJson<'a>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retryable refusal (busy, or a write that met a reconfiguration) is told when to send
    /// it again; a plain unavailability isn't.
    #[test]
    fn a_retryable_refusal_says_when_to_retry() {
        let response = ApiError::busy("reconfigured").into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("1"))
        );
        let plain = ApiError::unavailable("down").into_response();
        assert!(plain.headers().get(header::RETRY_AFTER).is_none());
    }

    /// A rejection whose committer's graphs show no reason names no gate: its type is the
    /// same whichever gate rejected (D16-503).
    #[tokio::test]
    async fn a_rejection_with_no_reason_shown_names_no_gate() {
        let response = ApiError::Rejected {
            kind: RejectKind::NotShown,
            detail: "rejected".to_owned(),
            reject: Box::new(None),
            shacl: None,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem json");
        assert_eq!(problem["type"], "https://nrese.dev/problems/commit-reject");
        assert_eq!(problem["title"], "Commit Reject");
    }
}
