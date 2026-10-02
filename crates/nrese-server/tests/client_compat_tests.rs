//! The HTTP surface as real clients use it (plan step U1): RDF4J's SPARQL repository (which
//! ResearchSpace connects through), exporters that use SPARQL Update and the Graph Store
//! Protocol (the Datamodel Workflow), browsers and scripts.

mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use nrese_reasoner::ReasonerConfig;
use nrese_server::DeploymentPosture;
use nrese_server::policy::PolicyConfig;
use tower::util::ServiceExt;

use support::{body_text, test_app, test_app_with_posture};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FORM: &str = "application/x-www-form-urlencoded; charset=utf-8";
/// What RDF4J's SPARQL repository sends for tuple, graph and boolean queries.
const RDF4J_TUPLE: &str = "application/x-binary-rdf-results-table, \
    application/sparql-results+xml;q=0.8, application/sparql-results+json;q=0.8, \
    text/csv;q=0.8, text/tab-separated-values;q=0.8";
const RDF4J_GRAPH: &str = "application/x-binary-rdf, application/n-triples;q=0.8, \
    text/turtle;q=0.8, application/rdf+xml;q=0.5, application/trig;q=0.8, \
    application/n-quads;q=0.8";
const RDF4J_BOOLEAN: &str = "text/boolean, application/sparql-results+json;q=0.8, \
    application/sparql-results+xml;q=0.8";

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

