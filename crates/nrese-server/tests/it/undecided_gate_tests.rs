use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::config::ServerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::{StoreConfig, UndecidedPolicy};
use serde_json::{Value, json};
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::{body_text, test_app_with_store_config, test_state_with_store_config};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const DATA: &str = "/dataset/data?default";
const INFO: &str = "/dataset/info";
const READY: &str = "/readyz";
const IMPORT: &str = "/api/v1/repositories/nrese/import";
const REBUILD: &str = "/api/v1/repositories/nrese/reasoning/rematerialise";
const QUERY: &str = "/dataset/query";
const EXPLAIN: &str = "/dataset/query?explain=true";
const STUDENTS: &str = "SELECT ?s WHERE { ?s a <urn:ex:Student> }";
const UNRELATED: &str = "SELECT ?s WHERE { ?s <urn:ex:unrelated> ?o }";
const TURTLE: &str = "text/turtle";
const SPARQL: &str = "application/sparql-query";
const FIXTURE: &str = r#"
@prefix : <urn:ex:> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
:Student a owl:Class ;
    owl:equivalentClass [ owl:intersectionOf (
        :Person [ a owl:Restriction ; owl:onProperty :takesCourse ; owl:someValuesFrom :Course ]
    ) ] .
:Person a owl:Class .
:Course a owl:Class .
:takesCourse a owl:ObjectProperty .
:subOrganizationOf a owl:TransitiveProperty .
:alice a :Student .
:bob a :Person ; :takesCourse :c .
:c a :Course .
"#;
const CLASH: &str = r#"
@prefix : <urn:ex:> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
:A owl:disjointWith :B .
:x a :A, :B .
"#;

fn app(config: StoreConfig, mode: ReasoningMode) -> TestResult<axum::Router> {
    test_app_with_store_config(
        config,
        PolicyConfig::default(),
        ReasonerConfig::for_mode(mode),
    )
}

fn gate_config() -> TestResult<StoreConfig> {
    Ok(ServerConfig::from_env()?.store)
}

/// Run one test with the operator's environment setting, without changing process-wide
/// state shared by the other tests or referring to a store API that the baseline lacks.
fn run_with_refuse(test: &str) -> TestResult<bool> {
    if std::env::var("NRESE_GATES_UNDECIDED").as_deref() == Ok("refuse") {
        return Ok(false);
    }
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            &format!("undecided_gate_tests::{test}"),
            "--nocapture",
        ])
        .env_remove("NRESE_CONFIG_PATH")
        .env("NRESE_STORE_MODE", "in-memory")
        .env("NRESE_GATES_UNDECIDED", "refuse")
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(true)
}

struct Reply {
    status: StatusCode,
    body: String,
    completeness: Option<String>,
}

impl Reply {
    fn json(&self) -> TestResult<Value> {
        Ok(serde_json::from_str(&self.body)?)
    }
}

async fn send(
    app: &axum::Router,
    method: Method,
    path: &str,
    media: &str,
    body: &str,
    roles: &[&str],
) -> TestResult<Reply> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", media);
    if !roles.is_empty() {
        request = request.header("authorization", format!("Bearer {}", token(roles)));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned()))?)
        .await?;
    Ok(Reply {
        status: response.status(),
        completeness: response
            .headers()
            .get("nrese-completeness")
            .map(|header| header.to_str().map(str::to_owned))
            .transpose()?,
        body: body_text(response).await?,
    })
}

async fn post(app: &axum::Router, path: &str, media: &str, body: &str) -> TestResult<Reply> {
    send(app, Method::POST, path, media, body, &[]).await
}

async fn get(app: &axum::Router, path: &str) -> TestResult<Reply> {
    send(app, Method::GET, path, "", "", &[]).await
}

fn assert_refused(reply: &Reply) -> TestResult {
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let problem = reply.json()?;
    assert_eq!(
        problem["type"],
        "https://nrese.dev/problems/undecided-reject"
    );
    assert_eq!(
        problem["undecided"],
        json!({
            "gate": "consistency", "causes": ["no-decider"], "decided_by": "owl2-dl"
        })
    );
    Ok(())
}

fn assert_marked(info: &Value) {
    assert_eq!(info["consistency"], "undecided", "{info}");
    assert!(info["violations"].is_null(), "{info}");
    let report = &info["undecided"];
    assert_eq!(report["gate"], "consistency");
    assert_eq!(report["disposition"], "marked");
    assert_eq!(report["causes"], json!(["no-decider"]));
}

// Spec. Refuse returns the typed cause and leaves the revision and quad count unchanged.
#[tokio::test]
async fn refuses_an_undecided_write() -> TestResult {
    if run_with_refuse("refuses_an_undecided_write")? {
        return Ok(());
    }
    let app = app(gate_config()?, ReasoningMode::Owl2Ql)?;
    let before = get(&app, INFO).await?.json()?;
    assert_refused(&post(&app, DATA, TURTLE, FIXTURE).await?)?;
    let after = get(&app, INFO).await?.json()?;
    assert_eq!(before["revision"], after["revision"]);
    assert_eq!(before["quad_count"], after["quad_count"]);
    Ok(())
}

