//! Graph-level access control end to end (`auth.access_policy`): what users of different
//! roles see and may change through SPARQL, the Graph Store Protocol and RDF4J's REST API.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use jsonwebtoken::{EncodingKey, Header, encode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::auth::{AuthConfig, JwtBearerConfig};
use nrese_server::policy::PolicyConfig;
use serde::Serialize;
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_settings};

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
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("access.toml");
    std::fs::write(&path, policy)?;
    let policy = PolicyConfig {
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
    };
    test_app_with_settings(policy, ReasonerConfig::for_mode(mode))
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

/// N12 (research round 11): a restricted reader's EXPLAIN, the plan alone and the analysed
/// run, is the same whatever a graph it can't read holds: no plan node, estimate or time,
/// all of which follow every graph. Here the hidden graph adds 500 matches of the query's
/// pattern and a join partner for each.
#[tokio::test]
async fn explain_gives_a_restricted_reader_nothing_that_follows_hidden_graphs()
-> Result<(), Box<dyn std::error::Error>> {
    let explain = |hidden: usize| async move {
        let app = seeded().await?;
        let secret: String = (0..hidden)
            .map(|i| format!("<{EX}h{i}> <{EX}p> <{EX}o{i}> . <{EX}o{i}> <{EX}q> {i} . "))
            .collect();
        let status = update(
            &app,
            &["nrese.admin"],
            &format!("INSERT DATA {{ GRAPH <{EX}g/secret> {{ {secret} }} }}"),
        )
        .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let mut bodies = Vec::new();
        for mode in ["plan", "true"] {
            let (status, body) = send(
                &app,
                &["nrese.read", "analyst"],
                Method::POST,
                &format!("/dataset/query?explain={mode}"),
                Some("application/sparql-query"),
                &format!("SELECT ?s ?z WHERE {{ ?s <{EX}p> ?o . ?o <{EX}q> ?z }}"),
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let report: serde_json::Value = serde_json::from_str(&body)?;
            assert!(report.get("steps").is_none(), "{report}");
            bodies.push(report);
        }
        Ok::<Vec<serde_json::Value>, Box<dyn std::error::Error>>(bodies)
    };
    assert_eq!(explain(500).await?, explain(0).await?);
    Ok(())
}

/// N12 (Sol r3, F1): the same under OWL 2 QL rewriting for a reader who sees every inference
/// (`inferred = "visible"`, so the whole schema is compiled): an existential stated only in a
/// graph it can't read changes the rewriting, its report and its completeness, not what the
/// reader's EXPLAIN says. No instance of `A` anywhere, so the answers are the same.
#[tokio::test]
async fn explain_gives_a_restricted_reader_nothing_that_follows_hidden_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let policy = POLICY.replace("inferred = \"hidden\"", "inferred = \"visible\"");
    let explain = |hidden: bool| {
        let policy = policy.clone();
        async move {
            let app = app_in(&policy, ReasoningMode::Owl2Ql)?;
            let schema = match hidden {
                true => format!(
                    "GRAPH <{EX}g/secret> {{ <{EX}A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r .
                       _:r a <http://www.w3.org/2002/07/owl#Restriction> ;
                         <http://www.w3.org/2002/07/owl#onProperty> <{EX}p> ;
                         <http://www.w3.org/2002/07/owl#someValuesFrom> <{EX}B> . }}"
                ),
                false => String::new(),
            };
            // The same public statement in both worlds; the schema differs only in secret.
            let status = update(
                &app,
                &["nrese.admin"],
                &format!(
                    "INSERT DATA {{ GRAPH <{EX}g/pub/a> {{ <{EX}s> <{EX}q> <{EX}o> }} {schema} }}"
                ),
            )
            .await?;
            assert_eq!(status, StatusCode::NO_CONTENT);
            let mut bodies = Vec::new();
            for mode in ["plan", "true"] {
                let (status, body) = send(
                    &app,
                    &["nrese.read", "analyst"],
                    Method::POST,
                    &format!("/dataset/query?explain={mode}"),
                    Some("application/sparql-query"),
                    &format!("ASK {{ ?x <{EX}p> ?y }}"),
                )
                .await?;
                assert_eq!(status, StatusCode::OK, "{body}");
                bodies.push(serde_json::from_str::<serde_json::Value>(&body)?);
            }
            Ok::<Vec<serde_json::Value>, Box<dyn std::error::Error>>(bodies)
        }
    };
    assert_eq!(explain(true).await?, explain(false).await?);
    Ok(())
}

