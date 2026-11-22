//! One error envelope for every answer (RFC 9457 problem details). Handlers report errors
//! as `ApiError`, which writes the document; what the framework answers on its own (no
//! route, a method the route doesn't take, a body its extractors reject) comes as plain
//! text and is wrapped here the same way. Every document gets the request's id, the one
//! the response carries in `x-request-id` and the logs in the request span, so a client
//! (the console, its CLI) can show it and an operator can find the request.

use axum::http::{HeaderValue, Method, header};
use axum::response::Response;
use serde_json::{Map, Value};

use crate::error::{Own, PROBLEM_JSON, Verbatim, kind, problem};

/// The framework's messages are short: a body beyond this isn't a message, and only its
/// status is reported. The server's own documents ([`Own`]) are read whole: what one holds
/// (a refusal's values, a justification) is what it wrote, at any size.
const MAX_BODY: usize = 64 * 1024;

/// A response mapped on its way out (`map_response`, outside the request id's propagation):
/// an answer that isn't an error passes untouched, and an error reads the id its response
/// carries, so no request pays for the id but the errors.
pub(crate) async fn problems(method: Method, response: Response) -> Response {
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error())
        || method == Method::HEAD
        || response.extensions().get::<Verbatim>().is_some()
    {
        return response;
    }
    let id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let (mut parts, body) = response.into_parts();
    let documented = parts
        .headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(PROBLEM_JSON.as_bytes()));
    let own = parts.extensions.get::<Own>().is_some();
    let limit = if own { usize::MAX } else { MAX_BODY };
    let bytes = axum::body::to_bytes(body, limit).await.ok();
    // The server's own document passes as it was written, its request id appended: never
    // decoded and written again, which would hold two more copies of it, uncharged.
    if own
        && documented
        && let Some(bytes) = &bytes
        && let Some(body) = with_request_id(bytes, id.as_deref())
    {
        parts.headers.remove(header::CONTENT_LENGTH);
        parts.headers.remove(header::CONTENT_ENCODING);
        return Response::from_parts(parts, body);
    }
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

