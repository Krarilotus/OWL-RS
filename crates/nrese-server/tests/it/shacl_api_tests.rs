//! SHACL validation over HTTP (plan step U2, design slice C1b): stored shapes and shapes
//! sent with the request, what is validated, and the report as JSON and as RDF.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use nrese_store::{GateSeverity, ShaclGate, StoreConfig};
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::{body_text, query_text, test_app, test_app_with_store_config};
use crate::upgrade_tests::{Server, configure};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SHAPES_GRAPH: &str = "http://rdf4j.org/schema/rdf4j#SHACLShapeGraph";
const PREFIXES: &str = "@prefix ex: <http://example.com/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
";
/// Every person needs a name.
const SHAPES: &str = "ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:message \"a person needs a name\" ] .";

const NAMED_SHAPE: &str = "ex:NameShape a sh:PropertyShape ; sh:targetClass ex:Person ;
    sh:path ex:name ; sh:minCount 1 ; sh:message \"Name fehlt\"@de .";

fn restricted_shacl_app(read_shapes: bool) -> Result<axum::Router, Box<dyn std::error::Error>> {
    let shapes = if read_shapes {
        format!(", \"{SHAPES_GRAPH}\"")
    } else {
        String::new()
    };
    let policy = format!(
        r#"default = "deny"
inferred = "hidden"
[[role]]
name = "writer"
read = ["http://example.com/open"{shapes}]
write = ["http://example.com/open"]"#
    );
    test_app_with_store_config(
        StoreConfig {
            shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
            ..StoreConfig::in_memory()
        },
        policy_of(&policy)?,
        ReasonerConfig::for_mode(ReasoningMode::Disabled),
    )
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): an enforcing gate reports what the
/// committer's own graphs show. Equal readable data gives the same refusal body.
#[tokio::test]
async fn a_restricted_shacl_refusal_body_depends_only_on_readable_graphs() -> TestResult {
    let admin = format!("Bearer {}", token(&["nrese.admin"]));
    let writer = format!("Bearer {}", token(&["writer"]));
    let mut replies = Vec::new();
    // Each hidden graph satisfies a different visible focus node and adds its own focus
    // when Candidate becomes a subclass of Person. Even filtering the full report by
    // visible focus nodes would leave different answers and lose a visible result.
    for hidden in [
        "ex:bob ex:name \"Bob\" . ex:secretOne a ex:Candidate ; ex:note \"Name fehlt\"@de",
        "ex:ada ex:name \"Ada\" . ex:secretTwo a ex:Candidate ; ex:note \"A name is required\"",
    ] {
        let app = restricted_shacl_app(true)?;
        let update = "/dataset/update";
        let media = ("content-type", "application/sparql-update");
        let auth = ("authorization", admin.as_str());
        let seed = format!(
            "PREFIX ex: <http://example.com/> INSERT DATA {{ GRAPH ex:secret {{ {hidden} }} }}"
        );
        let response = send(&app, Method::POST, update, &[media, auth], seed).await?;
        assert!(
            response.status().is_success(),
            "{}",
            body_text(response).await?
        );
        let setup =
            format!("{PREFIXES}{NAMED_SHAPE}\nex:NameShape sh:message \"A name is required\" .");
        let response = send(
            &app,
            Method::PUT,
            "/api/v1/repositories/nrese/shapes",
            &[("content-type", "text/turtle"), auth],
            setup,
        )
        .await?;
        assert!(
            response.status().is_success(),
            "{}",
            body_text(response).await?
        );
        let response = send(
            &app,
            Method::POST,
            update,
            &[
                media,
                ("authorization", &writer),
                ("x-request-id", "same-request"),
            ],
            "PREFIX ex: <http://example.com/>
             INSERT DATA { GRAPH ex:open {
                ex:bob a ex:Person . ex:ada a ex:Person .
                ex:Candidate <http://www.w3.org/2000/01/rdf-schema#subClassOf> ex:Person
             } }",
        )
        .await?;
        replies.push((response.status(), body_text(response).await?));
    }
    assert_eq!(replies[0], replies[1]);
    let (status, body) = &replies[0];
    let problem: serde_json::Value = serde_json::from_str(body)?;
    assert_eq!(
        problem["shacl"]["results"],
        serde_json::json!([
            name_result("ada", &["A name is required", "Name fehlt"]),
            name_result("bob", &["A name is required", "Name fehlt"]),
        ]),
        "{body}"
    );
    assert_eq!(*status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["type"], "https://nrese.dev/problems/shacl-reject");
    assert!(!body.contains("secret"), "{body}");
    Ok(())
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): unreadable shapes and SHACL-SPARQL
/// shapes supply no SHACL report to a committer who can read only some graphs.
#[tokio::test]
async fn a_restricted_shacl_refusal_can_withhold_the_report() -> TestResult {
    let admin = format!("Bearer {}", token(&["nrese.admin"]));
    let writer = format!("Bearer {}", token(&["writer"]));
    for (read_shapes, shapes, data) in [
        (false, NAMED_SHAPE, "ex:bob a ex:Person"),
        (true, SPARQL_SHAPES, "ex:bob a ex:Person ; ex:name \"Bob\""),
    ] {
        let app = restricted_shacl_app(read_shapes)?;
        let setup = send(
            &app,
            Method::PUT,
            "/api/v1/repositories/nrese/shapes",
            &[("content-type", "text/turtle"), ("authorization", &admin)],
            format!("{PREFIXES}{shapes}"),
        )
        .await?;
        assert!(setup.status().is_success(), "{}", body_text(setup).await?);
        let response = send(
            &app,
            Method::POST,
            "/dataset/update",
            &[
                ("content-type", "application/sparql-update"),
                ("authorization", &writer),
            ],
            format!(
                "PREFIX ex: <http://example.com/> INSERT DATA {{ GRAPH ex:open {{ {data} }} }}"
            ),
        )
        .await?;
        let status = response.status();
        let problem: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
        assert_eq!(
            (problem["type"].as_str(), problem.get("shacl")),
            (Some("https://nrese.dev/problems/commit-reject"), None),
            "{problem}"
        );
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn name_result(person: &str, messages: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "focusNode": format!("http://example.com/{person}"),
        "resultPath": "http://example.com/name",
        "value": null,
        "sourceShape": "http://example.com/NameShape",
        "sourceConstraintComponent": "http://www.w3.org/ns/shacl#MinCountConstraintComponent",
        "resultSeverity": "http://www.w3.org/ns/shacl#Violation",
        "resultMessage": messages,
    })
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): report mode accepts the write and
/// logs its results, with the first result chosen by the same term order as a refusal.
#[test]
fn the_reporting_gate_logs_its_first_result_in_term_order() -> TestResult {
    use std::io::{Read, Seek};
    use std::sync::Arc;

    use nrese_store::{
        MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest,
        StoreService,
    };

    let store = StoreService::new(StoreConfig {
        shacl_gate: ShaclGate::Report,
        ..StoreConfig::in_memory()
    })?;
    let reasoner =
        nrese_reasoner::ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Disabled));
    let pipeline = MutationPipeline::new(Arc::new(store), Arc::new(reasoner));
    let apply = |body: &str| {
        let update = format!(
            "PREFIX ex: <http://example.com/> PREFIX sh: <http://www.w3.org/ns/shacl#> {body}"
        );
        pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &Requester::all(),
            &MutationTicket::new(),
        )
    };
    apply("INSERT DATA { ex:z ex:seed ex:z }")?;
    apply(&format!(
        "INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{ {NAMED_SHAPE} }} }}"
    ))?;
    let mut log = tempfile::tempfile()?;
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(log.try_clone()?)
        .finish();
    let write = tracing::subscriber::with_default(subscriber, || {
        apply("INSERT DATA { ex:z a ex:Person . ex:a a ex:Person }")
    });
    log.rewind()?;
    let mut output = String::new();
    log.read_to_string(&mut output)?;
    assert!(
        output.lines().any(|line| {
            line.contains("first: focus node <http://example.com/a>")
                && line.contains("SHACL gate: not rejected")
        }),
        "{output}"
    );
    write?;
    Ok(())
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): every committing protocol answers
/// a SHACL refusal with the same structured results and first-result explanation.
#[tokio::test]
async fn every_commit_protocol_carries_the_shacl_refusal() -> TestResult {
    let app = test_app_with_store_config(
        StoreConfig {
            shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
            ..StoreConfig::in_memory()
        },
        policy_of("default = \"deny\"")?,
        ReasonerConfig::for_mode(ReasoningMode::Disabled),
    )?;
    let admin = format!("Bearer {}", token(&["nrese.admin"]));
    let auth = ("authorization", admin.as_str());
    let update_media = ("content-type", "application/sparql-update");
    let turtle = ("content-type", "text/turtle");
    let data = "<http://example.com/bob> a <http://example.com/Person> .";
    let update = format!("INSERT DATA {{ {data} }}");
    let shapes = send(
        &app,
        Method::PUT,
        "/api/v1/repositories/nrese/shapes",
        &[auth, turtle],
        format!("{PREFIXES}{NAMED_SHAPE}"),
    )
    .await?;
    assert!(shapes.status().is_success(), "{}", body_text(shapes).await?);

    let repo = "/repositories/nrese";
    let opened = send(
        &app,
        Method::POST,
        &format!("{repo}/transactions"),
        &[auth],
        Body::empty(),
    )
    .await?;
    let tx = opened.headers()["location"].to_str()?.to_owned();
    let staged = send(
        &app,
        Method::PUT,
        &format!("{tx}?action=ADD"),
        &[auth, turtle],
        data,
    )
    .await?;
    assert_eq!(
        staged.status(),
        StatusCode::OK,
        "{}",
        body_text(staged).await?
    );

    let opened = send(
        &app,
        Method::POST,
        "/api/v1/repositories/nrese/sessions",
        &[auth],
        Body::empty(),
    )
    .await?;
    let opened: serde_json::Value = serde_json::from_str(&body_text(opened).await?)?;
    let session = opened["path"].as_str().expect("session path");
    let staged = send(
        &app,
        Method::POST,
        &format!("{session}/update"),
        &[auth, update_media],
        update.clone(),
    )
    .await?;
    assert!(staged.status().is_success(), "{}", body_text(staged).await?);

    let restore = format!(
        "<http://example.com/bob> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Person> .\n\
         <http://example.com/NameShape> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/shacl#PropertyShape> <{SHAPES_GRAPH}> .\n\
         <http://example.com/NameShape> <http://www.w3.org/ns/shacl#targetClass> <http://example.com/Person> <{SHAPES_GRAPH}> .\n\
         <http://example.com/NameShape> <http://www.w3.org/ns/shacl#path> <http://example.com/name> <{SHAPES_GRAPH}> .\n\
         <http://example.com/NameShape> <http://www.w3.org/ns/shacl#minCount> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> <{SHAPES_GRAPH}> .\n\
         <http://example.com/NameShape> <http://www.w3.org/ns/shacl#message> \"Name fehlt\"@de <{SHAPES_GRAPH}> .\n"
    );
    let expected = serde_json::json!({
        "results": [name_result("bob", &["Name fehlt"])],
        "results_omitted": 0,
        "failures": [],
        "failures_omitted": 0
    });
    for (method, uri, media, payload) in [
        (
            Method::POST,
            "/dataset/update",
            update_media,
            update.as_str(),
        ),
        (Method::PUT, "/dataset/data?default", turtle, data),
        (Method::POST, "/dataset/tell", turtle, data),
        (Method::POST, "/repositories/nrese/statements", turtle, data),
        (Method::PUT, &format!("{tx}?action=COMMIT"), turtle, ""),
        (Method::POST, &format!("{session}/commit"), turtle, ""),
        (
            Method::POST,
            "/ops/api/admin/dataset/restore",
            ("content-type", "application/n-quads"),
            restore.as_str(),
        ),
    ] {
        let response = send(&app, method, uri, &[auth, media], payload.to_owned()).await?;
        let status = response.status();
        let problem: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
        assert_eq!(problem["shacl"], expected, "{uri}: {problem}");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(problem["type"], "https://nrese.dev/problems/shacl-reject");
        let reason = &problem["reasoner_reject"];
        assert_eq!(
            reason["violated_constraint"],
            expected["results"][0]["sourceConstraintComponent"]
        );
        assert_eq!(reason["focus_resource"], "<http://example.com/bob>");
        assert_eq!(reason["evidence"], serde_json::json!([]));
    }
    let bad = send(
        &app,
        Method::POST,
        "/dataset/update",
        &[auth, update_media],
        "bad",
    )
    .await?;
    let problem: serde_json::Value = serde_json::from_str(&body_text(bad).await?)?;
    assert_eq!(problem.get("shacl"), None, "{problem}");
    Ok(())
}

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
    let before = crate::support::dataset_info_text(app.clone()).await?;

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
    assert_eq!(
        crate::support::dataset_info_text(app.clone()).await?,
        before
    );
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