// Spec. The default accepts a clean QL write, reports the undecided cause, and stays ready.
#[tokio::test]
async fn marks_an_undecided_write_by_default() -> TestResult {
    let app = app(StoreConfig::in_memory(), ReasoningMode::Owl2Ql)?;
    assert_eq!(
        post(&app, DATA, TURTLE, FIXTURE).await?.status,
        StatusCode::NO_CONTENT
    );
    assert_marked(&get(&app, INFO).await?.json()?);
    assert_eq!(get(&app, READY).await?.status, StatusCode::OK);
    Ok(())
}

// Spec. Rebuild and reopen reproduce the QL report and read status.
#[tokio::test]
async fn rebuild_and_reopen_preserve_the_verdict() -> TestResult {
    let directory = tempfile::tempdir()?;
    let open = || {
        app(
            StoreConfig::on_disk(directory.path()),
            ReasoningMode::Owl2Ql,
        )
    };
    let first = open()?;
    assert_eq!(
        post(&first, DATA, TURTLE, FIXTURE).await?.status,
        StatusCode::NO_CONTENT
    );
    let before = get(&first, INFO).await?.json()?;
    assert_marked(&before);
    let header = post(&first, QUERY, SPARQL, STUDENTS).await?.completeness;
    assert!(header.as_deref().is_some_and(|header| {
        header.starts_with("sound-only;") && header.contains("consistency:")
    }));
    assert_eq!(post(&first, REBUILD, "", "").await?.status, StatusCode::OK);
    assert_eq!(
        get(&first, INFO).await?.json()?["undecided"],
        before["undecided"]
    );
    assert_eq!(
        post(&first, QUERY, SPARQL, STUDENTS).await?.completeness,
        header
    );
    drop(first);
    let reopened = open()?;
    assert_eq!(
        get(&reopened, INFO).await?.json()?["undecided"],
        before["undecided"]
    );
    assert_eq!(
        post(&reopened, QUERY, SPARQL, STUDENTS).await?.completeness,
        header
    );
    Ok(())
}

// Spec. Opening a clean store under refuse quarantines its published undecided verdict.
#[tokio::test]
async fn reopening_a_marked_store_under_refuse_quarantines_it() -> TestResult {
    let directory = tempfile::tempdir()?;
    let marked = app(
        StoreConfig::on_disk(directory.path()),
        ReasoningMode::Owl2Ql,
    )?;
    assert_eq!(
        post(&marked, DATA, TURTLE, FIXTURE).await?.status,
        StatusCode::NO_CONTENT
    );
    drop(marked);

    let reopened = app(
        StoreConfig {
            gates_undecided: UndecidedPolicy::Refuse,
            ..StoreConfig::on_disk(directory.path())
        },
        ReasoningMode::Owl2Ql,
    )?;
    let ready = get(&reopened, READY).await?;
    assert_eq!(
        ready.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        ready.body
    );
    assert_eq!(ready.json()?["status"], "quarantined");
    let info = get(&reopened, INFO).await?.json()?;
    assert_eq!(info["consistency"], "undecided", "{info}");
    assert_eq!(info["undecided"]["disposition"], "quarantined");
    assert_refused(&post(&reopened, DATA, TURTLE, "<urn:ex:d> a <urn:ex:Person> .").await?)
}

// Spec. A bulk load in mark mode records the same report as an ordinary write.
#[tokio::test]
async fn marks_an_import_by_default() -> TestResult {
    let app = app(StoreConfig::in_memory(), ReasoningMode::Owl2Ql)?;
    assert_eq!(
        post(&app, IMPORT, TURTLE, FIXTURE).await?.status,
        StatusCode::OK
    );
    assert_marked(&get(&app, INFO).await?.json()?);
    Ok(())
}

// Spec. Refuse mode quarantines a bulk load, then rejects the next commit.
#[tokio::test]
async fn quarantines_an_import_under_refuse() -> TestResult {
    if run_with_refuse("quarantines_an_import_under_refuse")? {
        return Ok(());
    }
    let app = app(gate_config()?, ReasoningMode::Owl2Ql)?;
    assert_eq!(
        post(&app, IMPORT, TURTLE, FIXTURE).await?.status,
        StatusCode::OK
    );
    let ready = get(&app, READY).await?;
    assert_eq!(
        ready.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        ready.body
    );
    assert_eq!(ready.json()?["status"], "quarantined");
    assert_refused(&post(&app, DATA, TURTLE, "<urn:ex:d> a <urn:ex:Person> .").await?)
}

