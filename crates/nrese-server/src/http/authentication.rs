//! Authentication runs once per request, before the handler and before its body is read:
//! [`authenticate`] is a middleware on every route but the public ones ([`super::routes`]),
//! so a request without valid credentials is refused before the server buffers more of
//! what it sends than a small body. Handlers take the result as an extractor ([`Authenticated`]) and check their
//! action against it ([`super::guard`]).
//!
//! A small body (a known length up to [`READ_AHEAD`]) is read whole before the answer,
//! whether or not the handler (or the refusal) needs it: an answer sent while the client
//! still sends would close the connection under it, and clients that send the headers and
//! the body apart (Python's `http.client`, for one) would see it aborted rather than the
//! answer (found by the HTTP soak: 1 in 20 `POST …/sessions` with `{}` failed so).

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::auth::Authenticated;
use crate::error::ApiError;
use crate::state::AppState;

/// Establishes who the request is and keeps it for the handler; 401 for missing or
/// invalid credentials.
pub async fn authenticate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let client = client_address(&state, request.extensions(), request.headers());
    let checked = state.authenticate(request.headers(), client).await;
    let mut request = match read_ahead(request).await {
        Ok(request) => request,
        Err(error) => {
            return ApiError::bad_request(format!("the request body: {error}")).into_response();
        }
    };
    match checked {
        Ok(authenticated) => {
            request.extensions_mut().insert(authenticated);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

/// The largest body read whole before the answer.
pub const READ_AHEAD: u64 = 64 * 1024;

/// `request` with its body read whole if it has a known length up to [`READ_AHEAD`]; as it
/// is otherwise (no body, a larger one, or one of unknown length, which handlers stream).
async fn read_ahead(request: Request) -> Result<Request, axum::Error> {
    let small = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|length| (1..=READ_AHEAD).contains(&length));
    if !small {
        return Ok(request);
    }
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, READ_AHEAD as usize).await?;
    Ok(Request::from_parts(parts, axum::body::Body::from(bytes)))
}

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match parts.extensions.get::<Authenticated>() {
            Some(authenticated) => Ok(authenticated.clone()),
            // A route without the middleware (none should be): authenticated here.
            None => {
                let client = client_address(state, &parts.extensions, &parts.headers);
                state.authenticate(&parts.headers, client).await
            }
        }
    }
}

/// The client's address: the peer's, or what a trusted proxy says
/// ([`crate::auth::peers::client`]); `None` for a router served without peer addresses.
pub fn client_address(
    state: &AppState,
    extensions: &Extensions,
    headers: &HeaderMap,
) -> Option<IpAddr> {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    crate::auth::peers::client(&state.policy().trusted_proxies, peer, headers)
}

/// The client's address as an extractor ([`client_address`]).
pub struct ClientAddress(pub Option<IpAddr>);

impl FromRequestParts<AppState> for ClientAddress {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, std::convert::Infallible> {
        Ok(Self(client_address(
            state,
            &parts.extensions,
            &parts.headers,
        )))
    }
}