/// A commit the SHACL gate rejects is answered as a SHACL rejection, not as the reasoner's:
/// a client that branches on the problem's type tells it from an inconsistency.
#[tokio::test]
async fn a_commit_the_shacl_gate_rejects_is_a_shacl_reject() -> TestResult {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::default()
    };
    let app =
        test_app_with_store_config(config, PolicyConfig::default(), ReasonerConfig::default())?;
    let shapes = format!("{PREFIXES}{SHAPES}");
    let shapes_uri = "/api/v1/repositories/nrese/shapes";
    let turtle = [("content-type", "text/turtle")];
    let response = send(&app, Method::PUT, shapes_uri, &turtle, shapes).await?;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = send(
        &app,
        Method::POST,
        "/dataset/update",
        &[("content-type", "application/sparql-update")],
        "INSERT DATA { <http://example.com/bob> a <http://example.com/Person> }",
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(
        problem["type"], "https://nrese.dev/problems/shacl-reject",
        "{problem}"
    );
    assert_eq!(problem["title"], "SHACL Reject", "{problem}");
    Ok(())
}

/// SHACL-SPARQL constraints with messages (SHACL 1.0 §5.3.2): one whose `sh:message` names
/// the solution's `?name`, one whose query binds `?message`. Every person needs a full name.
const SPARQL_SHAPES: &str = "ex:FullName a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:sparql [ sh:message \"{?name} is not a full name\" ;
    sh:select '''SELECT $this ?name WHERE {
      $this <http://example.com/name> ?name . FILTER (!CONTAINS(?name, \" \")) }''' ] .
