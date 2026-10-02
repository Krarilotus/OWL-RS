//! Authentication runs once per request, before the handler and before its body is read:
//! [`authenticate`] is a middleware on every route but the public ones ([`super::routes`]),
//! so a request without valid credentials is refused before the server buffers what it
//! sends. Handlers take the result as an extractor ([`Authenticated`]) and check their
//! action against it ([`super::guard`]).

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
pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let client = client_address(&state, request.extensions(), request.headers());
    match state.authenticate(request.headers(), client).await {
        Ok(authenticated) => {
            request.extensions_mut().insert(authenticated);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
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