// Spec. The consistency reason lowers unrelated reads and composes with QL and negation.
#[tokio::test]
async fn consistency_reason_composes_and_survives_the_cache() -> TestResult {
    let state = test_state_with_store_config(
        StoreConfig {
            query_cache_bytes: 1 << 20,
            ..StoreConfig::in_memory()
        },
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Ql),
    )?;
    let app = nrese_server::build_app(state.clone());
    assert_eq!(
        post(&app, DATA, TURTLE, FIXTURE).await?.status,
        StatusCode::NO_CONTENT
    );
    let unrelated = post(&app, QUERY, SPARQL, UNRELATED)
        .await?
        .completeness
        .unwrap();
    assert!(unrelated.starts_with("sound-only;"), "{unrelated}");
    assert!(
        unrelated.contains("consistency:") && !unrelated.contains("ql: answers"),
        "{unrelated}"
    );
    let query = post(&app, QUERY, SPARQL, STUDENTS).await?;
    let header = query.completeness.as_deref().unwrap();
    assert!(header.starts_with("sound-only;"), "{header}");
    assert!(
        header.contains("ql: answers") && header.contains("consistency:"),
        "{header}"
    );
    let before = state.store().query_cache_stats().hits;
    let again = post(&app, QUERY, SPARQL, STUDENTS).await?;
    assert_eq!(again.completeness, query.completeness);
    assert_eq!(again.body, query.body);
    assert!(state.store().query_cache_stats().hits > before);
    let negation =
        "SELECT ?s WHERE { ?s a <urn:ex:Person> FILTER NOT EXISTS { ?s a <urn:ex:Student> } }";
    let negative = post(&app, QUERY, SPARQL, negation)
        .await?
        .completeness
        .unwrap();
    assert!(
        negative.starts_with("unsound;") && negative.contains("consistency:"),
        "{negative}"
    );
    let explain = post(&app, EXPLAIN, SPARQL, STUDENTS).await?.json()?;
    for reason in ["ql", "consistency"] {
        assert!(
            explain["completeness"]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["source"] == reason),
            "{explain}"
        );
    }
    Ok(())
}

// Spec. A restricted reader always gets 403; the restricted writer gets a fixed refusal.
#[tokio::test]
async fn restricted_reader_cannot_see_the_report() -> TestResult {
    let app = restricted_app(StoreConfig::in_memory())?;
    let reply = send(&app, Method::GET, INFO, "", "", &["writer"]).await?;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    assert!(reply.json()?.get("undecided").is_none());
    Ok(())
}

// Spec. A restricted writer learns only the fixed no-decider cause.
#[tokio::test]
async fn restricted_writer_gets_the_fixed_cause() -> TestResult {
    if run_with_refuse("restricted_writer_gets_the_fixed_cause")? {
        return Ok(());
    }
    let app = restricted_app(gate_config()?)?;
    let reply = send(
        &app,
        Method::POST,
        "/dataset/data?graph=urn%3Aallowed",
        TURTLE,
        FIXTURE,
        &["writer"],
    )
    .await?;
    assert_refused(&reply)
}

// Spec. An ill-typed literal keeps consistency unknown and outside the undecided gate.
#[tokio::test]
async fn ill_typed_literal_keeps_consistency_unknown_under_refuse() -> TestResult {
    if run_with_refuse("ill_typed_literal_keeps_consistency_unknown_under_refuse")? {
        return Ok(());
    }
    let app = app(gate_config()?, ReasoningMode::Owl2Ql)?;
    let data = r#"<urn:s> <urn:p> "abc"^^<http://www.w3.org/2001/XMLSchema#integer> ."#;
    let write = post(&app, DATA, TURTLE, data).await?;
    assert_eq!(write.status, StatusCode::NO_CONTENT, "{}", write.body);
    let info = get(&app, INFO).await?.json()?;
    assert_eq!(info["consistency"], "unknown", "{info}");
    assert!(info.get("undecided").is_none());
    assert_eq!(get(&app, READY).await?.status, StatusCode::OK);
    Ok(())
}

fn restricted_app(config: StoreConfig) -> TestResult<axum::Router> {
    let policy = policy_of(
        r#"
default = "deny"
inferred = "hidden"
[[role]]
name = "writer"
read = ["urn:allowed"]
write = ["urn:allowed"]
"#,
    )?;
    test_app_with_store_config(
        config,
        policy,
        ReasonerConfig::for_mode(ReasoningMode::Owl2Ql),
    )
}

// Spec. A rules clash still wins; OWL 2 RL retains its verdict and read status.
#[tokio::test]
async fn rules_clashes_and_rl_verdicts_keep_their_behavior() -> TestResult {
    for config in [StoreConfig::in_memory(), gate_config()?] {
        let ql = app(config, ReasoningMode::Owl2Ql)?;
        let reply = post(&ql, DATA, TURTLE, CLASH).await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
        assert_eq!(
            reply.json()?["type"],
            "https://nrese.dev/problems/reasoner-reject"
        );
    }
    if run_with_refuse("rules_clashes_and_rl_verdicts_keep_their_behavior")? {
        return Ok(());
    }
    let rl = app(gate_config()?, ReasoningMode::Owl2Rl)?;
    let info = get(&rl, INFO).await?.json()?;
    assert_eq!(info["consistency"], "consistent", "{info}");
    assert!(info.get("undecided").is_none(), "{info}");
    let header = post(&rl, QUERY, SPARQL, UNRELATED)
        .await?
        .completeness
        .unwrap();
    assert!(
        header.contains("regime=owl2-rl") && !header.contains("consistency:"),
        "{header}"
    );
    Ok(())
}