ex:Surname a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:sparql [ sh:message \"never shown\" ;
    sh:select '''SELECT $this ?message WHERE {
      $this <http://example.com/name> ?name . FILTER (!CONTAINS(?name, \" \"))
      BIND (CONCAT(\"add a surname to \", ?name) AS ?message) }''' ] .";

/// The report of `GET /dataset/shacl` carries a SHACL-SPARQL result's message as §5.3.2 gives
/// it: the constraint's `sh:message` with the solution's value filled in, and a message the
/// query binds in place of the constraint's.
#[tokio::test]
async fn the_report_carries_sparql_constraint_messages() -> TestResult {
    let app = test_app()?;
    put(&app, SHAPES_GRAPH, SPARQL_SHAPES).await?;
    put(
        &app,
        "http://example.com/data",
        "ex:bob a ex:Person ; ex:name \"Bob\" .",
    )
    .await?;
    let report = report(&app, "/dataset/shacl").await?;
    // Each result's own messages, by its source shape: one result per shape.
    let mut messages: Vec<(String, Vec<String>)> = report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| {
            let shape = result["sourceShape"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let messages = result["resultMessage"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|message| message.as_str().map(str::to_owned))
                .collect();
            (shape, messages)
        })
        .collect();
    messages.sort();
    assert_eq!(
        messages,
        [
            (
                "http://example.com/FullName".to_owned(),
                vec!["Bob is not a full name".to_owned()]
            ),
            (
                "http://example.com/Surname".to_owned(),
                vec!["add a surname to Bob".to_owned()]
            )
        ],
        "{report}"
    );
    Ok(())
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): a refused commit carries the filled
/// SHACL-SPARQL message and names its shape and component. The detail repeats the message.
#[tokio::test]
async fn a_shacl_reject_names_the_result_s_message() -> TestResult {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::default()
    };
    let app =
        test_app_with_store_config(config, PolicyConfig::default(), ReasonerConfig::default())?;
    let only_filled = "ex:FullName a sh:NodeShape ; sh:targetClass ex:Person ;
      sh:sparql [ sh:message \"{?name} is not a full name\" ;
        sh:select '''SELECT $this ?name WHERE {
          $this <http://example.com/name> ?name . FILTER (!CONTAINS(?name, \" \")) }''' ] .";
    let shapes = format!("{PREFIXES}{only_filled}");
    let turtle = [("content-type", "text/turtle")];
    let response = send(
        &app,
        Method::PUT,
        "/api/v1/repositories/nrese/shapes",
        &turtle,
        shapes,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = send(
        &app,
        Method::POST,
        "/dataset/update",
        &[("content-type", "application/sparql-update")],
        "INSERT DATA { <http://example.com/bob> a <http://example.com/Person> ;
           <http://example.com/name> \"Bob\" }",
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    let result = &problem["shacl"]["results"][0];
    assert_eq!(
        (
            &result["resultMessage"],
            &result["sourceShape"],
            &result["sourceConstraintComponent"],
        ),
        (
            &serde_json::json!(["Bob is not a full name"]),
            &serde_json::json!("http://example.com/FullName"),
            &serde_json::json!("http://www.w3.org/ns/shacl#SPARQLConstraintComponent"),
        ),
        "{problem}"
    );
    assert_eq!(
        problem["type"], "https://nrese.dev/problems/shacl-reject",
        "{problem}"
    );
    let detail = problem["detail"].as_str().expect("a detail");
    assert!(
        detail.contains("message \"Bob is not a full name\""),
        "{detail}"
    );
    Ok(())
}

/// A refusal lists a result's messages by their text, not by the order the store happened to
/// learn the literals in, which other graphs decide: here the data's "zz" is learnt before the
/// shape's "aa", and the detail still lists "aa" first.
#[tokio::test]
async fn a_shacl_reject_lists_messages_by_their_text() -> TestResult {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::default()
    };
    let app =
        test_app_with_store_config(config, PolicyConfig::default(), ReasonerConfig::default())?;
    put(&app, "http://example.com/other", "ex:x ex:note \"zz\" .").await?;
    let shapes = format!(
        "{PREFIXES}ex:Named a sh:NodeShape ; sh:targetClass ex:Person ;
           sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:message \"zz\" , \"aa\" ] ."
    );
    let turtle = [("content-type", "text/turtle")];
    let response = send(
        &app,
        Method::PUT,
        "/api/v1/repositories/nrese/shapes",
        &turtle,
        shapes,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = send(
        &app,
        Method::POST,
        "/dataset/update",
        &[("content-type", "application/sparql-update")],
        "INSERT DATA { <http://example.com/bob> a <http://example.com/Person> }",
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    let detail = problem["detail"].as_str().expect("a detail");
    assert!(detail.contains("message \"aa\", \"zz\""), "{detail}");
    Ok(())
}

/// The SHACL gate's state as the repository's info gives it.
async fn gate_state(app: &axum::Router) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let info = report(app, "/dataset/info").await?;
    Ok(info["shacl"].clone())
}

/// The info names an enforcing gate, the least severity it refuses, and the shapes it
/// checks against: their number, nested ones included, as a validation counts them.
#[tokio::test]
async fn the_info_names_the_enforcing_gate_and_its_shapes() -> TestResult {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Warning),
        ..StoreConfig::default()
    };
    let app =
        test_app_with_store_config(config, PolicyConfig::default(), ReasonerConfig::default())?;
    put(&app, SHAPES_GRAPH, SHAPES).await?;
    let gate = gate_state(&app).await?;
    assert_eq!(
        gate["shapes"], 2,
        "the node shape and its property shape: {gate}"
    );
    assert_eq!(gate["vacuous"], false, "{gate}");
    assert_eq!(gate["gate"], "enforce", "{gate}");
    assert_eq!(gate["severity"], "warning", "{gate}");
    Ok(())
}

