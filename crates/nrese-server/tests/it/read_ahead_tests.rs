//! The authentication middleware reads a small body (a known length up to 64 KiB) before it
//! answers. That read ends within a bound, whatever the client sends or withholds: a client
//! that declares a body and never sends it gets its answer, not none. The clock is paused:
//! tokio moves it to the next timer once nothing else can run, so a wait passes without real
//! time, and a request still unanswered a virtual day later waits forever.

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use tower::util::ServiceExt;

use nrese_server::auth::{AuthConfig, StaticBearerConfig};
use nrese_server::policy::PolicyConfig;

use crate::support::test_app_with_policy;

const UPDATE: &str = "INSERT DATA { <urn:s> <urn:p> <urn:o> }";

/// Far past any wait the middleware has.
const NEVER: Duration = Duration::from_secs(24 * 3600);

/// An update to the default repository with bearer `token` (none: no credentials),
/// declaring `length` bytes of body and sending what `body` yields; an error if no answer
/// comes before [`NEVER`].
async fn send(token: Option<&str>, length: usize, body: Body) -> Result<Response, String> {
    let app = test_app_with_policy(PolicyConfig {
        auth: AuthConfig::BearerStatic(StaticBearerConfig {
            read_token: Some("reader".to_owned()),
            admin_token: "admin".to_owned(),
        }),
        ..PolicyConfig::default()
    })
    .map_err(|error| error.to_string())?;
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/dataset/update");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .header("content-type", "application/sparql-update")
        .header("content-length", length.to_string())
        .body(body)
        .map_err(|error| error.to_string())?;
    match tokio::time::timeout(NEVER, app.oneshot(request)).await {
        Ok(response) => response.map_err(|error| error.to_string()),
        Err(_) => Err(format!("no answer in {NEVER:?}")),
    }
}

/// A body declared and never sent.
fn withheld() -> Body {
    Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>())
}

/// Credentials the authentication refuses (none at all), one byte declared and withheld:
/// their refusal, 401.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_withheld_body_without_credentials_gets_their_refusal() {
    let response = send(None, 1, withheld()).await;
    assert_eq!(
        response.map(|response| response.status()),
        Ok(StatusCode::UNAUTHORIZED)
    );
}

/// Credentials the authentication accepts, one byte declared and withheld: 408 (the
/// handler can't run without the body the middleware has started to read). A token the
/// configuration doesn't list is accepted here, as one with no grants: the handler's
/// guard refuses it, so its answer is 408 too.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_withheld_body_with_accepted_credentials_times_out() {
    let mut wrong = Vec::new();
    for token in ["admin", "not-listed"] {
        let status = send(Some(token), 1, withheld())
            .await
            .map(|response| response.status());
        if status != Ok(StatusCode::REQUEST_TIMEOUT) {
            wrong.push(format!("{token}: {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Guard (passes without the bound too): a body that comes a second late is read and the
/// update runs, as clients that send the headers and the body apart need.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_body_a_second_late_is_read_and_handled() {
    let late = futures_util::stream::once(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        Ok::<_, std::io::Error>(Bytes::from_static(UPDATE.as_bytes()))
    });
    let response = send(Some("admin"), UPDATE.len(), Body::from_stream(late)).await;
    let status = response.map(|response| response.status());
    assert!(
        status.as_ref().is_ok_and(StatusCode::is_success),
        "{status:?}"
    );
}
