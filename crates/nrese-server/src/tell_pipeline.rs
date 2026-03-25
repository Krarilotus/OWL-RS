use nrese_store::{TellRequest, compile_tell_update};

use crate::error::ApiError;
use crate::state::AppState;
use crate::update_pipeline;

pub async fn execute(state: AppState, request: TellRequest) -> Result<(), ApiError> {
    let update = tokio::task::spawn_blocking(move || compile_tell_update(&request))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(|error| ApiError::bad_request(error.to_string()))?;

    update_pipeline::execute(state, update.update).await
}
