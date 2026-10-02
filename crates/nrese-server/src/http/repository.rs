//! The repository a request is for ([ADR-0007](../../../../docs/adr/0007-one-engine-api.md)):
//! the `{id}` of the route's path, or the default repository where the route has none.
//! Handlers take [`Repository`] instead of the server's state, so one handler serves a
//! capability for the default repository (`/dataset/…`) and for every repository
//! (`/api/v1/repositories/{id}/…`).

use std::collections::HashMap;

use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;

use crate::error::ApiError;
use crate::state::AppState;

/// The server's state as the request's repository sees it ([`AppState::for_repository`]).
pub struct Repository(pub AppState);

impl FromRequestParts<AppState> for Repository {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let id = Path::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .ok()
            .and_then(|Path(params)| params.get("id").cloned());
        match id {
            None => Ok(Self(state.clone())),
            Some(id) => state.for_repository(&id).map(Self),
        }
    }
}
