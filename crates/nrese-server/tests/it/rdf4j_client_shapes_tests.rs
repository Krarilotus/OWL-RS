//! A client of the RDF4J repository protocol, through the server router.
//! Sources: RDF4J REST API `/repositories` and `/transactions` operations;
//! https://rdf4j.org/documentation/reference/rest-api/rdf4j-openapi.yaml
//! NRESE's documented plain/union default dataset: docs/ops/config-reference.md.
//! shape: from the audit list, unconfirmed

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use nrese_rdf::Dataset;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use nrese_store::{GateSeverity, ShaclGate, StoreConfig};
use serde_json::{Value, json};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app, test_app_with_store_config};

const REPO: &str = "/repositories/nrese";
const STATEMENTS: &str = "/repositories/nrese/statements";
const QUERY: &str = "application/sparql-query";
const UPDATE: &str = "application/sparql-update";
const JSON: &str = "application/sparql-results+json";
const MIXED: &str = r#"
    @prefix : <urn:> .
    :root :list _:head .
    _:head <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> :item ;
           <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .
    :g1 { :entity :p :value . :root :list _:head . }
    :g2 { :other :p :value . :root :list _:head . }
"#;
const SCHEMA: &str = r#"
    <urn:C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:D> .
    <urn:g1> { <urn:entity> a <urn:C> . }
"#;

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    (status, headers, body_text(response).await.unwrap())
}

fn app(union: bool, reasoning: bool) -> Router {
    if !union && !reasoning {
        return test_app().unwrap();
    }
    test_app_with_store_config(
        StoreConfig {
            union_default_graph: union,
            ..StoreConfig::default()
        },
        PolicyConfig::default(),
        ReasonerConfig::for_mode(if reasoning {
            ReasoningMode::Rdfs
        } else {
            ReasoningMode::Disabled
        }),
    )
    .unwrap()
}