/// With no shapes stored, the info says the gate checks nothing (0 shapes, `vacuous`: an
/// enforcing gate then accepts every write, docs/limits.md), and names the gate: `off` has
/// no severity.
#[tokio::test]
async fn without_shapes_the_info_says_the_gate_checks_nothing() -> TestResult {
    for (gate, name, severity) in [
        (ShaclGate::Off, "off", serde_json::Value::Null),
        (
            ShaclGate::Enforce(GateSeverity::Violation),
            "enforce",
            "violation".into(),
        ),
    ] {
        let config = StoreConfig {
            shacl_gate: gate,
            ..StoreConfig::default()
        };
        let app =
            test_app_with_store_config(config, PolicyConfig::default(), ReasonerConfig::default())?;
        let state = gate_state(&app).await?;
        assert_eq!(state["shapes"], 0, "{name}: {state}");
        assert_eq!(state["vacuous"], true, "{name}: {state}");
        assert_eq!(state["gate"], name, "{state}");
        assert_eq!(state["severity"], severity, "{name}: {state}");
    }
    Ok(())
}

/// A shapes graph with no shape but an `sh:entailment` isn't vacuous: every validation
/// against it fails (SHACL §1.5: no entailment regime is supported), as the gate treats it.
#[tokio::test]
async fn an_entailment_alone_is_counted_and_not_vacuous() -> TestResult {
    let app = test_app()?;
    put(
        &app,
        SHAPES_GRAPH,
        "<http://example.com/shapes> sh:entailment <http://www.w3.org/ns/entailment/RDFS> .",
    )
    .await?;
    let gate = gate_state(&app).await?;
    assert_eq!(gate["vacuous"], false, "{gate}");
    assert_eq!(gate["shapes"], 0, "{gate}");
    Ok(())
}