/// N12 (Sol r2, R2-F1): under `owl2-dl`, a restricted reader's `/info` keeps the DL
/// consistency word but neither its reason nor the engine that decided (chosen by the whole
/// ontology); a reader of every graph gets both.
#[tokio::test]
async fn info_gives_a_restricted_reader_no_dl_engine_or_reason()
-> Result<(), Box<dyn std::error::Error>> {
    let app = app_in(POLICY, ReasoningMode::Owl2Dl)?;
    let status = update(
        &app,
        &["nrese.admin"],
        &format!(
            "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ <{EX}a> a <{EX}C> . <{EX}C> rdfs:subClassOf <{EX}D> .
                            GRAPH <{EX}g/secret> {{ <{EX}s1> a <{EX}C> }} }}"
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let info = |roles: &'static [&'static str]| {
        let app = app.clone();
        async move {
            let (status, body) = send(&app, roles, Method::GET, "/dataset/info", None, "").await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            Ok::<serde_json::Value, Box<dyn std::error::Error>>(serde_json::from_str(&body)?)
        }
    };
    let restricted = info(&["nrese.read", "analyst"]).await?;
    let dl = restricted["dl"].as_object().ok_or("a dl status")?;
    assert!(dl.contains_key("consistency"), "{restricted}");
    assert!(
        !dl.contains_key("engine") && !dl.contains_key("reason"),
        "{restricted}"
    );
    let all = info(&["nrese.admin"]).await?;
    assert!(all["dl"]["engine"].is_string(), "{all}");
    Ok(())
}

/// N12 (the orchestrator's decision on `/readyz`): the unauthenticated probe answers its
/// status and what is degraded alone, with no store-wide count; the counts are `/info`'s,
/// for an authenticated reader of every graph.
#[tokio::test]
async fn the_unauthenticated_probe_carries_no_counts() -> Result<(), Box<dyn std::error::Error>> {
    let app = seeded().await?;
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/readyz").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let probe: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    let mut keys: Vec<&str> = probe
        .as_object()
        .ok_or("an object")?
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["degraded", "status"], "{probe}");
    assert_eq!(probe["status"], "ready");
    Ok(())
}

/// N12 (attack review r1, P2-8): `/info` gives a reader who may not read every graph none
/// of the store-wide counts, which follow graphs it can't read; a reader of every graph
/// gets them.
#[tokio::test]
async fn info_gives_a_restricted_reader_no_store_wide_counts()
-> Result<(), Box<dyn std::error::Error>> {
    let app = seeded().await?;
    let info = |roles: &'static [&'static str]| {
        let app = app.clone();
        async move {
            let (status, body) = send(&app, roles, Method::GET, "/dataset/info", None, "").await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            Ok::<serde_json::Value, Box<dyn std::error::Error>>(serde_json::from_str(&body)?)
        }
    };
    let restricted = info(&["nrese.read", "analyst"]).await?;
    for key in ["quad_count", "named_graph_count", "violations"] {
        assert!(restricted.get(key).is_none(), "{key}: {restricted}");
    }
    assert!(restricted.get("status").is_some(), "{restricted}");
    let all = info(&["nrese.admin"]).await?;
    assert!(all["quad_count"].as_u64().is_some_and(|n| n > 0), "{all}");
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

/// Restricted-visible ordinary QL certification reveals no hidden schema; real HTTP emission.
#[tokio::test]
async fn ordinary_query_certification_is_independent_of_hidden_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let policy = POLICY.replace("inferred = \"hidden\"", "inferred = \"visible\"");
    let mut observations = Vec::new();
    let mut certificates = Vec::new();
    let mut bodies = Vec::new();
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
        // `send` intentionally discards headers. Use its existing token/Request/oneshot
        // construction inline here to retain every value from the actual HTTP owner.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dataset/query")
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
                    .body(Body::from(format!("ASK {{ ?x <{EX}p> ?y }}")))?,
            )
            .await?;
        let status = response.status();
        let headers: Vec<_> = response
            .headers()
            .keys()
            .map(|name| {
                (
                    name.as_str().to_owned(),
                    response
                        .headers()
                        .get_all(name)
                        .iter()
                        .map(|value| value.as_bytes().to_vec())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let certificate: Vec<_> = response
            .headers()
            .get_all("nrese-completeness")
            .iter()
            .map(|value| value.as_bytes().to_vec())
            .collect();
        let readable: Vec<_> = response
            .headers()
            .get_all("nrese-completeness")
            .iter()
            .map(|value| value.to_str().map(str::to_owned))
            .collect::<Result<_, _>>()?;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await?
            .to_vec();
        observations.push(serde_json::json!({
            "hidden": hidden, "status": status.as_u16(), "headers_bytes": headers,
            "nrese_completeness": readable, "body_bytes": body,
            "body_utf8": std::str::from_utf8(&body)?,
        }));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body)?["boolean"],
            false
        );
        bodies.push(body);
        certificates.push(certificate);
    }
    let evidence = serde_json::to_vec_pretty(&observations)?;
    println!("ORDINARY_PAIR={}", std::str::from_utf8(&evidence)?);
    if let Some(out) = std::env::var_os("FLEET_OUT") {
        std::fs::write(
            std::path::Path::new(&out).join("ordinary-pair.json"),
            evidence,
        )?;
    }
    // Establish identical actual answer bytes before comparing metadata.
    assert_eq!(
        bodies[0], bodies[1],
        "answer bodies differ; no metadata-only witness"
    );
    assert!(!certificates[0].is_empty() && !certificates[1].is_empty());
    assert_eq!(
        certificates[0], certificates[1],
        "hidden schema changed ordinary certification"
    );
    assert_eq!(certificates[0], vec![b"not-certified".to_vec()]);
    for world in &observations {
        assert!(
            world["headers_bytes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|header| {
                    header[0] == "access-control-expose-headers"
                        && header[1] == serde_json::json!([b"nrese-completeness".to_vec()])
                })
        );
    }
    Ok(())
}
