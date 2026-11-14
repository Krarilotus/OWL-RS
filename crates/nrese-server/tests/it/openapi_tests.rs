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

/// Sends `method` to the default repository's `path` with `body` as `media` (no
/// `Content-Type` if `None`); the answer's status and media type.
async fn send(app: &axum::Router, request: Sent) -> (u16, String) {
    let (method, path, media, body) = request;
    let uri = format!("/api/v1/repositories/nrese{path}");
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(media) = media {
        request = request.header("content-type", media);
    }
    let request = request.body(Body::from(body)).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let media = response.headers().get("content-type");
    let media = media.map_or("", |value| value.to_str().unwrap());
    (
        response.status().as_u16(),
        media.split(';').next().unwrap().to_owned(),
    )
}

type Sent = (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
);

const SPARQL_QUERY: &str = "application/sparql-query";
const SPARQL_UPDATE: &str = "application/sparql-update";
const FORM: &str = "application/x-www-form-urlencoded";
const ASK: &str = "ASK {}";
const INSERT: &str = "INSERT DATA { <urn:ex:s> <urn:ex:p> <urn:ex:o> }";
const ASK_FORM: &str = "query=ASK%20%7B%7D";
const INSERT_FORM: &str = "update=INSERT%20DATA%20%7B%20%3Curn%3Aex%3As%3E%20%3Curn%3Aex%3Ap%3E%20%3Curn%3Aex%3Ao%3E%20%7D";

/// The SPARQL Protocol endpoints' description says how their handlers take a request
/// (SPARQL 1.1 Protocol §2.1, §2.2; docs/ops/http-api.md, "Endpoint contracts"): `GET` with
/// the query in the URL, required on `…/query`, optional on `…/sparql` (without it, the
/// service description), and no body; `POST` with a required body, in each media type the
/// handler reads; and every failure the handlers answer here is listed, as the media type
/// they send it as.
#[tokio::test]
async fn sparql_protocol_description_matches_its_handlers() {
    let app = test_app().unwrap();
    let request = Request::builder().uri("/api/v1/openapi.json");
    let response = app.clone().oneshot(request.body(Body::empty()).unwrap());
    let text = body_text(response.await.unwrap()).await.unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let operation = |(path, method): (&str, &str)| {
        &doc["paths"][format!("/api/v1/repositories/{{id}}{path}")][method]
    };
    // Whether `query` is a required URL parameter; `None` if it isn't one.
    let query_required = |at| {
        let parameters = operation(at)["parameters"].as_array().unwrap();
        let query = parameters
            .iter()
            .find(|p| p["name"] == "query" && p["in"] == "query");
        query.map(|parameter| parameter["required"] == true)
    };
    // The request body's media types, if it is required; `None` if there is no body.
    let body_media = |at| {
        let body = &operation(at)["requestBody"];
        body["content"].as_object().map(|content| {
            assert_eq!(body["required"], true, "{at:?}");
            content.keys().cloned().collect::<BTreeSet<_>>()
        })
    };
    let set = |media: &[&str]| -> Option<BTreeSet<String>> {
        Some(media.iter().map(|&m| m.to_owned()).collect())
    };
    let (query_get, query_post) = (("/query", "get"), ("/query", "post"));
    let (sparql_get, sparql_post) = (("/sparql", "get"), ("/sparql", "post"));
    let update = ("/update", "post");

    assert_eq!(query_required(query_get), Some(true));
    assert_eq!(query_required(sparql_get), Some(false));
    assert_eq!(query_required(query_post), None);
    assert_eq!(body_media(query_get), None);
    assert_eq!(body_media(sparql_get), None);
    assert_eq!(body_media(query_post), set(&[SPARQL_QUERY, FORM]));
    assert_eq!(
        body_media(update),
        set(&[SPARQL_UPDATE, "text/plain", FORM])
    );
    assert_eq!(
        body_media(sparql_post),
        set(&[SPARQL_QUERY, SPARQL_UPDATE, FORM])
    );

    // Each way the description names, as the handler answers it.
    let served: [(Sent, u16); 13] = [
        (("GET", "/query?query=ASK%20%7B%7D", None, ""), 200),
        (("POST", "/query", Some(SPARQL_QUERY), ASK), 200),
        (("POST", "/query", Some(FORM), ASK_FORM), 200),
        (("POST", "/update", Some(SPARQL_UPDATE), INSERT), 204),
        (("POST", "/update", Some("text/plain"), INSERT), 204),
        (("POST", "/update", None, INSERT), 204),
        (("POST", "/update", Some(FORM), INSERT_FORM), 204),
        (("GET", "/sparql?query=ASK%20%7B%7D", None, ""), 200),
        (("GET", "/sparql", None, ""), 200),
        (("POST", "/sparql", Some(SPARQL_QUERY), ASK), 200),
        (("POST", "/sparql", Some(SPARQL_UPDATE), INSERT), 204),
        (("POST", "/sparql", Some(FORM), ASK_FORM), 200),
        (("POST", "/sparql", Some(FORM), INSERT_FORM), 204),
    ];
    for (request, status) in served {
        assert_eq!(send(&app, request).await.0, status, "{request:?}");
    }
    assert_eq!(
        send(&app, ("GET", "/sparql", None, "")).await.1,
        "text/turtle"
    );

    // What the handler refuses, with the failure described for it, in its media type.
    let refused: [((&str, &str), Sent); 6] = [
        (query_get, ("GET", "/query", None, "")),
        (
            query_get,
            (
                "GET",
                "/query?query=ASK%20%7B%7D&query=ASK%20%7B%7D",
                None,
                "",
            ),
        ),
        // A POST's query is in its body, never in its URL.
        (
            query_post,
            ("POST", "/query?query=ASK%20%7B%7D", Some(SPARQL_QUERY), ""),
        ),
        (
            update,
            ("POST", "/update", Some("application/json"), INSERT),
        ),
        (sparql_post, ("POST", "/sparql", None, ASK)),
        (sparql_post, ("POST", "/sparql", Some(FORM), "")),
    ];
    for (at, request) in refused {
        let (status, media) = send(&app, request).await;
        let response = &operation(at)["responses"][status.to_string()];
        let described = response["content"]
            .as_object()
            .map(|c| c.keys().collect::<Vec<_>>());
        assert_eq!(
            described,
            Some(vec![&media]),
            "{request:?}: {status} {response}"
        );
    }
}
