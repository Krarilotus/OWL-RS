//! The engine API's OpenAPI description (`GET /api/v1/openapi.json`) describes every
//! `/api/v1` route the router serves, with every method, and nothing it doesn't serve: the
//! routes are read from the router's source, so a route added without its description (or
//! a description left after its route) fails here.

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app};

const ROUTES: &str = include_str!("../../src/http/routes.rs");

/// The `(path, method)` pairs the router's source serves under `prefix` in the function
/// whose body starts at `from` and ends at `to`.
fn routes_in(from: &str, to: &str, prefix: &str) -> BTreeSet<(String, String)> {
    let start = ROUTES.find(from).expect("function");
    let end = start + ROUTES[start..].find(to).expect("its end");
    let body = &ROUTES[start..end];
    let mut found = BTreeSet::new();
    for chunk in body.split(".route(").skip(1) {
        let chunk = chunk.split(".nest(").next().unwrap_or(chunk);
        // Paths given by constants are the protocols' (`/dataset/…`), not the engine API's.
        if !chunk.trim_start().starts_with('"') {
            continue;
        }
        let path = chunk.split('"').nth(1).expect("the path's literal");
        let path = format!("{prefix}{path}");
        if !path.starts_with("/api/v1") {
            continue;
        }
        for method in ["get", "post", "put", "patch", "delete", "head"] {
            let call = format!("{method}(");
            let called = chunk.match_indices(&call).any(|(at, _)| {
                // `get(` as a call of its own, not the end of another name.
                at == 0
                    || !chunk.as_bytes()[at - 1].is_ascii_alphanumeric()
                        && chunk.as_bytes()[at - 1] != b'_'
            });
            if called {
                found.insert((path.clone(), method.to_owned()));
            }
        }
    }
    found
}

#[tokio::test]
async fn every_engine_api_route_is_described() {
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
    let doc: serde_json::Value = serde_json::from_str(&body_text(response).await.unwrap()).unwrap();
    assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
    assert!(!doc["info"]["version"].as_str().unwrap().is_empty());
    let mut described = BTreeSet::new();
    for (path, operations) in doc["paths"].as_object().unwrap() {
        for method in operations.as_object().unwrap().keys() {
            described.insert((path.clone(), method.clone()));
        }
    }
    let mut routed = routes_in("fn router(", ".with_state(state)", "");
    routed.extend(routes_in(
        "fn repository_routes(",
        "pub fn router(",
        "/api/v1/repositories/{id}",
    ));
    assert!(routed.len() > 40, "{routed:?}");
    let undescribed: Vec<_> = routed.difference(&described).collect();
    assert!(
        undescribed.is_empty(),
        "routes without a description: {undescribed:?}"
    );
    let unrouted: Vec<_> = described.difference(&routed).collect();
    assert!(
        unrouted.is_empty(),
        "descriptions without a route: {unrouted:?}"
    );
    // The schemas the description refers to are in it.
    let text = doc.to_string();
    for schema in [
        "Problem",
        "ChangeRecord",
        "RepositorySettings",
        "WorkspaceView",
        "Settings",
    ] {
        assert!(
            doc["components"]["schemas"][schema].is_object(),
            "no schema {schema}"
        );
    }
    assert!(text.contains("#/components/schemas/Problem"));
}
