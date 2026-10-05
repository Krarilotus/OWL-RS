//! One error envelope for every answer (RFC 9457 problem details). Handlers report errors
//! as `ApiError`, which writes the document; what the framework answers on its own (no
//! route, a method the route doesn't take, a body its extractors reject) comes as plain
//! text and is wrapped here the same way. Every document gets the request's id, the one
//! the response carries in `x-request-id` and the logs in the request span, so a client
//! (the console, its CLI) can show it and an operator can find the request.

use axum::extract::Request;
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{Map, Value};

use crate::error::{PROBLEM_JSON, Verbatim, kind, problem};

/// The framework's messages are short: a body beyond this isn't a message, and only its
/// status is reported.
const MAX_BODY: usize = 64 * 1024;

pub(crate) async fn problems(request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let head = request.method() == Method::HEAD;
    let response = next.run(request).await;
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error())
        || head
        || response.extensions().get::<Verbatim>().is_some()
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let documented = parts
        .headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(PROBLEM_JSON.as_bytes()));
    let bytes = axum::body::to_bytes(body, MAX_BODY).await.ok();
    let mut document = match (&bytes, documented) {
        (Some(bytes), true) => match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(document)) => document,
            _ => framework(status, ""),
        },
        (Some(bytes), false) => framework(status, &String::from_utf8_lossy(bytes)),
        (None, _) => framework(status, ""),
    };
    if let Some(id) = id {
        document.insert("request_id".to_owned(), Value::String(id));
    }
    // The original head (`Allow`, `WWW-Authenticate`, `Retry-After`, …), but for the
    // body's own headers.
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_ENCODING);
    parts
        .headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
    let (_, body) = problem(status, &document).into_parts();
    Response::from_parts(parts, body)
}

/// The document for a plain answer: the status's kind, its text as the detail.
fn framework(status: axum::http::StatusCode, text: &str) -> Map<String, Value> {
    let (problem_type, title) = kind(status);
    let detail = match text.trim() {
        "" => title.to_string(),
        text => text.to_owned(),
    };
    let mut document = Map::new();
    document.insert("type".to_owned(), Value::String(problem_type.into_owned()));
    document.insert("title".to_owned(), Value::String(title.into_owned()));
    document.insert("status".to_owned(), Value::from(status.as_u16()));
    document.insert("detail".to_owned(), Value::String(detail));
    document
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::routing::get;
    use tower::util::ServiceExt;

    use super::*;
    use crate::error::ApiError;

    async fn answer(app: Router, method: Method, uri: &str) -> (StatusCode, String, Value) {
        let app = app.layer(axum::middleware::from_fn(problems));
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-request-id", "req-1")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let media = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY)
            .await
            .unwrap();
        let document = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, media, document)
    }

    fn app() -> Router {
        Router::new()
            .route("/ok", get(|| async { "fine" }))
            .route(
                "/missing",
                get(|| async { ApiError::not_found("no such graph") }),
            )
            .route(
                "/plain",
                get(|| async { ApiError::bad_request_plain_text("parse error") }),
            )
            .route(
                "/framework",
                get(|| async { (StatusCode::UNPROCESSABLE_ENTITY, "bad json") }),
            )
    }

    #[tokio::test]
    async fn handler_errors_get_the_request_id() {
        let (status, media, document) = answer(app(), Method::GET, "/missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(media, PROBLEM_JSON);
        assert_eq!(document["type"], "https://nrese.dev/problems/not-found");
        assert_eq!(document["detail"], "no such graph");
        assert_eq!(document["request_id"], "req-1");
    }

    #[tokio::test]
    async fn framework_answers_are_wrapped() {
        let (status, media, document) = answer(app(), Method::GET, "/framework").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(media, PROBLEM_JSON);
        assert_eq!(
            document["type"],
            "https://nrese.dev/problems/unprocessable-entity"
        );
        assert_eq!(document["detail"], "bad json");
        assert_eq!(document["status"], 422);
        assert_eq!(document["request_id"], "req-1");
        // No route, and a method the route doesn't take.
        let (status, media, document) = answer(app(), Method::GET, "/nowhere").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(media, PROBLEM_JSON);
        assert_eq!(document["detail"], "Not Found");
        let (status, media, _) = answer(app(), Method::POST, "/ok").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(media, PROBLEM_JSON);
    }

    #[tokio::test]
    async fn plain_text_profile_and_successes_stay_as_they_are() {
        let (status, media, document) = answer(app(), Method::GET, "/plain").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(media, "text/plain");
        assert_eq!(document, Value::Null);
        let (status, media, _) = answer(app(), Method::GET, "/ok").await;
        assert_eq!(status, StatusCode::OK);
        assert!(media.starts_with("text/plain"));
    }
}