/// The shapes are compiled when the shapes graph changes (the owner's ruling): a commit of
/// data keeps the revision they were compiled at; one that adds a shape counts it, at its
/// own revision.
#[tokio::test]
async fn the_shapes_are_counted_again_when_the_shapes_graph_changes() -> TestResult {
    let app = test_app()?;
    put(&app, SHAPES_GRAPH, SHAPES).await?;
    let gate = gate_state(&app).await?;
    assert_eq!(gate["shapes"], 2, "{gate}");
    let compiled = gate["revision"].as_u64().expect("a revision");

    put(
        &app,
        "http://example.com/people",
        "ex:ada a ex:Person ; ex:name \"Ada\" .",
    )
    .await?;
    let info = report(&app, "/dataset/info").await?;
    assert!(
        info["revision"].as_u64().is_some_and(|now| now > compiled),
        "the data's commit is a later revision: {info}"
    );
    assert_eq!(info["shacl"]["revision"], compiled, "{info}");
    assert_eq!(info["shacl"]["shapes"], 2, "{info}");

    let more = format!(
        "{SHAPES}\nex:Named a sh:NodeShape ; sh:targetClass ex:Person ; sh:nodeKind sh:IRI ."
    );
    put(&app, SHAPES_GRAPH, &more).await?;
    let after = report(&app, "/dataset/info").await?;
    assert_eq!(after["shacl"]["shapes"], 3, "{after}");
    let written = info["revision"].as_u64().expect("the data revision") + 1;
    assert_eq!(after["revision"], written, "{after}");
    assert_eq!(after["shacl"]["revision"], written, "{after}");
    Ok(())
}

