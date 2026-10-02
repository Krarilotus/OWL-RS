//! Authentication runs once per request, before the handler and before its body is read:
//! [`authenticate`] is a middleware on every route but the public ones ([`super::routes`]),
//! so a request without valid credentials is refused before the server buffers what it
//! sends. Handlers take the result as an extractor ([`Authenticated`]) and check their
//! action against it ([`super::guard`]).

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
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
    match state.authenticate(request.headers()).await {
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
            None => state.authenticate(&parts.headers).await,
        }
    }
}