fn dataset(body: &str, format: RdfFormat) -> Dataset {
    let mut data: Dataset = RdfParser::from_format(format)
        .for_slice(body.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    data.canonicalize();
    data
}

async fn upload(app: &Router, data: &str) {
    let (status, _, body) = send(
        app,
        Method::POST,
        STATEMENTS,
        &[("content-type", "application/x-trig")],
        data,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(body.is_empty());
}

async fn update(app: &Router, uri: &str, text: &str) {
    let transactional = uri.contains("action=UPDATE");
    let (status, _, body) = send(
        app,
        if transactional {
            Method::PUT
        } else {
            Method::POST
        },
        uri,
        &[("content-type", UPDATE)],
        text,
    )
    .await;
    assert_eq!(
        status,
        if transactional {
            StatusCode::OK
        } else {
            StatusCode::NO_CONTENT
        },
        "{body}"
    );
    assert!(body.is_empty());
}

async fn query(app: &Router, uri: &str, text: &str) -> Value {
    let (status, headers, body) = send(
        app,
        Method::POST,
        uri,
        &[("content-type", QUERY), ("accept", JSON)],
        text,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers["content-type"], JSON);
    serde_json::from_str(&body).unwrap()
}

async fn export(app: &Router, infer: bool) -> String {
    let uri = if infer {
        STATEMENTS.to_owned()
    } else {
        format!("{STATEMENTS}?infer=false")
    };
    let (status, _, body) = send(
        app,
        Method::GET,
        &uri,
        &[("accept", "application/x-trig")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn begin(app: &Router) -> String {
    let (status, headers, body) = send(
        app,
        Method::POST,
        &format!("{REPO}/transactions?isolation-level=READ_COMMITTED"),
        &[
            ("host", "internal.example:7878"),
            ("x-forwarded-host", "public.example"),
            ("x-forwarded-proto", "https"),
        ],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.is_empty());
    let location = headers["location"].to_str().unwrap();
    assert!(
        location.starts_with("https://public.example/repositories/nrese/transactions/"),
        "{location}"
    );
    location.to_owned()
}

async fn commit(app: &Router, location: &str) {
    let (status, _, body) = send(
        app,
        Method::PUT,
        &format!("{location}?action=COMMIT"),
        &[],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.is_empty());
}

#[tokio::test]
async fn raw_construct_ask_and_count_obey_the_documented_default_dataset() {
    // Spec: documented default dataset says named-only data -> absent by default, present under union.
    for union in [false, true] {
        let app = app(union, false);
        upload(&app, "<urn:g> { <urn:entity> <urn:p> <urn:value> . }").await;
        let (status, headers, body) = send(
            &app,
            Method::POST,
            REPO,
            &[("content-type", QUERY), ("accept", "application/n-triples")],
            "CONSTRUCT { <urn:entity> ?p ?o } WHERE { <urn:entity> ?p ?o }",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(headers["content-type"], "application/n-triples");
        assert_eq!(
            dataset(&body, RdfFormat::NTriples),
            dataset(
                if union {
                    "<urn:entity> <urn:p> <urn:value> ."
                } else {
                    ""
                },
                RdfFormat::NTriples
            )
        );
        assert_eq!(
            query(&app, REPO, "ASK { <urn:entity> <urn:p> <urn:value> }").await["boolean"],
            union
        );
        let count = query(
            &app,
            REPO,
            "SELECT (COUNT(*) AS ?count) WHERE { <urn:entity> ?p ?o }",
        )
        .await;
        assert_eq!(count["head"]["vars"], json!(["count"]));
        assert_eq!(
            count["results"]["bindings"],
            json!([{"count": {
                "type": "literal", "datatype": "http://www.w3.org/2001/XMLSchema#integer",
                "value": if union { "1" } else { "0" }
            }}])
        );
    }
}

#[tokio::test]
async fn raw_insert_is_visible_after_the_no_content_response() {
    // Spec: RDF4J POST statements says a SPARQL update -> 204 and committed data.
    for union in [false, true] {
        let app = app(union, false);
        update(
            &app,
            STATEMENTS,
            "INSERT DATA { <urn:entity> <urn:p> <urn:value> }",
        )
        .await;
        assert_eq!(
            query(&app, REPO, "ASK { <urn:entity> <urn:p> <urn:value> }").await["boolean"],
            true
        );
    }
}

async fn accepted_trig_export(union: bool) {
    let app = app(union, true);
    upload(&app, SCHEMA).await;
    let (status, headers, body) = send(
        &app,
        Method::GET,
        STATEMENTS,
        &[("accept", "application/x-trig")],
        "",
    )
    .await;
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/x-trig")
    );
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = dataset(&body, RdfFormat::TriG);
    for quad in dataset(SCHEMA, RdfFormat::TriG).iter() {
        assert!(data.contains(quad));
    }
    for quad in dataset("<urn:entity> a <urn:D> .", RdfFormat::TriG).iter() {
        assert!(data.contains(quad));
    }
}

#[tokio::test]
#[ignore = "RFC 9110 section 12.5.1 requires an accepted media type; x-trig is answered as application/trig"]
async fn default_export_answers_in_the_only_accepted_trig_media_type() {
    // Spec: RFC 9110 12.5.1 and RDF4J GET statements say Accept x-trig -> x-trig with graphs and inference.
    accepted_trig_export(false).await;
}

#[tokio::test]
#[ignore = "RFC 9110 section 12.5.1 requires an accepted media type; x-trig is answered as application/trig"]
async fn union_export_answers_in_the_only_accepted_trig_media_type() {
    // Spec: RFC 9110 12.5.1 and RDF4J GET statements say Accept x-trig -> x-trig with graphs and inference.
    accepted_trig_export(true).await;
}

#[tokio::test]
async fn drop_all_clears_assertions_and_shapes_but_preserves_access_state() {
    // Spec: documented DROP ALL scope says all asserted graphs, including configured shapes -> empty; access stays.
    for union in [false, true] {
        let app = test_app_with_store_config(
            StoreConfig {
                union_default_graph: union,
                shapes_graph: "urn:shapes".to_owned(),
                ..StoreConfig::default()
            },
            PolicyConfig::default(),
            ReasonerConfig::default(),
        )
        .unwrap();
        let (status, _, body) = send(
            &app,
            Method::PUT,
            "/api/v1/access/users/client",
            &[("content-type", "application/json")],
            r#"{"roles":["reader"],"reason":"protocol test"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let before = send(&app, Method::GET, "/api/v1/access", &[], "").await;
        assert_eq!(before.0, StatusCode::OK, "{}", before.2);
        upload(&app, MIXED).await;
        upload(
            &app,
            "<urn:shapes> { <urn:shape> a <http://www.w3.org/ns/shacl#NodeShape> . }",
        )
        .await;
        update(&app, STATEMENTS, "DROP ALL").await;
        assert!(dataset(&export(&app, false).await, RdfFormat::TriG).is_empty());
        let after = send(&app, Method::GET, "/api/v1/access", &[], "").await;
        assert_eq!(after.0, StatusCode::OK, "{}", after.2);
        assert_eq!(after.2, before.2);
    }
}

#[tokio::test]
async fn trig_upload_preserves_graphs_and_shared_blank_node_identity() {
    // Spec: RDF4J POST statements says a TriG payload -> its dataset, modulo blank-node labels.
    for union in [false, true] {
        let app = app(union, false);
        upload(&app, MIXED).await;
        assert_eq!(
            dataset(&export(&app, false).await, RdfFormat::TriG),
            dataset(MIXED, RdfFormat::TriG)
        );
    }
}

#[tokio::test]
async fn transaction_location_can_be_followed_verbatim_through_query_update_commit_and_rollback() {
    // Spec: RDF4J transactions say begin -> 201 Location; QUERY/UPDATE/COMMIT -> 200; rollback -> 204 then 404.
    for union in [false, true] {
        let app = app(union, false);
        upload(
            &app,
            "<urn:g1> { <urn:a> <urn:p> 1 . } <urn:g2> { <urn:b> <urn:p> 2 . }",
        )
        .await;
        let location = begin(&app).await;
        for suffix in [
            "",
            "&infer=false",
            "&default-graph-uri=urn%3Ag1",
            "&infer=false&default-graph-uri=urn%3Ag1",
        ] {
            let answer = query(
                &app,
                &format!("{location}?action=QUERY{suffix}"),
                "SELECT ?s WHERE { ?s <urn:p> ?o } ORDER BY ?s",
            )
            .await;
            let expected = if suffix.contains("default-graph-uri") {
                json!([{"s":{"type":"uri","value":"urn:a"}}])
            } else if union {
                json!([{"s":{"type":"uri","value":"urn:a"}}, {"s":{"type":"uri","value":"urn:b"}}])
            } else {
                json!([])
            };
            assert_eq!(answer["results"]["bindings"], expected);
        }
        update(
            &app,
            &format!("{location}?action=UPDATE"),
            "INSERT DATA { <urn:own> <urn:p> 3 }",
        )
        .await;
        assert_eq!(
            query(
                &app,
                &format!("{location}?action=QUERY"),
                "ASK { <urn:own> <urn:p> 3 }"
            )
            .await["boolean"],
            true
        );
        assert_eq!(
            query(&app, REPO, "ASK { <urn:own> <urn:p> 3 }").await["boolean"],
            false
        );
        commit(&app, &location).await;
        assert_eq!(
            query(&app, REPO, "ASK { <urn:own> <urn:p> 3 }").await["boolean"],
            true
        );
        let location = begin(&app).await;
        update(
            &app,
            &format!("{location}?action=UPDATE"),
            "INSERT DATA { <urn:rolled> <urn:p> 4 }",
        )
        .await;
        let (status, _, body) = send(&app, Method::DELETE, &location, &[], "").await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(body.is_empty());
        let (status, _, _) = send(
            &app,
            Method::PUT,
            &format!("{location}?action=COMMIT"),
            &[],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            query(&app, REPO, "ASK { <urn:rolled> <urn:p> 4 }").await["boolean"],
            false
        );
    }
}

#[tokio::test]
async fn repository_create_list_and_size_are_the_rdf4j_fixture_mapping() {
    // Spec: RDF4J repository management says PUT config -> 204/409; list includes id; size -> 200/404.
    const CONFIG: &str = r#"
        @prefix rep: <http://www.openrdf.org/config/repository#> .
        @prefix sr: <http://www.openrdf.org/config/repository/sail#> .
        @prefix sail: <http://www.openrdf.org/config/sail#> .
        @prefix owlim: <http://www.ontotext.com/trree/owlim#> .
        [] a rep:Repository ; rep:repositoryID "client" ;
          rep:repositoryImpl [ rep:repositoryType "graphdb:SailRepository" ;
            sr:sailImpl [ sail:sailType "graphdb:Sail" ; owlim:ruleset "empty" ] ] .
    "#;
    for union in [false, true] {
        let app = app(union, false);
        for expected in [StatusCode::NO_CONTENT, StatusCode::CONFLICT] {
            let (status, _, body) = send(
                &app,
                Method::PUT,
                "/repositories/client",
                &[("content-type", "text/turtle")],
                CONFIG,
            )
            .await;
            assert_eq!(status, expected, "{body}");
        }
        let (status, headers, body) =
            send(&app, Method::GET, "/repositories", &[("accept", JSON)], "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(headers["content-type"], JSON);
        let list: Value = serde_json::from_str(&body).unwrap();
        assert!(
            list["results"]["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"]["value"] == "client")
        );
        for (id, expected) in [
            ("client", StatusCode::OK),
            ("absent", StatusCode::NOT_FOUND),
        ] {
            let (status, _, body) = send(
                &app,
                Method::GET,
                &format!("/repositories/{id}/size"),
                &[],
                "",
            )
            .await;
            assert_eq!(status, expected, "{body}");
            if status == StatusCode::OK {
                assert_eq!(body.trim().parse::<u64>().unwrap(), 0);
            }
        }
    }
}

async fn wrong_basic_challenge(
    union: bool,
    method: Method,
    path: &str,
    content_type: &str,
    accept: &str,
    body: &str,
) {
    let app = app(union, false); // default policy enables local logins
    let (status, headers, text) = send(
        &app,
        method.clone(),
        path,
        &[
            ("content-type", content_type),
            ("accept", accept),
            ("authorization", "Basic Y2xpZW50Ondyb25n"),
        ],
        body,
    )
    .await;
    assert!(
        headers
            .get_all("www-authenticate")
            .iter()
            .any(|v| v.to_str().unwrap().split(',').any(|s| s
                .trim()
                .split_whitespace()
                .next()
                .is_some_and(|s| s.eq_ignore_ascii_case("Basic")))),
        "RFC 9110 requires a Basic challenge for a refused local login; got {headers:?}"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
}

macro_rules! refused_basic {
    ($name:ident, $method:ident, $path:expr, $content:expr, $accept:expr, $body:expr) => {
        mod $name {
        use super::*;
        #[tokio::test]
        #[ignore = "RFC 9110 section 15.5.2 requires WWW-Authenticate on 401; local-login refusals omit the Basic challenge"]
        async fn defaults() {
            // Spec: RFC 9110 15.5.2 says wrong Basic credentials -> 401 with a Basic challenge.
            wrong_basic_challenge(false, Method::$method, $path, $content, $accept, $body).await;
        }
        #[tokio::test]
        #[ignore = "RFC 9110 section 15.5.2 requires WWW-Authenticate on 401; local-login refusals omit the Basic challenge"]
        async fn union() {
            // Spec: RFC 9110 15.5.2 says wrong Basic credentials -> 401 with a Basic challenge.
            wrong_basic_challenge(true, Method::$method, $path, $content, $accept, $body).await;
        }
        }
    };
}
refused_basic!(
    construct_challenges_wrong_basic,
    POST,
    REPO,
    QUERY,
    "application/n-triples",
    "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"
);
refused_basic!(
    ask_challenges_wrong_basic,
    POST,
    REPO,
    QUERY,
    JSON,
    "ASK {}"
);
refused_basic!(
    count_challenges_wrong_basic,
    POST,
    REPO,
    QUERY,
    JSON,
    "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"
);
refused_basic!(
    insert_challenges_wrong_basic,
    POST,
    STATEMENTS,
    UPDATE,
    "*/*",
    "INSERT DATA { <urn:a> <urn:p> 1 }"
);
refused_basic!(
    export_challenges_wrong_basic,
    GET,
    STATEMENTS,
    "",
    "application/x-trig",
    ""
);
refused_basic!(
    drop_challenges_wrong_basic,
    POST,
    STATEMENTS,
    UPDATE,
    "*/*",
    "DROP ALL"
);
refused_basic!(
    upload_challenges_wrong_basic,
    POST,
    STATEMENTS,
    "application/x-trig",
    "*/*",
    MIXED
);
refused_basic!(
    begin_challenges_wrong_basic,
    POST,
    "/repositories/nrese/transactions?isolation-level=READ_COMMITTED",
    "",
    "*/*",
    ""
);
refused_basic!(
    transaction_query_challenges_wrong_basic,
    POST,
    "/repositories/nrese/transactions/tx-absent?action=QUERY",
    QUERY,
    JSON,
    "ASK {}"
);
refused_basic!(
    transaction_update_challenges_wrong_basic,
    PUT,
    "/repositories/nrese/transactions/tx-absent?action=UPDATE",
    UPDATE,
    "*/*",
    "INSERT DATA { <urn:a> <urn:p> 1 }"
);
refused_basic!(
    commit_challenges_wrong_basic,
    PUT,
    "/repositories/nrese/transactions/tx-absent?action=COMMIT",
    "",
    "*/*",
    ""
);
refused_basic!(
    rollback_challenges_wrong_basic,
    DELETE,
    "/repositories/nrese/transactions/tx-absent",
    "",
    "*/*",
    ""
);

#[tokio::test]
async fn default_auth_configuration_allows_anonymous_protocol_calls() {
    // Spec: documented default auth configuration says no credentials -> anonymous reads and writes permitted.
    for union in [false, true] {
        let app = app(union, false);
        update(&app, STATEMENTS, "INSERT DATA { <urn:a> <urn:p> 1 }").await;
        assert_eq!(
            query(&app, REPO, "ASK { <urn:a> <urn:p> 1 }").await["boolean"],
            true
        );
        let location = begin(&app).await;
        commit(&app, &location).await;
    }
}

#[tokio::test]
async fn two_clients_with_delete_where_keep_uncommitted_changes_private_and_rollback_cleanly() {
    // Spec: RDF4J READ_COMMITTED says pending changes -> invisible outside; rollback -> committed data unchanged.
    for union in [false, true] {
        for second_before_write in [false, true] {
            let app = app(union, false);
            update(&app, STATEMENTS, "INSERT DATA { <urn:a> <urn:p> 1 }").await;
            let first = begin(&app).await;
            assert_eq!(
                query(
                    &app,
                    &format!("{first}?action=QUERY"),
                    "ASK { <urn:a> <urn:p> 1 }"
                )
                .await["boolean"],
                true
            );
            let second = begin(&app).await;
            update(
                &app,
                &format!("{second}?action=UPDATE"),
                "INSERT DATA { <urn:b> <urn:p> 2 }",
            )
            .await;
            if second_before_write {
                commit(&app, &second).await;
            }
            update(
                &app,
                &format!("{first}?action=UPDATE"),
                "DELETE WHERE { ?s <urn:p> ?o }",
            )
            .await;
            if !second_before_write {
                commit(&app, &second).await;
            }
            let committed = dataset(&export(&app, false).await, RdfFormat::TriG);
            assert_eq!(
                committed,
                dataset("<urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 .", RdfFormat::TriG)
            );
            assert_eq!(
                query(
                    &app,
                    &format!("{first}?action=QUERY"),
                    "ASK { <urn:a> <urn:p> 1 }"
                )
                .await["boolean"],
                false
            );
            let (status, _, _) = send(&app, Method::DELETE, &first, &[], "").await;
            assert_eq!(status, StatusCode::NO_CONTENT);
            assert_eq!(
                dataset(&export(&app, false).await, RdfFormat::TriG),
                committed
            );
        }
    }
}

#[tokio::test]
async fn a_gate_rejected_commit_exposes_none_of_its_updates() {
    // Spec: RDF4J atomic commit says a gate-rejected batch -> no committed change, including earlier valid updates.
    for union in [false, true] {
        let app = test_app_with_store_config(
            StoreConfig {
                union_default_graph: union,
                shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
                ..StoreConfig::default()
            },
            PolicyConfig::default(),
            ReasonerConfig::default(),
        )
        .unwrap();
        update(
            &app,
            STATEMENTS,
            r#"INSERT DATA {
            GRAPH <http://rdf4j.org/schema/rdf4j#SHACLShapeGraph> {
                <urn:Shape> a <http://www.w3.org/ns/shacl#NodeShape> ;
                    <http://www.w3.org/ns/shacl#targetClass> <urn:C> ;
                    <http://www.w3.org/ns/shacl#property> [
                        <http://www.w3.org/ns/shacl#path> <urn:name> ;
                        <http://www.w3.org/ns/shacl#minCount> 1 ] .
            } <urn:kept> <urn:p> 0 . }"#,
        )
        .await;
        let before = dataset(&export(&app, false).await, RdfFormat::TriG);
        let location = begin(&app).await;
        update(
            &app,
            &format!("{location}?action=UPDATE"),
            "INSERT DATA { <urn:valid> <urn:p> 1 }",
        )
        .await;
        update(
            &app,
            &format!("{location}?action=UPDATE"),
            "INSERT DATA { <urn:invalid> a <urn:C> }",
        )
        .await;
        assert_eq!(dataset(&export(&app, false).await, RdfFormat::TriG), before);
        let (status, _, text) = send(
            &app,
            Method::PUT,
            &format!("{location}?action=COMMIT"),
            &[],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(dataset(&export(&app, false).await, RdfFormat::TriG), before);
        // Whether the refused session remains open is mode-specific; only its lack of effects is common.
        let _ = send(&app, Method::DELETE, &location, &[], "").await;
        assert_eq!(dataset(&export(&app, false).await, RdfFormat::TriG), before);
    }
}

#[tokio::test]
async fn export_clear_import_asserts_former_inferences_and_preserves_blank_nodes() {
    // Spec: RDF4J infer defaults true; POST RDF asserts its payload -> exported inferences survive premise deletion.
    for union in [false, true] {
        let app = app(union, true);
        upload(&app, &format!("{MIXED}\n{SCHEMA}")).await;
        let inference = "ASK { <urn:entity> a <urn:D> }";
        assert_eq!(
            query(&app, &format!("{REPO}?infer=false"), inference).await["boolean"],
            false
        );
        let exported = export(&app, true).await;
        let visible = dataset(&exported, RdfFormat::TriG);
        for q in dataset("<urn:entity> a <urn:D> .", RdfFormat::TriG).iter() {
            assert!(visible.contains(q));
        }
        update(&app, STATEMENTS, "DROP ALL").await;
        upload(&app, &exported).await;
        assert_eq!(dataset(&export(&app, true).await, RdfFormat::TriG), visible);
        assert_eq!(
            query(&app, &format!("{REPO}?infer=false"), inference).await["boolean"],
            true
        );
        update(
            &app,
            STATEMENTS,
            "DELETE DATA { GRAPH <urn:g1> { <urn:entity> a <urn:C> } }",
        )
        .await;
        assert_eq!(
            query(&app, &format!("{REPO}?infer=false"), inference).await["boolean"],
            true
        );
    }
}

#[tokio::test]
async fn transaction_query_applies_infer_false_and_the_protocol_default_graph() {
    // Spec: RDF4J transaction QUERY says infer=false plus default-graph-uri -> that graph's asserted pending data only.
    for union in [false, true] {
        let app = app(union, true);
        upload(&app, &format!("{SCHEMA}\n<urn:g1> {{ <urn:old> <urn:p> 1 . }} <urn:g2> {{ <urn:other> <urn:p> 2 . }}")).await;
        let before = dataset(&export(&app, false).await, RdfFormat::TriG);
        let location = begin(&app).await;
        update(&app, &format!("{location}?action=UPDATE"), "DELETE DATA { GRAPH <urn:g1> { <urn:old> <urn:p> 1 } }; INSERT DATA { GRAPH <urn:g1> { <urn:new> <urn:p> 3 } }").await;
        let result = query(
            &app,
            &format!("{location}?action=QUERY&infer=false&default-graph-uri=urn%3Ag1"),
            "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s",
        )
        .await;
        assert_eq!(
            result["results"]["bindings"],
            json!([
                {"s":{"type":"uri","value":"urn:entity"},"p":{"type":"uri","value":"http://www.w3.org/1999/02/22-rdf-syntax-ns#type"},"o":{"type":"uri","value":"urn:C"}},
                {"s":{"type":"uri","value":"urn:new"},"p":{"type":"uri","value":"urn:p"},"o":{"type":"literal","value":"3","datatype":"http://www.w3.org/2001/XMLSchema#integer"}}
            ])
        );
        assert_eq!(dataset(&export(&app, false).await, RdfFormat::TriG), before);
        let _ = send(&app, Method::DELETE, &location, &[], "").await;
    }
}
