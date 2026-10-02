//! The user console, served from the binary: `build.rs` embeds the files of
//! `apps/nrese-console/dist`, so a packaged server needs nothing beside it.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

use crate::error::ApiError;

include!(concat!(env!("OUT_DIR"), "/console_assets.rs"));

/// An embedded console file by its path below the console's base (`index.html`,
/// `assets/…`).
fn asset(path: &str) -> Option<&'static [u8]> {
    CONSOLE_ASSETS
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| *bytes)
}

/// Whether this binary was built with the console.
pub fn is_embedded() -> bool {
    asset("index.html").is_some()
}

pub fn index() -> Result<Html<String>, ApiError> {
    let html = asset("index.html").ok_or_else(|| {
        ApiError::unavailable(
            "this server was built without the user console: run npm install && npm run build \
             in apps/nrese-console, then build the server again",
        )
    })?;
    Ok(Html(String::from_utf8_lossy(html).into_owned()))
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// A console file other than the page itself. Files under `assets/` carry their content
/// hash in the name, so browsers may keep them; everything else is revalidated.
pub fn file(path: &str) -> Response {
    let Some(bytes) = asset(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

#[cfg(test)]
mod tests {
    use super::{CONSOLE_ASSETS, content_type};

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(
            content_type("assets/index-abc.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("assets/index-abc.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            content_type("console-config.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("unknown"), "application/octet-stream");
    }

    /// The table is what `build.rs` found: paths with forward slashes, no duplicates.
    #[test]
    fn embedded_paths_are_relative_and_unique() {
        let mut names: Vec<&str> = CONSOLE_ASSETS.iter().map(|(name, _)| *name).collect();
        assert!(
            names
                .iter()
                .all(|name| !name.contains('\\') && !name.starts_with('/'))
        );
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
