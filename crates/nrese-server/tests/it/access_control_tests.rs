//! Graph-level access control end to end (`auth.access_policy`): what users of different
//! roles see and may change through SPARQL, the Graph Store Protocol and RDF4J's REST API.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use jsonwebtoken::{EncodingKey, Header, encode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::auth::{AuthConfig, JwtBearerConfig};
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;
use serde::Serialize;
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_settings, test_app_with_store_config};

const EX: &str = "http://example.com/";

const POLICY: &str = r#"
default = "deny"
inferred = "hidden"

[[role]]
name = "analyst"
read = ["http://example.com/g/pub/*"]
write = ["http://example.com/g/analyst/*"]
default_graph = "read"

[[role]]
name = "public"
read = ["http://example.com/g/pub/*"]
"#;

#[derive(Serialize)]
struct Claims {
    exp: usize,
    roles: Vec<String>,
}

fn token(roles: &[&str]) -> String {
    encode(
        &Header::default(),
        &Claims {
            exp: 4_102_444_800,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        },
        &EncodingKey::from_secret(b"test-secret"),
    )
    .expect("jwt")
}

fn app() -> Result<axum::Router, Box<dyn std::error::Error>> {
    app_with(POLICY)
}

fn app_with(policy: &str) -> Result<axum::Router, Box<dyn std::error::Error>> {
    app_in(policy, ReasoningMode::Rdfs)
}

fn app_in(policy: &str, mode: ReasoningMode) -> Result<axum::Router, Box<dyn std::error::Error>> {
    test_app_with_settings(policy_config(policy)?, ReasonerConfig::for_mode(mode))
}

/// [`app_with`] over a store configured as `store`.
fn app_with_store(
    policy: &str,
    store: StoreConfig,
) -> Result<axum::Router, Box<dyn std::error::Error>> {
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Rdfs);
    test_app_with_store_config(store, policy_config(policy)?, reasoner)
}

fn policy_config(policy: &str) -> Result<PolicyConfig, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("access.toml");
    std::fs::write(&path, policy)?;
    Ok(PolicyConfig {
        auth: AuthConfig::BearerJwt(JwtBearerConfig {
            shared_secret: "test-secret".to_owned(),
            issuer: None,
            audience: None,
            read_role: "nrese.read".to_owned(),
            admin_role: "nrese.admin".to_owned(),
            leeway_seconds: 0,
        }),
        access: Some(Arc::new(nrese_server::access::load(&path)?)),
        ..PolicyConfig::default()
    })
}

async fn send(
    app: &axum::Router,
    roles: &[&str],
    method: Method,
    uri: &str,
    content_type: Option<&str>,
    body: &str,
) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let mut request = Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", format!("Bearer {}", token(roles)))
        .header(
            "accept",
            "application/sparql-results+json, application/n-quads;q=0.9, */*;q=0.1",
        );
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned()))?)
        .await?;
    let status = response.status();
    Ok((status, body_text(response).await?))
}

async fn update(
    app: &axum::Router,
    roles: &[&str],
    update: &str,
) -> Result<StatusCode, Box<dyn std::error::Error>> {
    let (status, _) = send(
        app,
        roles,
        Method::POST,
        "/dataset/update",
        Some("application/sparql-update"),
        update,
    )
    .await?;
    Ok(status)
}

/// The status and body of `query` sent by a requester with `roles` (`POST /dataset/query`).
async fn sparql_query(
    app: &axum::Router,
    roles: &[&str],
    query: &str,
) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    send(
        app,
        roles,
        Method::POST,
        "/dataset/query",
        Some("application/sparql-query"),
        query,
    )
    .await
}

/// The value of the line of `/metrics` that starts with `name`, read as the admin.
async fn metric(app: &axum::Router, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let (status, text) = send(app, &["nrese.admin"], Method::GET, "/metrics", None, "").await?;
    assert_eq!(status, StatusCode::OK, "{text}");
    let line = text
        .lines()
        .find(|line| line.starts_with(name))
        .ok_or_else(|| format!("no {name} in /metrics"))?;
    Ok(line.rsplit(' ').next().unwrap_or_default().parse()?)
}