/// A bulk load passes no commit gate, so it can store shapes that don't compile
/// (docs/limits.md): the info then counts nothing, null and never 0, and says why. Shapes
/// stored in their place are counted again.
#[tokio::test]
async fn shapes_that_dont_compile_are_not_counted() -> TestResult {
    let app = test_app()?;
    let uri = format!(
        "/api/v1/repositories/nrese/import?graph={}",
        encode(SHAPES_GRAPH)
    );
    let unreadable = format!(
        "{PREFIXES}ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:property [ sh:path \"not a path\" ] ."
    );
    let turtle = [("content-type", "text/turtle")];
    let response = send(&app, Method::POST, &uri, &turtle, unreadable).await?;
    let status = response.status();
    assert_eq!(status, StatusCode::OK, "{}", body_text(response).await?);

    let gate = gate_state(&app).await?;
    assert_eq!(gate.get("shapes"), Some(&serde_json::Value::Null), "{gate}");
    assert_eq!(
        gate.get("vacuous"),
        Some(&serde_json::Value::Null),
        "{gate}"
    );
    assert!(
        gate["not_counted"]
            .as_str()
            .is_some_and(|why| why.contains("don't compile") && why.contains("sh:path")),
        "{gate}"
    );

    put(&app, SHAPES_GRAPH, SHAPES).await?;
    let gate = gate_state(&app).await?;
    assert_eq!(gate["shapes"], 2, "{gate}");
    assert_eq!(gate.get("not_counted"), None, "{gate}");
    Ok(())
}

