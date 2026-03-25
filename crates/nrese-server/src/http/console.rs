use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use axum::response::Html;

use crate::error::ApiError;

pub fn dist_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/nrese-console/dist")
}

pub fn assets_dir() -> PathBuf {
    dist_dir().join("assets")
}

pub fn index() -> Result<Html<String>, ApiError> {
    let path = dist_dir().join("index.html");
    let html = fs::read_to_string(&path).with_context(|| {
        format!(
            "frontend build assets are missing at {}. Run npm install && npm run build in apps/nrese-console",
            path.display()
        )
    });
    html.map(Html)
        .map_err(|error| ApiError::unavailable(error.to_string()))
}
