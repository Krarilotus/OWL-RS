//! The generated views committed with the documentation are what this code generates, so a
//! reader can see them without running the server (docs/ops/http-api.md, "Engine API";
//! docs/ops/config-reference.md, "Validation"):
//!
//! - `docs/ops/openapi.json`: the OpenAPI description the server serves at
//!   `GET /api/v1/openapi.json`;
//! - `docs/ops/config-schema.json`: the configuration file's JSON Schema, as
//!   `nrese-server config-schema` prints it.
//!
//! A committed file that differs from the code fails here. With `NRESE_WRITE_GENERATED=1`
//! the test writes the files instead. The bytes are the same on every platform:
//! `serde_json`'s pretty form with `\n` line ends, keys in its map's order (sorted).
//! `.gitattributes` checks the files out with `\n` on Windows too.

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app};

/// The command that rewrites both files, run from the repository root.
const REGENERATE: &str =
    "NRESE_WRITE_GENERATED=1 cargo test -p nrese-server --test it generated_views";

/// Compares `docs/ops/{name}` with `generated`, or writes it under `NRESE_WRITE_GENERATED`.
fn check(name: &str, generated: &serde_json::Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/ops")
        .join(name);
    let generated = serde_json::to_string_pretty(generated).unwrap() + "\n";
    if std::env::var_os("NRESE_WRITE_GENERATED").is_some() {
        std::fs::write(&path, generated).unwrap();
        return;
    }
    let committed = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "docs/ops/{name} can't be read ({error}): generate it with `{REGENERATE}` and commit it"
        )
    });
    assert!(
        committed == generated.as_bytes(),
        "docs/ops/{name} differs from what the code generates: run `{REGENERATE}` from the \
         repository root, read the diff, and commit the file with the change that caused it"
    );
}

#[tokio::test]
async fn generated_views_match_the_code() {
    let app = test_app().unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let openapi: serde_json::Value =
        serde_json::from_str(&body_text(response).await.unwrap()).unwrap();
    check("openapi.json", &openapi);
    check(
        "config-schema.json",
        &nrese_server::config::settings::schema(),
    );
}