/// The values of `?x` in the query's solutions, sorted.
async fn select(
    app: &axum::Router,
    roles: &[&str],
    query: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let (status, body) = send(
        app,
        roles,
        Method::POST,
        "/dataset/query",
        Some("application/sparql-query"),
        query,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let mut values: Vec<String> = json["results"]["bindings"]
        .as_array()
        .ok_or("no bindings")?
        .iter()
        .map(|row| row["x"]["value"].as_str().unwrap_or_default().to_owned())
        .collect();
    values.sort();
    Ok(values)
}

async fn seeded() -> Result<axum::Router, Box<dyn std::error::Error>> {
    let app = app()?;
    let status = update(
        &app,
        &["nrese.admin"],
        &format!(
            "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{
               <{EX}a> a <{EX}C> . <{EX}C> rdfs:subClassOf <{EX}D> .
               GRAPH <{EX}g/pub/1> {{ <{EX}p1> <{EX}p> 1 }}
               GRAPH <{EX}g/secret> {{ <{EX}s1> <{EX}p> 2 }}
               GRAPH <{EX}g/analyst/notes> {{ <{EX}n1> <{EX}p> 3 }}
             }}"
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    Ok(app)
}

const GRAPHS: &str = "SELECT DISTINCT ?x WHERE { GRAPH ?x { ?s ?p ?o } }";

// Spec: D16-310's task makes public readiness status-only, including for a reader.
#[tokio::test]
async fn readiness_disclosure_public_status_only() -> Result<(), Box<dyn std::error::Error>> {
    let app = seeded().await?;
    for roles in [None, Some(&["analyst"][..]), Some(&["nrese.admin"][..])] {
        let mut request = Request::builder().uri("/readyz");
        if let Some(roles) = roles {
            request = request.header("authorization", format!("Bearer {}", token(roles)));
        }
        let response = app.clone().oneshot(request.body(Body::empty())?).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
        assert_eq!(json, serde_json::json!({"status": "ready"}));
    }
    Ok(())
}

// Spec: D16-310 denies global figures to restricted readers on either info alias.
#[tokio::test]
async fn readiness_disclosure_restricted_info() -> Result<(), Box<dyn std::error::Error>> {
    let app = seeded().await?;
    for uri in ["/dataset/info", "/api/v1/repositories/nrese/info"] {
        let (status, body) = send(&app, &["analyst"], Method::GET, uri, None, "").await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        for field in [
            "quad_count",
            "named_graph_count",
            "violations",
            "engine",
            "revision",
        ] {
            assert!(!body.contains(field), "{body}");
        }
    }
    Ok(())
}

// Spec: D16-310 preserves detailed monitoring for full readers/admins, and defaults.
#[tokio::test]
async fn readiness_disclosure_authorized_details() -> Result<(), Box<dyn std::error::Error>> {
    // With fallback allow, the normal read grant has an unrestricted view; the analyst
    // still has its explicit restricted rule. DL is active so its fields are exercised.
    let policy = POLICY.replacen("default = \"deny\"", "default = \"allow\"", 1);
    let app = app_in(&policy, ReasoningMode::Owl2Dl)?;
    for roles in [&["nrese.read"][..], &["nrese.admin"][..]] {
        for uri in ["/dataset/info", "/api/v1/repositories/nrese/info"] {
            let (status, body) = send(&app, roles, Method::GET, uri, None, "").await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let json: serde_json::Value = serde_json::from_str(&body)?;
            assert!(json["quad_count"].is_number(), "{json}");
            assert!(json["named_graph_count"].is_number(), "{json}");
            assert!(json.get("violations").is_some(), "{json}");
            assert!(json["dl"]["engine"].is_string(), "{json}");
            assert!(json["dl"].get("reason").is_some(), "{json}");
        }
    }
    let app = crate::support::test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/info")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(json["quad_count"], 0);
    Ok(())
}

#[tokio::test]
async fn each_role_queries_its_own_dataset() -> Result<(), Box<dyn std::error::Error>> {
    let app = seeded().await?;
    let g = |name: &str| format!("{EX}g/{name}");
    assert_eq!(
        select(&app, &["nrese.admin"], GRAPHS).await?,
        vec![g("analyst/notes"), g("pub/1"), g("secret")]
    );
    // Roles get read access from the policy alone (no `nrese.read`).
    assert_eq!(
        select(&app, &["analyst"], GRAPHS).await?,
        vec![g("analyst/notes"), g("pub/1")]
    );
    assert_eq!(select(&app, &["public"], GRAPHS).await?, vec![g("pub/1")]);
    // A forbidden graph is absent, even by name.
    let secret = format!("SELECT ?x WHERE {{ GRAPH <{EX}g/secret> {{ ?x ?p ?o }} }}");
    assert!(select(&app, &["analyst"], &secret).await?.is_empty());
    // The default graph: asserted statements for the analyst (inferred ones are hidden),
    // nothing for the public role.
    let types = format!("SELECT ?x WHERE {{ <{EX}a> a ?x FILTER(STRSTARTS(STR(?x), \"{EX}\")) }}");
    assert_eq!(
        select(&app, &["nrese.admin"], &types).await?,
        vec![format!("{EX}C"), format!("{EX}D")]
    );
    assert_eq!(
        select(&app, &["analyst"], &types).await?,
        vec![format!("{EX}C")]
    );
    assert!(select(&app, &["public"], &types).await?.is_empty());
    // A role the policy doesn't name reads nothing under `default = "deny"`.
    assert!(select(&app, &["nrese.read"], GRAPHS).await?.is_empty());
    // Without a rule nor a grant: no access at all.
    let (status, _) = send(
        &app,
        &["nobody"],
        Method::POST,
        "/dataset/query",
        Some("application/sparql-query"),
        GRAPHS,
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn updates_change_only_writable_graphs() -> Result<(), Box<dyn std::error::Error>> {
    let app = seeded().await?;
    let analyst = &["analyst"];
    // Its own graphs: allowed, from the policy alone.
    let insert =
        |graph: &str| format!("INSERT DATA {{ GRAPH <{EX}g/{graph}> {{ <{EX}x> <{EX}p> 9 }} }}");
    assert_eq!(
        update(&app, analyst, &insert("analyst/new")).await?,
        StatusCode::NO_CONTENT
    );
    // Readable but not writable, unreadable, the default graph: refused, whether the
    // statement is there or not.
    assert_eq!(
        update(&app, analyst, &insert("pub/1")).await?,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        update(&app, analyst, &insert("secret")).await?,
        StatusCode::FORBIDDEN
    );
    // Re-inserting a statement that is there changes nothing, and is still refused: the
    // answer doesn't tell whether a graph the role can't read holds it.
    assert_eq!(
        update(
            &app,
            analyst,
            &format!("INSERT DATA {{ GRAPH <{EX}g/secret> {{ <{EX}s1> <{EX}p> 2 }} }}")
        )
        .await?,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        update(
            &app,
            analyst,
            &format!("INSERT DATA {{ <{EX}x> <{EX}p> 9 }}")
        )
        .await?,
        StatusCode::FORBIDDEN
    );
    // A WHERE clause sees the readable graphs: copying them into a writable graph copies
    // nothing secret.
    assert_eq!(
        update(
            &app,
            analyst,
            &format!(
                "INSERT {{ GRAPH <{EX}g/analyst/copy> {{ ?s ?p ?o }} }} WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}"
            )
        )
        .await?,
        StatusCode::NO_CONTENT
    );
    let copied = format!("SELECT ?x WHERE {{ GRAPH <{EX}g/analyst/copy> {{ ?x ?p ?o }} }}");
    assert_eq!(
        select(&app, &["nrese.admin"], &copied).await?,
        vec![format!("{EX}n1"), format!("{EX}p1"), format!("{EX}x")]
    );
    // Deleting everything it sees would change a public graph: refused as a whole.
    assert_eq!(
        update(&app, analyst, "DELETE WHERE { GRAPH ?g { ?s ?p ?o } }").await?,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        update(&app, analyst, "CLEAR ALL").await?,
        StatusCode::FORBIDDEN
    );
    // An unreadable graph is absent: dropping it does nothing.
    assert_eq!(
        update(&app, analyst, &format!("DROP SILENT GRAPH <{EX}g/secret>")).await?,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        select(&app, &["nrese.admin"], GRAPHS).await?.len(),
        5,
        "nothing refused was changed"
    );
    // CLEAR GRAPH of its own graph.
    assert_eq!(
        update(&app, analyst, &format!("CLEAR GRAPH <{EX}g/analyst/copy>")).await?,
        StatusCode::NO_CONTENT
    );
    // The public role may not write at all.
    assert_eq!(
        update(&app, &["public"], &insert("pub/1")).await?,
        StatusCode::FORBIDDEN
    );
    Ok(())
}

/// Spec (S4, `ReadScope::require_all`): an explanation under OWL 2 DL reads every graph, so
/// a reader of some graphs is refused it (403), though it sees inferred statements; a
/// reader of every graph gets it.
#[tokio::test]
async fn an_owl2_dl_explanation_is_refused_to_a_reader_of_some_graphs()
-> Result<(), Box<dyn std::error::Error>> {
    const READER: &str = r#"
default = "deny"
inferred = "visible"

[[role]]
name = "reader"
read = ["http://example.com/g/pub/*"]
default_graph = "read"
"#;
    let app = app_in(READER, ReasoningMode::Owl2Dl)?;
    let status = update(
        &app,
        &["nrese.admin"],
        &format!(
            "PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{
               <{EX}B> rdfs:subClassOf <{EX}D> . <{EX}C> rdfs:subClassOf <{EX}D> .
               <{EX}w> a [ owl:unionOf ( <{EX}B> <{EX}C> ) ]
             }}"
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let explain = format!(
        "/api/v1/repositories/nrese/explain?{}",
        serde_urlencoded::to_string([
            ("subj", format!("<{EX}w>")),
            (
                "pred",
                "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>".to_owned(),
            ),
            ("obj", format!("<{EX}D>")),
        ])?
    );
    let (status, body) = send(&app, &["reader"], Method::GET, &explain, None, "").await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("reads every graph"), "{body}");
    let (status, body) = send(&app, &["nrese.admin"], Method::GET, &explain, None, "").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(())
}

#[tokio::test]
async fn graph_store_and_rdf4j_follow_the_policy() -> Result<(), Box<dyn std::error::Error>> {
    let app = seeded().await?;
    let analyst = &["analyst"];
    let graph = |name: &str| format!("/dataset/data?graph={EX}g/{name}");
    let (status, _) = send(&app, analyst, Method::GET, &graph("pub/1"), None, "").await?;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, analyst, Method::GET, &graph("secret"), None, "").await?;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unreadable graph is absent"
    );
    let turtle = Some("text/turtle");
    let triple = format!("<{EX}y> <{EX}p> 1 .");
    let (status, _) = send(
        &app,
        analyst,
        Method::PUT,
        &graph("analyst/gsp"),
        turtle,
        &triple,
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = send(&app, analyst, Method::PUT, &graph("pub/1"), turtle, &triple).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&app, analyst, Method::DELETE, &graph("secret"), None, "").await?;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // RDF4J: contexts, size and statements count what the role may read.
    let (status, body) = send(
        &app,
        analyst,
        Method::GET,
        "/repositories/nrese/contexts",
        None,
        "",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("g/pub/1") && !body.contains("g/secret"),
        "{body}"
    );
    let (_, size) = send(
        &app,
        analyst,
        Method::GET,
        "/repositories/nrese/size",
        None,
        "",
    )
    .await?;
    // Two asserted statements in the default graph, one each in pub/1, analyst/notes and
    // analyst/gsp; the secret one and the inferred ones don't count.
    assert_eq!(size.trim(), "5");
    let (_, statements) = send(
        &app,
        analyst,
        Method::GET,
        "/repositories/nrese/statements",
        None,
        "",
    )
    .await?;
    assert!(!statements.contains("s1"), "{statements}");
    // A statement deletion without context removes only what the role sees, and is
    // refused if that includes a graph it may not write.
    let (status, _) = send(
        &app,
        analyst,
        Method::DELETE,
        &format!("/repositories/nrese/statements?pred=%3C{EX}p%3E"),
        None,
        "",
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &app,
        analyst,
        Method::DELETE,
        &format!("/repositories/nrese/statements?subj=%3C{EX}y%3E"),
        None,
        "",
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Deletions remove asserted statements only: one whose pattern matches nothing
    // but an inferred statement (`a a D`) changes nothing and is allowed, even in a graph
    // the role may not write.
    let (status, _) = send(
        &app,
        analyst,
        Method::DELETE,
        &format!("/repositories/nrese/statements?subj=%3C{EX}a%3E&obj=%3C{EX}D%3E"),
        None,
        "",
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Endpoints over the whole dataset are for users who may read every graph.
    let (status, _) = send(
        &app,
        analyst,
        Method::GET,
        "/dataset/autocomplete?q=a",
        None,
        "",
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &app,
        &["nrese.admin"],
        Method::GET,
        "/dataset/autocomplete?q=a",
        None,
        "",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}

/// A reader of some graphs who sees every inference is told the same over HTTP whether or
/// not a graph it can't read holds schema the OWL 2 QL rewriting would use: the same
/// answer, and the completeness `not-certified`, which other origins may read; and from
/// an EXPLAIN, run or plan only, the same completeness and no QL report (R13-diag).
#[tokio::test]
async fn query_certification_tells_nothing_of_a_hidden_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let policy = POLICY.replace("inferred = \"hidden\"", "inferred = \"visible\"");
    let query = |uri: &str| {
        Request::builder()
            .uri(uri)
            .method(Method::POST)
            .header(
                "authorization",
                format!("Bearer {}", token(&["nrese.read", "analyst"])),
            )
            .header(
                "accept",
                "application/sparql-results+json, application/n-quads;q=0.9, */*;q=0.1",
            )
            .header("content-type", "application/sparql-query")
            .body(Body::from(format!("ASK {{ ?x <{EX}p> ?y }}")))
    };
    let mut told = Vec::new();
    let mut explained = Vec::new();
    for hidden in [false, true] {
        let app = app_in(&policy, ReasoningMode::Owl2Ql)?;
        let schema = if hidden {
            format!(
                "GRAPH <{EX}g/secret> {{
                   <{EX}A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r .
                   _:r a <http://www.w3.org/2002/07/owl#Restriction> ;
                     <http://www.w3.org/2002/07/owl#onProperty> <{EX}p> ;
                     <http://www.w3.org/2002/07/owl#someValuesFrom> <{EX}B> .
                   <{EX}p> a <http://www.w3.org/2002/07/owl#TransitiveProperty> . }}"
            )
        } else {
            String::new()
        };
        assert_eq!(
            update(
                &app,
                &["nrese.admin"],
                &format!(
                    "INSERT DATA {{ GRAPH <{EX}g/pub/a> {{ <{EX}s> <{EX}q> <{EX}o> }} {schema} }}"
                ),
            )
            .await?,
            StatusCode::NO_CONTENT
        );
        for mode in ["true", "plan"] {
            let response = app
                .clone()
                .oneshot(query(&format!("/dataset/query?explain={mode}"))?)
                .await?;
            assert_eq!(response.status(), StatusCode::OK, "explain={mode}");
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
            let explanation: serde_json::Value = serde_json::from_slice(&body)?;
            explained.push((
                mode,
                explanation["completeness"].clone(),
                explanation["ql"].clone(),
            ));
        }
        // Not `send`: it drops the headers.
        let response = app.oneshot(query("/dataset/query")?).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let header = |name: &str| -> Vec<Vec<u8>> {
            response
                .headers()
                .get_all(name)
                .iter()
                .map(|value| value.as_bytes().to_vec())
                .collect()
        };
        let completeness = header("nrese-completeness");
        assert_eq!(
            header("access-control-expose-headers"),
            [b"nrese-completeness".to_vec()],
            "hidden {hidden}"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        told.push((body, completeness));
    }
    assert_eq!(
        told[0], told[1],
        "a hidden schema changed what the reader is told"
    );
    assert_eq!(told[0].1, [b"not-certified".to_vec()]);
    let (without, with) = explained.split_at(2);
    assert_eq!(
        without, with,
        "a hidden schema changed what an EXPLAIN tells the reader"
    );
    for (mode, completeness, ql) in without {
        assert_eq!(completeness["status"], "not-certified", "explain={mode}");
        assert!(ql.is_null(), "explain={mode}: {ql}");
    }
    Ok(())
}

/// An ASK's answer in a SPARQL JSON result.
fn boolean(body: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let json: serde_json::Value = serde_json::from_str(body)?;
    Ok(json["boolean"]
        .as_bool()
        .ok_or_else(|| format!("no boolean: {body}"))?)
}

/// The answer of the ASK `query` as `roles` read.
async fn ask(
    app: &axum::Router,
    roles: &[&str],
    query: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let (status, body) = send(
        app,
        roles,
        Method::POST,
        "/dataset/query",
        Some("application/sparql-query"),
        query,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    boolean(&body)
}

/// An update's WHERE reads what its requester reads, as its queries do (review r1 of PR #50,
/// F1). For a reader of what its graphs support (`inferred = "supported"`), `open` derives
/// `alice a Patient` whether or not `secret`, which it can't read, asserts it; so a marker its
/// update inserts where that holds lands either way: through SPARQL UPDATE, and through an
/// RDF4J transaction's UPDATE, read inside the transaction before any commit.
#[tokio::test]
async fn an_updates_where_reads_what_its_requester_reads() -> Result<(), Box<dyn std::error::Error>>
{
    const SUPPORTED: &str = r#"
default = "deny"
inferred = "supported"

[[role]]
name = "editor"
read = ["http://example.com/g/open"]
write = ["http://example.com/g/open"]
default_graph = "read"
"#;
    let editor = &["editor"];
    let marker = |name: &str| format!("<{EX}{name}> <{EX}observed> true");
    let insert = |name: &str| {
        format!(
            "INSERT {{ GRAPH <{EX}g/open> {{ {} }} }} WHERE {{ <{EX}alice> a <{EX}Patient> }}",
            marker(name)
        )
    };
    let holds = |name: &str| format!("ASK {{ GRAPH <{EX}g/open> {{ {} }} }}", marker(name));
    let mut seen = Vec::new();
    for hidden in [false, true] {
        let app = app_in(SUPPORTED, ReasoningMode::Owl2Rl)?;
        let secret = match hidden {
            true => format!("GRAPH <{EX}g/secret> {{ <{EX}alice> a <{EX}Patient> }}"),
            false => String::new(),
        };
        let data = format!(
            "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ GRAPH <{EX}g/open> {{ <{EX}alice> a <{EX}Human> .
                            <{EX}Human> rdfs:subClassOf <{EX}Patient> }} {secret} }}"
        );
        assert_eq!(
            update(&app, &["nrese.admin"], &data).await?,
            StatusCode::NO_CONTENT
        );
        let derived = ask(
            &app,
            editor,
            &format!("ASK {{ <{EX}alice> a <{EX}Patient> }}"),
        )
        .await?;
        assert_eq!(
            update(&app, editor, &insert("committed")).await?,
            StatusCode::NO_CONTENT
        );
        let committed = ask(&app, editor, &holds("committed")).await?;
        // An RDF4J transaction: its UPDATE pending, read inside it.
        let begin = Request::builder()
            .method(Method::POST)
            .uri("/repositories/nrese/transactions")
            .header("authorization", format!("Bearer {}", token(editor)))
            .body(Body::empty())?;
        let response = app.clone().oneshot(begin).await?;
        assert_eq!(response.status(), StatusCode::CREATED);
        let tx = response.headers()[axum::http::header::LOCATION]
            .to_str()?
            .to_owned();
        let (status, body) = send(
            &app,
            editor,
            Method::PUT,
            &format!("{tx}?action=UPDATE"),
            Some("application/x-www-form-urlencoded"),
            &serde_urlencoded::to_string([("update", insert("pending"))])?,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = send(
            &app,
            editor,
            Method::PUT,
            &format!(
                "{tx}?action=QUERY&{}",
                serde_urlencoded::to_string([("query", holds("pending"))])?
            ),
            None,
            "",
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        seen.push((derived, committed, boolean(&body)?));
    }
    assert_eq!(seen[0], (true, true, true), "derived from open");
    assert_eq!(seen[1], seen[0], "alice a Patient asserted in secret");
    Ok(())
}

/// An update's WHERE reads no inferred statement its requester's queries don't show (review r1
/// of PR #50, D7): with `inferred = "hidden"`, `open` derives `alice a Patient`, which the
/// editor's ASK doesn't see, and its update inserts no marker where it holds.
#[tokio::test]
async fn an_updates_where_reads_no_inferred_statement_its_requester_doesnt_see()
-> Result<(), Box<dyn std::error::Error>> {
    const HIDDEN: &str = r#"
default = "deny"
inferred = "hidden"

[[role]]
name = "editor"
read = ["http://example.com/g/open"]
write = ["http://example.com/g/open"]
default_graph = "read"
"#;
    let editor = &["editor"];
    let app = app_in(HIDDEN, ReasoningMode::Owl2Rl)?;
    let data = format!(
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA {{ GRAPH <{EX}g/open> {{ <{EX}alice> a <{EX}Human> .
                        <{EX}Human> rdfs:subClassOf <{EX}Patient> }} }}"
    );
    assert_eq!(
        update(&app, &["nrese.admin"], &data).await?,
        StatusCode::NO_CONTENT
    );
    let derived = format!("ASK {{ <{EX}alice> a <{EX}Patient> }}");
    assert!(
        ask(&app, &["nrese.admin"], &derived).await?,
        "the store derives it"
    );
    assert!(
        !ask(&app, editor, &derived).await?,
        "the editor doesn't see it"
    );
    assert_eq!(
        update(
            &app,
            editor,
            &format!(
                "INSERT {{ GRAPH <{EX}g/open> {{ <{EX}marker> <{EX}observed> true }} }}
                 WHERE {{ <{EX}alice> a <{EX}Patient> }}"
            )
        )
        .await?,
        StatusCode::NO_CONTENT
    );
    let marked = format!("ASK {{ GRAPH <{EX}g/open> {{ <{EX}marker> <{EX}observed> true }} }}");
    assert!(
        !ask(&app, &["nrese.admin"], &marked).await?,
        "the update's WHERE read an inferred statement the editor doesn't see"
    );
    Ok(())
}

/// Whether a query's results announce RDF 1.2 (the media type's `version`, JSON's
/// `version` member, `VERSION "1.2"` first in a graph result) tells a reader nothing of a
/// graph it can't read: a triple term only there, or derived only from there, is announced
/// as none at all; one the reader sees is, as its results may then hold it. The media type
/// and the body always say the same.
#[tokio::test]
async fn the_rdf12_announcement_tells_nothing_of_a_graph_the_reader_cannot_read()
-> Result<(), Box<dyn std::error::Error>> {
    let term = format!("<<( <{EX}x> <{EX}y> <{EX}z> )>>");
    let says = format!("<{EX}s> <{EX}says> {term}");
    let claims = format!("<{EX}s> <{EX}claims> {term}");
    let schema =
        format!("<{EX}says> <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <{EX}claims>");
    let secret = |data: &str| format!("GRAPH <{EX}g/secret> {{ {data} }}");
    let open = |data: &str| format!("GRAPH <{EX}g/pub/1> {{ {data} }}");
    let readers = [
        (
            "a reader who sees no inference",
            "hidden",
            [
                ("no term", String::new()),
                ("one only in a graph it can't read", secret(&says)),
                ("one in a graph it reads", open(&says)),
            ],
            ["none", "none", "1.2"],
        ),
        (
            "a reader of what its graphs support",
            "supported",
            [
                ("no term", String::new()),
                (
                    "one derived only from a graph it can't read",
                    format!("{} {}", secret(&says), open(&schema)),
                ),
                (
                    "one in a graph it reads",
                    open(&format!("{says} . {schema}")),
                ),
            ],
            ["none", "none", "1.2"],
        ),
        (
            "a reader of every inference",
            "visible",
            [
                ("no term", String::new()),
                (
                    "one asserted only in a graph it can't read and derived",
                    format!(
                        "{} {}",
                        secret(&format!("{says} . {claims}")),
                        open(&schema)
                    ),
                ),
                (
                    "one in a graph it reads",
                    open(&format!("{says} . {schema}")),
                ),
            ],
            ["none", "1.2", "1.2"],
        ),
    ];
    let (json, member) = ("application/sparql-results+json", "\"version\":\"1.2\"");
    let forms = [
        ("SELECT", "SELECT * WHERE { ?s ?p ?o }", json, member),
        ("ASK", "ASK { ?s ?p ?o }", json, member),
        (
            "CONSTRUCT",
            "CONSTRUCT WHERE { ?s ?p ?o }",
            "text/turtle",
            "VERSION \"1.2\"\n",
        ),
    ];
    let mut wrong = Vec::new();
    for (reader, inferred, variants, want) in &readers {
        let policy = POLICY.replace(
            "inferred = \"hidden\"",
            &format!("inferred = \"{inferred}\""),
        );
        for (form, query, accept, marker) in forms {
            let mut told = Vec::new();
            for (_, data) in variants {
                let app = app_with(&policy)?;
                let insert = format!(
                    "INSERT DATA {{ GRAPH <{EX}g/pub/1> {{ <{EX}a> <{EX}p> <{EX}b> }} {data} }}"
                );
                assert_eq!(
                    update(&app, &["nrese.admin"], &insert).await?,
                    StatusCode::NO_CONTENT
                );
                // Not `send`: it drops the headers.
                let request = Request::builder()
                    .uri("/dataset/query")
                    .method(Method::POST)
                    .header(
                        "authorization",
                        format!("Bearer {}", token(&["nrese.read", "analyst"])),
                    )
                    .header("accept", accept)
                    .header("content-type", "application/sparql-query")
                    .body(Body::from(query))?;
                let response = app.oneshot(request).await?;
                assert_eq!(response.status(), StatusCode::OK, "{form} to {reader}");
                let header = response
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.contains("version=1.2"));
                let body = body_text(response).await?;
                let body = if form == "CONSTRUCT" {
                    body.starts_with(marker)
                } else {
                    body.contains(marker)
                };
                let version = |announced: bool| if announced { "1.2" } else { "none" };
                told.push(format!("header {} body {}", version(header), version(body)));
            }
            let got = told.join(" / ");
            let want = want.map(|v| format!("header {v} body {v}")).join(" / ");
            let names = variants.each_ref().map(|(name, _)| *name).join(" / ");
            let cell = format!("the {form}'s announcement to {reader}, {names}");
            if got == want {
                println!("CASE {cell}: right {got}");
            } else {
                println!("CASE {cell}: WRONG {got}, want {want}");
                wrong.push(format!("{form} to {reader}"));
            }
        }
    }
    assert!(wrong.is_empty(), "the announcement told wrong: {wrong:?}");
    Ok(())
}

/// A view a reader of some graphs reads through, refused for memory, reaches it as the
/// server's 503 for its query memory, while a reader of every graph, who reads no view, is
/// answered under the same budget. The budget is just above what the admin's query took on
/// the same data (one query credit, 64 KiB, more); no view built says the refusal was the
/// view's, not the evaluation's.
#[tokio::test]
async fn a_readers_view_refused_for_memory_is_the_servers_503()
-> Result<(), Box<dyn std::error::Error>> {
    const N: usize = 20_000;
    let policy = POLICY.replace("inferred = \"hidden\"", "inferred = \"supported\"");
    let facts: String = (0..N)
        .map(|i| format!("<{EX}s{i}> <{EX}says> <{EX}o{i}> . "))
        .collect();
    let insert = format!(
        "INSERT DATA {{ GRAPH <{EX}g/secret> {{ {facts} }} GRAPH <{EX}g/pub/1> {{
           <{EX}says> <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <{EX}claims> }} }}"
    );
    let (admin, reader) = (&["nrese.admin"][..], &["nrese.read", "analyst"][..]);
    let app_at = |bytes: usize| {
        app_with_store(
            &policy,
            StoreConfig {
                total_query_memory_bytes: bytes,
                ..StoreConfig::default()
            },
        )
    };
    let ask = "ASK { ?s ?p ?o }";
    let mut peaks = Vec::new();
    for roles in [admin, reader] {
        let app = app_at(1 << 30)?;
        assert_eq!(update(&app, admin, &insert).await?, StatusCode::NO_CONTENT);
        let (status, text) = sparql_query(&app, roles, ask).await?;
        assert_eq!(status, StatusCode::OK, "{text}");
        peaks.push(metric(&app, "nrese_query_memory_peak_bytes").await?);
    }
    let budget = peaks[0] + (64 << 10);
    let app = app_at(usize::try_from(budget)?)?;
    assert_eq!(update(&app, admin, &insert).await?, StatusCode::NO_CONTENT);
    let built = "nrese_support_views_total{how=\"built\"}";
    let before = metric(&app, built).await?;
    let (status, text) = sparql_query(&app, reader, ask).await?;
    let views = metric(&app, built).await? - before;
    let sentence = match text.contains("the server's query memory is in use") {
        true => "the server's query memory is in use",
        false => "no sentence",
    };
    let (admin_status, _) = sparql_query(&app, admin, ask).await?;
    let got = format!(
        "{} {sentence}, views built {views} / {}",
        status.as_u16(),
        admin_status.as_u16()
    );
    let want = "503 the server's query memory is in use, views built 0 / 200";
    let calibrated = peaks[1] > budget;
    let cell = "the reader / an admin under a budget its view doesn't fit";
    if got == want && calibrated {
        println!("CASE {cell}: right {got}");
    } else {
        println!(
            "CASE {cell}: WRONG {got}, want {want} (peaks: admin {}, reader {}, budget {budget})",
            peaks[0], peaks[1]
        );
    }
    assert!(got == want && calibrated, "{got}: {text}");
    Ok(())
}
