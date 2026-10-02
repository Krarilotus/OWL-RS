//! SHACL validation over HTTP (plan step U2, design slice C1b): stored shapes and shapes
//! sent with the request, what is validated, and the report as JSON and as RDF.

mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use tower::util::ServiceExt;

use support::{body_text, query_text, test_app};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SHAPES_GRAPH: &str = "http://rdf4j.org/schema/rdf4j#SHACLShapeGraph";
const PREFIXES: &str = "@prefix ex: <http://example.com/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
";
/// Every person needs a name.
const SHAPES: &str = "ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:message \"a person needs a name\" ] .";

fn encode(text: &str) -> String {
    serde_urlencoded::to_string([("v", text)]).expect("encodes")[2..].to_owned()
}

async fn send(
    app: &axum::Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> Result<Response, Box<dyn std::error::Error>> {
    let mut request = Request::builder().uri(uri).method(method);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    Ok(app.clone().oneshot(request.body(body.into())?).await?)
}

/// Puts `turtle` into the named graph `graph`.
async fn put(app: &axum::Router, graph: &str, turtle: &str) -> TestResult {
    let uri = format!("/dataset/data?graph={}", encode(graph));
    let body = format!("{PREFIXES}{turtle}");
    let response = send(
        app,
        Method::PUT,
        &uri,
        &[("content-type", "text/turtle")],
        body,
    )
    .await?;
    assert!(
        response.status().is_success(),
        "{graph}: {}",
        response.status()
    );
    Ok(())
}

async fn report(
    app: &axum::Router,
    uri: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let response = send(
        app,
        Method::GET,
        uri,
        &[("accept", "application/json")],
        Body::empty(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    Ok(serde_json::from_str(&body_text(response).await?)?)
}

fn focus_nodes(report: &serde_json::Value) -> Vec<&str> {
    let mut nodes: Vec<&str> = report["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|result| result["focusNode"].as_str())
        .collect();
    nodes.sort_unstable();
    nodes
}

/// Shapes stored in the repository's shapes graph: the report, what the parameters
/// select, and that the shapes themselves aren't validated as data.
#[tokio::test]
async fn stored_shapes_validate_the_repository() -> TestResult {
    let app = test_app()?;
    // Without shapes there is nothing to violate.
    let empty = report(&app, "/dataset/shacl").await?;
    assert_eq!(empty["conforms"], true);
    assert_eq!(empty["shapes"], 0);

    put(&app, SHAPES_GRAPH, SHAPES).await?;
    put(
        &app,
        "http://example.com/g1",
        "ex:ada a ex:Person ; ex:name \"Ada\" .",
    )
    .await?;
    put(&app, "http://example.com/g2", "ex:bob a ex:Person .").await?;

    let all = report(&app, "/dataset/shacl").await?;
    assert_eq!(all["conforms"], false);
    assert_eq!(all["shapes"], 2, "the node shape and its property shape");
    assert_eq!(focus_nodes(&all), ["http://example.com/bob"]);
    let result = &all["results"][0];
    assert_eq!(result["resultPath"], "http://example.com/name");
    assert_eq!(
        result["sourceConstraintComponent"],
        "http://www.w3.org/ns/shacl#MinCountConstraintComponent"
    );
    assert_eq!(
        result["resultSeverity"],
        "http://www.w3.org/ns/shacl#Violation"
    );
    assert_eq!(result["resultMessage"][0], "a person needs a name");
    assert!(result["value"].is_null());
    assert!(
        result["sourceShape"]
            .as_str()
            .is_some_and(|shape| shape.starts_with("_:"))
    );

    // One graph at a time.
    let g1 = format!("/dataset/shacl?graph={}", encode("http://example.com/g1"));
    assert_eq!(report(&app, &g1).await?["conforms"], true);
    let g2 = format!("/dataset/shacl?graph={}", encode("http://example.com/g2"));
    assert_eq!(report(&app, &g2).await?["conforms"], false);
    assert_eq!(
        report(&app, "/dataset/shacl?default").await?["conforms"],
        true
    );
    let missing = format!("/dataset/shacl?graph={}", encode("http://example.com/none"));
    assert_eq!(report(&app, &missing).await?["conforms"], true);

    // Other stored shapes, by their graph.
    put(
        &app,
        "http://example.com/strict",
        "ex:Strict sh:targetClass ex:Person ; sh:path ex:email ; sh:minCount 1 ; sh:severity sh:Warning .",
    )
    .await?;
    let strict = format!(
        "/dataset/shacl?shapes-graph={}",
        encode("http://example.com/strict")
    );
    let strict = report(&app, &strict).await?;
    assert_eq!(
        focus_nodes(&strict),
        ["http://example.com/ada", "http://example.com/bob"]
    );
    assert_eq!(
        strict["results"][0]["resultSeverity"],
        "http://www.w3.org/ns/shacl#Warning"
    );

    for uri in [
        "/dataset/shacl?default&graph=http%3A%2F%2Fexample.com%2Fg1",
        "/dataset/shacl?infer=perhaps",
        "/dataset/shacl?shapes-graph=not%20an%20iri",
    ] {
        let response = send(&app, Method::GET, uri, &[], Body::empty()).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
    Ok(())
}

/// The report is RDF by default, and it is the same report: stored as a graph, a query
/// finds what the JSON says.
#[tokio::test]
async fn the_report_round_trips_as_rdf() -> TestResult {
    let app = test_app()?;
    put(&app, SHAPES_GRAPH, SHAPES).await?;
    put(&app, "http://example.com/g", "ex:bob a ex:Person .").await?;

    let response = send(&app, Method::GET, "/dataset/shacl", &[], Body::empty()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/turtle")
    );
    let turtle = body_text(response).await?;

    let target = format!(
        "/dataset/data?graph={}",
        encode("http://example.com/report")
    );
    let response = send(
        &app,
        Method::PUT,
        &target,
        &[("content-type", "text/turtle")],
        turtle,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let found = query_text(
        app.clone(),
        "PREFIX sh: <http://www.w3.org/ns/shacl#> PREFIX ex: <http://example.com/>
         ASK { GRAPH ex:report {
           ?report a sh:ValidationReport ; sh:conforms false ; sh:result ?result .
           ?result a sh:ValidationResult ; sh:focusNode ex:bob ; sh:resultPath ex:name ;
             sh:sourceConstraintComponent sh:MinCountConstraintComponent ;
             sh:resultSeverity sh:Violation ; sh:resultMessage \"a person needs a name\" .
         } }",
    )
    .await?;
    assert!(found.contains("true"), "{found}");

    for (accept, marker) in [
        (
            "application/n-triples",
            "<http://www.w3.org/ns/shacl#conforms>",
        ),
        ("application/ld+json", "http://www.w3.org/ns/shacl#conforms"),
        ("application/rdf+xml", "ValidationReport"),
    ] {
        let response = send(
            &app,
            Method::GET,
            "/dataset/shacl",
            &[("accept", accept)],
            Body::empty(),
        )
        .await?;
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some(accept)
        );
        assert!(body_text(response).await?.contains(marker), "{accept}");
    }
    let response = send(
        &app,
        Method::GET,
        "/dataset/shacl",
        &[("accept", "image/png")],
        Body::empty(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    Ok(())
}

/// Shapes sent with the request are validated against and leave nothing behind.
#[tokio::test]
async fn posted_shapes_validate_without_being_stored() -> TestResult {
    let app = test_app()?;
    put(
        &app,
        "http://example.com/g",
        "ex:bob a ex:Person . ex:ada a ex:Person ; ex:name \"Ada\" .",
    )
    .await?;
    let before = support::readyz_text(app.clone()).await?;

    let post = |body: String, content_type: &'static str| {
        let app = app.clone();
        async move {
            send(
                &app,
                Method::POST,
                "/dataset/shacl",
                &[
                    ("content-type", content_type),
                    ("accept", "application/json"),
                ],
                body,
            )
            .await
        }
    };
    let response = post(format!("{PREFIXES}{SHAPES}"), "text/turtle").await?;
    assert_eq!(response.status(), StatusCode::OK);
    let report: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(report["conforms"], false);
    assert_eq!(focus_nodes(&report), ["http://example.com/bob"]);

    // Nothing was committed: same revision and counts, no shapes anywhere, and the
    // writer is free.
    assert_eq!(support::readyz_text(app.clone()).await?, before);
    let shapes = query_text(
        app.clone(),
        "ASK { GRAPH ?g { ?s <http://www.w3.org/ns/shacl#targetClass> ?c } }",
    )
    .await?;
    assert!(shapes.contains("false"), "{shapes}");
    put(
        &app,
        "http://example.com/g",
        "ex:bob a ex:Person ; ex:name \"Bob\" .",
    )
    .await?;
    let response = post(format!("{PREFIXES}{SHAPES}"), "text/turtle").await?;
    let report: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(report["conforms"], true);

    // Ill-formed shapes are the request's fault, and the answer says which shape.
    let broken = format!("{PREFIXES}ex:Broken sh:targetClass ex:Person ; sh:minCount \"many\" .");
    let response = post(broken, "text/turtle").await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_text(response).await?;
    assert!(
        problem.contains("http://example.com/Broken") && problem.contains("sh:minCount"),
        "{problem}"
    );
    let response = post("not turtle {".to_owned(), "text/turtle").await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = post("{}".to_owned(), "application/json").await?;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    Ok(())
}

/// The shapes graph is a managed object: shapes that don't compile are refused, with every
/// problem, whichever way they are written, and the graph stays as it was.
#[tokio::test]
async fn shapes_are_checked_before_they_are_stored() -> TestResult {
    let app = test_app()?;
    let shapes_uri = "/api/v1/repositories/nrese/shapes";
    let broken = format!(
        "{PREFIXES}ex:S a sh:NodeShape ; sh:targetClass ex:C ;
           sh:property [ sh:path ex:p ; sh:minCount \"many\" ] ."
    );
    let turtle = [("content-type", "text/turtle")];
    let response = send(&app, Method::PUT, shapes_uri, &turtle, broken.clone()).await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let text = body_text(response).await?;
    assert!(text.contains("minCount"), "{text}");
    // Nothing was stored.
    let response = send(&app, Method::GET, shapes_uri, &[], Body::empty()).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let good = format!("{PREFIXES}{SHAPES}");
    let response = send(&app, Method::PUT, shapes_uri, &turtle, good).await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = send(
        &app,
        Method::GET,
        shapes_uri,
        &[("accept", "text/turtle")],
        Body::empty(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await?.contains("PersonShape"));

    // Through SPARQL too (the SHACL gate is off): the update is refused, the shapes stay.
    let update = format!(
        "PREFIX ex: <http://example.com/> PREFIX sh: <http://www.w3.org/ns/shacl#>
         INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{ ex:PersonShape sh:minCount \"many\" }} }}"
    );
    let response = send(
        &app,
        Method::POST,
        "/dataset/update",
        &[("content-type", "application/sparql-update")],
        update,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = send(&app, Method::DELETE, shapes_uri, &[], Body::empty()).await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    Ok(())
}