/// `document`, a JSON object as [`problem`] writes it, with `"request_id"` as its last
/// member, as a body of two chunks: the document's bytes up to its closing brace (shared,
/// not copied) and the member. `None` if it isn't an object's text.
fn with_request_id(document: &axum::body::Bytes, id: Option<&str>) -> Option<axum::body::Body> {
    let (Some(b'{'), Some(b'}')) = (document.first(), document.last()) else {
        return None;
    };
    let Some(id) = id else {
        return Some(axum::body::Body::from(document.clone()));
    };
    let id = serde_json::to_string(id).ok()?;
    let empty = document.len() == 2;
    let tail = format!("{}\"request_id\":{id}}}", if empty { "" } else { "," });
    let head = document.slice(..document.len() - 1);
    let chunks = [head, axum::body::Bytes::from(tail)].map(Ok::<_, std::convert::Infallible>);
    Some(axum::body::Body::from_stream(futures_util::stream::iter(
        chunks,
    )))
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
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::util::ServiceExt;
    use tower_http::request_id::PropagateRequestIdLayer;

    use super::*;
    use crate::error::ApiError;

    /// As the server layers it ([`crate::app::build_app`]): outside the id's propagation.
    async fn respond(app: Router, method: Method, uri: &str) -> Response {
        let app = app
            .layer(PropagateRequestIdLayer::x_request_id())
            .layer(axum::middleware::map_response(problems));
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-request-id", "req-1")
            .body(Body::empty())
            .unwrap();
        app.oneshot(request).await.unwrap()
    }

    async fn answer(app: Router, method: Method, uri: &str) -> (StatusCode, String, Value) {
        let response = respond(app, method, uri).await;
        let status = response.status();
        let media = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
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
            .route(
                "/long-refusal",
                get(|| async {
                    ApiError::Rejected {
                        kind: nrese_store::RejectKind::ShapesViolated,
                        detail: long_text(2 * KIB_64),
                        reject: Box::new(None),
                        shacl: None,
                    }
                }),
            )
            .route(
                "/empty",
                get(|| async { (StatusCode::UNPROCESSABLE_ENTITY, "") }),
            )
            .route(
                "/own-as-written",
                get(|| async {
                    let media = [(header::CONTENT_TYPE, PROBLEM_JSON)];
                    let mut response =
                        (StatusCode::CONFLICT, media, OWN_AS_WRITTEN).into_response();
                    response.extensions_mut().insert(Own);
                    response
                }),
            )
            .route("/at-limit", get(|| async { plain(KIB_64) }))
            .route("/past-limit", get(|| async { plain(KIB_64 + 1) }))
            .route(
                "/past-limit-unmarked-problem",
                get(|| async {
                    let document = serde_json::json!({"detail": long_text(KIB_64)});
                    let media = [(header::CONTENT_TYPE, PROBLEM_JSON)];
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        media,
                        document.to_string(),
                    )
                }),
            )
    }

    /// The limit the docs name for a framework's message: 64 KiB.
    const KIB_64: usize = 65_536;

    /// A document of the server's own, written with spaces a decode and new encoding
    /// would not keep.
    const OWN_AS_WRITTEN: &str =
        r#"{"type": "https://nrese.dev/problems/conflict", "detail": "x"}"#;

    /// Spec (docs/ops/http-api.md, the error envelope): the server's own document comes
    /// whole, with `request_id`. The envelope passes it as written, the member appended:
    /// it doesn't decode it and write it again, which would hold two more copies of a
    /// large one, outside the request's memory.
    #[tokio::test]
    async fn own_documents_pass_as_written_with_the_request_id_appended() {
        let response = respond(app(), Method::GET, "/own-as-written").await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let expected = OWN_AS_WRITTEN.replace("\"x\"}", "\"x\",\"request_id\":\"req-1\"}");
        assert_eq!(String::from_utf8_lossy(&bytes), expected);
        assert_eq!(status, StatusCode::CONFLICT);
    }

    /// `len` bytes of text, as a refusal that quotes long values has.
    fn long_text(len: usize) -> String {
        "x".repeat(len)
    }

    /// A body the server didn't write, of `len` bytes.
    fn plain(len: usize) -> (StatusCode, String) {
        (StatusCode::UNPROCESSABLE_ENTITY, long_text(len))
    }

    /// Spec (docs/ops/http-api.md, the error envelope): every error is a problem document
    /// with its type and detail, a handler's whole however long its detail. A SHACL refusal
    /// past 64 KiB keeps its type and its detail, not its status's bare document.
    #[tokio::test]
    async fn own_documents_pass_whole_past_the_frameworks_limit() {
        let (status, media, document) = answer(app(), Method::GET, "/long-refusal").await;
        assert_eq!(document["type"], "https://nrese.dev/problems/shacl-reject");
        assert_eq!(
            document["detail"],
            long_text(2 * KIB_64),
            "the whole detail"
        );
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(media, PROBLEM_JSON);
        assert_eq!(document["request_id"], "req-1");
    }

    /// Spec (docs/ops/http-api.md, the error envelope): a body the server didn't write is
    /// its detail up to 64 KiB; past that, the answer is the problem of its status alone,
    /// as for an empty body, with the request id. A problem document's media type alone
    /// doesn't make it the server's own.
    #[tokio::test]
    async fn other_bodies_past_64_kib_report_only_their_status() {
        let (_, _, bare) = answer(app(), Method::GET, "/empty").await;
        assert_eq!(
            bare["request_id"], "req-1",
            "the bare problem, its id included"
        );
        let (_, _, at) = answer(app(), Method::GET, "/at-limit").await;
        assert_eq!(
            at["detail"],
            long_text(KIB_64),
            "a body of 64 KiB is its detail"
        );
        for uri in ["/past-limit", "/past-limit-unmarked-problem"] {
            let (status, media, document) = answer(app(), Method::GET, uri).await;
            assert_eq!(document, bare, "{uri}: the status's problem alone");
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
            assert_eq!(media, PROBLEM_JSON, "{uri}");
        }
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