fn content_type(response: &Response) -> &str {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

async fn update(app: &axum::Router, update: &str) -> TestResult {
    let response = send(
        app,
        Method::POST,
        "/dataset/update",
        &[("content-type", "application/sparql-update")],
        update.to_owned(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "{update}");
    Ok(())
}

/// One endpoint URL for queries and updates, as RDF4J's SPARQL repository uses it: forms
/// with extra parameters, long weighted `Accept` lists.
#[tokio::test]
async fn one_endpoint_serves_queries_and_updates_as_rdf4j_sends_them() -> TestResult {
    let app = test_app()?;
    for endpoint in ["/dataset/sparql", "/dataset"] {
        let insert = format!(
            "update={}",
            encode(&format!(
                "INSERT DATA {{ <http://example.com/s> <http://example.com/p> \"{endpoint}\" }}"
            ))
        );
        let response = send(
            &app,
            Method::POST,
            endpoint,
            &[("content-type", FORM)],
            insert,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");

        // A tuple query: the binary format isn't offered, JSON is the first of the rest.
        let select = format!(
            "query={}&infer=true&queryLn=sparql",
            encode("SELECT ?o WHERE { <http://example.com/s> <http://example.com/p> ?o }")
        );
        let response = send(
            &app,
            Method::POST,
            endpoint,
            &[("content-type", FORM), ("accept", RDF4J_TUPLE)],
            select,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(content_type(&response), "application/sparql-results+json");
        assert!(body_text(response).await?.contains(endpoint));

        let ask = format!("{endpoint}?query={}", encode("ASK { ?s ?p ?o }"));
        let response = send(
            &app,
            Method::GET,
            &ask,
            &[("accept", RDF4J_BOOLEAN)],
            Body::empty(),
        )
        .await?;
        assert_eq!(content_type(&response), "application/sparql-results+json");
        assert!(body_text(response).await?.contains("true"));

        let construct = format!("query={}", encode("CONSTRUCT WHERE { ?s ?p ?o }"));
        let response = send(
            &app,
            Method::POST,
            endpoint,
            &[("content-type", FORM), ("accept", RDF4J_GRAPH)],
            construct,
        )
        .await?;
        // RDF4J asks for its Binary RDF first, and gets it.
        assert_eq!(content_type(&response), "application/x-binary-rdf");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        let quads =
            nrese_store::parse_payload(nrese_store::GraphResultFormat::BinaryRdf, None, &bytes)?;
        assert!(
            quads
                .iter()
                .any(|quad| quad.subject.to_string() == "<http://example.com/s>")
        );

        // The media types of the protocol's direct POSTs work on the same URL.
        let response = send(
            &app,
            Method::POST,
            endpoint,
            &[("content-type", "application/sparql-query")],
            "ASK {}",
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let response = send(
            &app,
            Method::POST,
            endpoint,
            &[("content-type", "application/sparql-update")],
            "INSERT DATA { <http://example.com/s> <http://example.com/q> 1 }",
        )
        .await?;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Without a query, the endpoint describes itself.
        let response = send(&app, Method::GET, endpoint, &[], Body::empty()).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(content_type(&response).starts_with("text/turtle"));

        // Neither a query nor an update, or both, or a body that says nothing.
        for (content, body, status) in [
            (FORM, "timeout=5".to_owned(), StatusCode::BAD_REQUEST),
            (
                FORM,
                format!("query={}&update={}", encode("ASK {}"), encode("CLEAR ALL")),
                StatusCode::BAD_REQUEST,
            ),
            (
                "text/plain",
                "ASK {}".to_owned(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
        ] {
            let response = send(
                &app,
                Method::POST,
                endpoint,
                &[("content-type", content)],
                body,
            )
            .await?;
            assert_eq!(response.status(), status, "{content}");
        }
    }
    Ok(())
}

/// The combined endpoint applies each operation's own access rule: a read-only deployment
/// answers queries there and hides updates, as on the separate endpoints.
#[tokio::test]
async fn the_combined_endpoint_keeps_the_access_rules() -> TestResult {
    let app = test_app_with_posture(
        PolicyConfig::default(),
        ReasonerConfig::default(),
        DeploymentPosture::ReadOnlyDemo,
    )?;
    let query = format!("query={}", encode("ASK {}"));
    let response = send(
        &app,
        Method::POST,
        "/dataset/sparql",
        &[("content-type", FORM)],
        query,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let update = format!("update={}", encode("CLEAR ALL"));
    let response = send(
        &app,
        Method::POST,
        "/dataset/sparql",
        &[("content-type", FORM)],
        update,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

/// Results come in the format the client weights highest among those the query form has;
/// a client that accepts none of them gets 406, not a format it didn't ask for.
#[tokio::test]
async fn results_are_negotiated_per_query_form() -> TestResult {
    let app = test_app()?;
    update(
        &app,
        "INSERT DATA { <http://example.com/s> <http://example.com/p> \"v\"@en }",
    )
    .await?;
    let query = |query: &str| format!("/dataset/query?query={}", encode(query));
    let select = query("SELECT * WHERE { ?s ?p ?o }");
    let ask = query("ASK { ?s ?p ?o }");
    let construct = query("CONSTRUCT WHERE { ?s ?p ?o }");

    for (uri, accept, expected) in [
        (
            &select,
            "text/csv;q=0.5, text/tab-separated-values",
            "text/tab-separated-values",
        ),
        (
            &select,
            "application/json",
            "application/sparql-results+json",
        ),
        (
            &select,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            "application/sparql-results+xml",
        ),
        (
            &ask,
            "text/csv, application/sparql-results+xml;q=0.1",
            "application/sparql-results+xml",
        ),
        (&construct, "text/turtle", "text/turtle"),
        (&construct, "application/ld+json", "application/ld+json"),
        (&construct, "application/trig", "application/trig"),
        (&construct, "application/n-quads", "application/n-quads"),
        (
            &construct,
            "application/rdf+xml;q=0.9, text/*;q=0.1",
            "application/rdf+xml",
        ),
    ] {
        let response = send(&app, Method::GET, uri, &[("accept", accept)], Body::empty()).await?;
        assert_eq!(response.status(), StatusCode::OK, "{accept}");
        assert_eq!(content_type(&response), expected, "{accept}");
        let body = body_text(response).await?;
        // The triple, or for the ASK its answer.
        assert!(
            body.contains("example.com") || body.contains("true"),
            "{accept}: {body}"
        );
    }

    for (uri, accept) in [
        (&select, "image/png"),
        (&select, "text/turtle"),
        (&ask, "text/csv"),
        (&construct, "application/sparql-results+json"),
    ] {
        let response = send(&app, Method::GET, uri, &[("accept", accept)], Body::empty()).await?;
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE, "{accept}");
        assert_eq!(content_type(&response), "application/problem+json");
        assert!(body_text(response).await?.contains("available:"));
    }
    Ok(())
}

/// `using-graph-uri` and `using-named-graph-uri` give an update's `WHERE` its dataset.
#[tokio::test]
async fn updates_take_their_dataset_from_the_protocol() -> TestResult {
    let app = test_app()?;
    update(
        &app,
        "INSERT DATA {
           GRAPH <http://example.com/g1> { <http://example.com/a> <http://example.com/p> 1 }
           GRAPH <http://example.com/g2> { <http://example.com/b> <http://example.com/p> 2 }
         }",
    )
    .await?;
    let mark =
        "INSERT { ?s <http://example.com/seen> true } WHERE { ?s <http://example.com/p> ?o }";
    // Form: the parameters travel with the update.
    let body = format!(
        "update={}&using-graph-uri={}",
        encode(mark),
        encode("http://example.com/g1")
    );
    let response = send(
        &app,
        Method::POST,
        "/dataset/update",
        &[("content-type", FORM)],
        body,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let seen = |app: axum::Router| async move {
        let query = "SELECT ?s WHERE { ?s <http://example.com/seen> true } ORDER BY ?s";
        support::query_text(app, query).await
    };
    let text = seen(app.clone()).await?;
    assert!(
        text.contains("example.com/a") && !text.contains("example.com/b"),
        "{text}"
    );

    // Direct POST: the parameters are in the URL.
    let uri = format!(
        "/dataset/update?using-graph-uri={}",
        encode("http://example.com/g2")
    );
    let response = send(
        &app,
        Method::POST,
        &uri,
        &[("content-type", "application/sparql-update")],
        mark,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(seen(app.clone()).await?.contains("example.com/b"));

    let uri = "/dataset/update?using-graph-uri=not%20an%20iri";
    let response = send(
        &app,
        Method::POST,
        uri,
        &[("content-type", "application/sparql-update")],
        mark,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

/// The Graph Store Protocol as an exporter uses it: create, replace, read back, delete.
/// A named graph that holds nothing doesn't exist (404); the default graph always does.
#[tokio::test]
async fn the_graph_store_reports_what_exists() -> TestResult {
    let app = test_app()?;
    let graph = format!(
        "/dataset/data?graph={}",
        encode("http://example.com/module/v1")
    );
    let turtle = "<http://example.com/s> <http://example.com/p> <http://example.com/o> .";

    for method in [Method::GET, Method::HEAD, Method::DELETE] {
        let response = send(&app, method.clone(), &graph, &[], Body::empty()).await?;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} before the export"
        );
    }
    let put = |body: &'static str| {
        send(
            &app,
            Method::PUT,
            &graph,
            &[("content-type", "text/turtle")],
            body,
        )
    };
    assert_eq!(put(turtle).await?.status(), StatusCode::CREATED);
    // Exporting the same version again replaces it with the same content.
    assert_eq!(put(turtle).await?.status(), StatusCode::OK);
    let response = send(
        &app,
        Method::GET,
        &graph,
        &[("accept", "text/turtle")],
        Body::empty(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), "text/turtle");
    let first = body_text(response).await?;
    assert!(first.contains("<http://example.com/o>"));
    let response = send(&app, Method::HEAD, &graph, &[], Body::empty()).await?;
    assert_eq!(response.status(), StatusCode::OK);

    let response = send(
        &app,
        Method::GET,
        &graph,
        &[("accept", "image/png")],
        Body::empty(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);

    let response = send(&app, Method::DELETE, &graph, &[], Body::empty()).await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = send(&app, Method::GET, &graph, &[], Body::empty()).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // The default graph exists even when it is empty.
    let response = send(
        &app,
        Method::GET,
        "/dataset/data?default",
        &[],
        Body::empty(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(
        &app,
        Method::DELETE,
        "/dataset/data?default",
        &[],
        Body::empty(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    Ok(())
}

/// Every graph format is read and written, and a graph survives the round trip.
#[tokio::test]
async fn graphs_round_trip_in_every_format() -> TestResult {
    let app = test_app()?;
    let source = format!(
        "/dataset/data?graph={}",
        encode("http://example.com/source")
    );
    let turtle = "@prefix ex: <http://example.com/> .
        ex:s ex:label \"Stra\\u00DFe\"@de ; ex:n 42 ; ex:knows [ ex:label \"anon\" ] .";
    let response = send(
        &app,
        Method::PUT,
        &source,
        &[("content-type", "text/turtle")],
        turtle,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);

    for media_type in [
        "application/n-triples",
        "text/turtle",
        "application/rdf+xml",
        "application/ld+json",
        "application/n-quads",
        "application/trig",
    ] {
        let response = send(
            &app,
            Method::GET,
            &source,
            &[("accept", media_type)],
            Body::empty(),
        )
        .await?;
        assert_eq!(content_type(&response), media_type);
        let payload = body_text(response).await?;
        let copy = format!(
            "/dataset/data?graph={}",
            encode(&format!("http://example.com/copy/{media_type}"))
        );
        let response = send(
            &app,
            Method::PUT,
            &copy,
            &[("content-type", media_type)],
            payload,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::CREATED, "{media_type}");
        let count = support::query_text(
            app.clone(),
            &format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <http://example.com/copy/{media_type}> {{
                   ?s <http://example.com/label> \"Straße\"@de ; <http://example.com/n> 42 ;
                      <http://example.com/knows> [ <http://example.com/label> \"anon\" ] }} }}"
            ),
        )
        .await?;
        assert!(count.contains("\"1\""), "{media_type}: {count}");
    }

    // A payload that names graphs of its own isn't a single graph.
    let quads = "<http://example.com/s> <http://example.com/p> <http://example.com/o> <http://example.com/other> .";
    let response = send(
        &app,
        Method::PUT,
        &source,
        &[("content-type", "application/n-quads")],
        quads,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = send(
        &app,
        Method::PUT,
        &source,
        &[("content-type", "text/plain")],
        "x",
    )
    .await?;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    Ok(())
}

/// The policy's size limits are the ones that apply: a request larger than the web
/// framework's own default (2 MiB) but within the policy passes, and one over the policy
/// is refused with a problem document. (ResearchSpace loads its larger system graphs in
/// single updates.)
#[tokio::test]
async fn request_size_limits_are_the_policys() -> TestResult {
    let mut policy = PolicyConfig::default();
    policy.limits.max_update_bytes = 6 * 1024 * 1024;
    policy.limits.max_rdf_upload_bytes = 6 * 1024 * 1024;
    let app = support::test_app_with_policy(policy)?;

    // About 3 MiB of statements, as a form-encoded update (larger still on the wire).
    let statements: String = (0..30_000)
        .map(|i| {
            format!(
                "<http://example.com/s{i}> <http://example.com/p> \"{}\" .\n",
                "x".repeat(60)
            )
        })
        .collect();
    assert!(statements.len() > 3 * 1024 * 1024);
    let body = format!(
        "update={}",
        encode(&format!("INSERT DATA {{ {statements} }}"))
    );
    let response = send(
        &app,
        Method::POST,
        "/dataset/sparql",
        &[("content-type", FORM)],
        body,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let graph = format!("/dataset/data?graph={}", encode("http://example.com/big"));
    let response = send(
        &app,
        Method::PUT,
        &graph,
        &[("content-type", "application/n-triples")],
        statements.clone(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);

    // Over the policy: refused by the policy, as a problem document.
    let too_large = statements.repeat(3);
    let response = send(
        &app,
        Method::PUT,
        &graph,
        &[("content-type", "application/n-triples")],
        too_large,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(content_type(&response), "application/problem+json");
    Ok(())
}

/// Blazegraph's query hints, which ResearchSpace's and other Blazegraph clients' queries
/// carry, don't change the answer: they are dropped, not read as triple patterns.
#[tokio::test]
async fn blazegraph_query_hints_are_ignored() -> TestResult {
    let app = test_app()?;
    update(
        &app,
        "INSERT DATA { <http://example.com/a> <http://www.w3.org/2000/01/rdf-schema#label> \"A\" }",
    )
    .await?;
    let query = "PREFIX hint: <http://www.bigdata.com/queryHints#>
        SELECT ?l WHERE { hint:Query hint:optimizer \"None\" .
          ?s <http://www.w3.org/2000/01/rdf-schema#label> ?l . hint:Prior hint:runFirst true }";
    let response = send(
        &app,
        Method::POST,
        "/dataset/query",
        &[
            ("content-type", "application/sparql-query"),
            ("accept", "application/sparql-results+json"),
        ],
        query.to_owned(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await?;
    assert!(body.contains("\"A\""), "{body}");
    Ok(())
}