/// The configuration of a server binary on `dir`'s data directory with its SHACL gate set
/// to `enforce` ([`configure`]).
fn enforcing(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let data = dir.join("data");
    std::fs::create_dir_all(&data)?;
    let file = configure(dir, &data)?;
    let mut text = std::fs::read_to_string(&file)?;
    text.push_str("[shacl]\ngate = \"enforce\"\n");
    std::fs::write(&file, text)?;
    Ok(file)
}

/// At its start, a server warns when its enforcing gate checks nothing: a shapes graph that
/// holds only a label (metadata) compiles to no shape, so every write passes the gate.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_warns_when_the_shapes_graph_holds_no_shape() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = enforcing(dir.path())?;
    let server = Server::start(&config).await?;
    let label = format!(
        "INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{ <http://example.com/shapes> \
         <http://www.w3.org/2000/01/rdf-schema#label> \"the shapes\" }} }}"
    );
    server.update("nrese", &label).await?;
    drop(server);

    let server = Server::start(&config).await?;
    let output = server.output();
    assert!(
        output.lines().any(|line| line.contains("WARN")
            && line.contains("SHACL gate: no shapes are compiled from the shapes graph")),
        "{output}"
    );
    Ok(())
}

/// After a restart on the same data, the info counts the shapes stored before it, and the
/// start doesn't warn.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_counts_the_stored_shapes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = enforcing(dir.path())?;
    let server = Server::start(&config).await?;
    let shapes = format!(
        "PREFIX ex: <http://example.com/> PREFIX sh: <http://www.w3.org/ns/shacl#>
         INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{ ex:PersonShape a sh:NodeShape ;
           sh:targetClass ex:Person ; sh:property [ sh:path ex:name ; sh:minCount 1 ] }} }}"
    );
    server.update("nrese", &shapes).await?;
    drop(server);

    let server = Server::start(&config).await?;
    let info: serde_json::Value = serde_json::from_str(&server.get("/dataset/info").await?)?;
    assert_eq!(info["shacl"]["shapes"], 2, "{info}");
    assert_eq!(info["shacl"]["vacuous"], false, "{info}");
    let output = server.output();
    assert!(!output.contains("SHACL gate: no shapes"), "{output}");
    Ok(())
}

/// Spec (docs/ops/http-api.md): malformed stored shapes cause an enforcing gate to
/// warn at restart and refuse data writes until the shapes are repaired.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_with_malformed_shapes_warns_and_refuses_writes_until_repair() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = enforcing(dir.path())?;
    let file = dir.path().join("malformed.ttl");
    std::fs::write(
        &file,
        format!(
            "{PREFIXES}ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:property [ sh:path \"bad\" ] ."
        ),
    )?;
    {
        let store = nrese_store::StoreService::new(StoreConfig::on_disk(dir.path().join("data")))?;
        store.bulk_load(&nrese_store::BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: nrese_store::GraphTarget::NamedGraph(SHAPES_GRAPH.to_owned()),
            invalid: None,
        })?;
    }

    let server = Server::start(&config).await?;
    let info: serde_json::Value = serde_json::from_str(&server.get("/dataset/info").await?)?;
    assert_eq!(info["shacl"].get("shapes"), Some(&serde_json::Value::Null));
    let write = "INSERT DATA { <http://example.com/a> a <http://example.com/Person> }";
    // Refused; its status is not specified yet.
    server.update("nrese", write).await.unwrap_err();
    let output = server.output();
    assert!(
        output.lines().any(|line| line.contains("WARN")
            && line.contains("SHACL gate: the stored shapes don't compile")),
        "{output}"
    );

    let repair = format!("CLEAR GRAPH <{SHAPES_GRAPH}>");
    server.update("nrese", &repair).await?;
    let info: serde_json::Value = serde_json::from_str(&server.get("/dataset/info").await?)?;
    assert_eq!(info["shacl"]["shapes"], 0, "{info}");
    assert_eq!(info["shacl"]["vacuous"], true, "{info}");
    server.update("nrese", write).await?;
    Ok(())
}
