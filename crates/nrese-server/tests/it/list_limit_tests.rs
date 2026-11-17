//! A key longer than one rule holds (docs/spec/reasoning-semantics.md, "Lists"), through
//! the server: a write that holds one, `nrese-server load` of one, and a store that holds
//! one (loaded with reasoning off, as a store main wrote may) started with reasoning on.

use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use nrese_store::{MutationPipeline, ReadModel, SparqlQueryRequest, StoreConfig, StoreService};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_settings, test_app_with_store};
use crate::upgrade_tests::{Server, configure};

const EX: &str = "http://example.com/";

/// A key of 128 properties on `:K`, one more than its rule can hold, and two `:K` it
/// would equate (Turtle statements, full IRIs).
fn long_key() -> String {
    let keys: Vec<String> = (1..=128).map(|i| format!("<{EX}k{i}>")).collect();
    let mut data = format!(
        "<{EX}K> <http://www.w3.org/2002/07/owl#hasKey> ( {} ) .\n",
        keys.join(" ")
    );
    for x in ["x", "y"] {
        data.push_str(&format!("<{EX}{x}> a <{EX}K> .\n"));
        for i in 1..=128 {
            data.push_str(&format!("<{EX}{x}> <{EX}k{i}> <{EX}v{i}> .\n"));
        }
    }
    data
}

fn update(text: String) -> Result<Request<Body>, axum::http::Error> {
    Request::builder()
        .uri("/dataset/update")
        .method(Method::POST)
        .header("content-type", "application/sparql-update")
        .body(Body::from(text))
}

async fn ask(app: &axum::Router, pattern: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let encoded = serde_urlencoded::to_string([("query", format!("ASK {{ {pattern} }}"))])?;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/dataset/query?{encoded}&infer=false"))
                .header("accept", "application/sparql-results+json")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(body_text(response).await?.contains("true"))
}

/// A write that holds such a key is answered, whatever it is answered with: refused (a
/// 500) with nothing of it committed, or accepted with it committed; either way the next
/// write is accepted. Which one ran is printed: the build before the bound refuses it.
#[tokio::test]
async fn a_write_with_a_key_past_the_rule_format_leaves_the_server_serving()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_settings(
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    let response = app
        .clone()
        .oneshot(update(format!("INSERT DATA {{ {} }}", long_key()))?)
        .await?;
    let status = response.status();
    let committed = ask(&app, &format!("<{EX}x> <{EX}k128> <{EX}v128>")).await?;
    match status {
        StatusCode::INTERNAL_SERVER_ERROR => {
            println!("the write was refused: 500");
            assert!(!committed, "a refused write left its statements behind");
        }
        StatusCode::NO_CONTENT => {
            println!("the write was accepted: 204");
            assert!(committed, "an accepted write's statements are missing");
        }
        other => panic!("the write was answered {other}"),
    }
    let next = app
        .clone()
        .oneshot(update(format!(
            "INSERT DATA {{ <{EX}a> <{EX}b> <{EX}c> }}"
        ))?)
        .await?;
    assert_eq!(next.status(), StatusCode::NO_CONTENT, "the next write");
    assert!(ask(&app, &format!("<{EX}a> <{EX}b> <{EX}c>")).await?);
    Ok(())
}

/// `nrese-server load` with `config`, `extra` arguments and `file`: its output, after it
/// ended.
fn load(
    config: &std::path::Path,
    extra: &[&str],
    file: &std::path::Path,
) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_nrese-server"))
        .arg("load")
        .arg("--config")
        .arg(config)
        .args(extra)
        .arg(file)
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
}

/// `load` under `owl2-rl` rebuilds the inferences over the loaded data: the key is
/// reported, and the load succeeds (the data is consistent).
#[test]
fn load_takes_a_key_past_the_rule_format() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let config = configure(dir.path(), &dir.path().join("data"))?;
    let file = dir.path().join("key.ttl");
    std::fs::write(&file, long_key())?;
    let output = load(&config, &[], &file)?;
    assert!(
        output.status.success(),
        "{}:\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// The case a store main wrote brings: the key loaded with reasoning off, then the server
/// started with reasoning on rebuilds the inferences at its start, and comes up.
#[tokio::test]
async fn a_store_holding_such_a_key_starts_with_reasoning_on()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let config = configure(dir.path(), &dir.path().join("data"))?;
    let file = dir.path().join("key.ttl");
    std::fs::write(&file, long_key())?;
    let output = load(&config, &["--set", "reasoner.mode=disabled"], &file)?;
    assert!(
        output.status.success(),
        "load without reasoning: {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // `configure` runs `owl2-rl`; `start` waits for /readyz to answer 200.
    let server = Server::start(&config).await?;
    drop(server);
    Ok(())
}

const EMPTY_LIST_PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

fn assert_empty_list_input(
    store: &StoreService,
    data: &str,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let asserted = |query: &str| {
        let mut request = SparqlQueryRequest::all(format!("{EMPTY_LIST_PREFIXES}{query}"));
        request.read_model = Some(ReadModel::Asserted);
        store.execute_query(&request)
    };
    let rows = asserted("SELECT ?o { { :C owl:intersectionOf ?o } UNION { :l owl:sameAs ?o } }")?;
    let response: serde_json::Value = serde_json::from_slice(&rows.payload)?;
    assert_eq!(
        response["results"]["bindings"]
            .as_array()
            .expect("rows")
            .len(),
        count,
        "asserted list statements"
    );
    let held = asserted(&format!("ASK {{ {data} }}"))?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&held.payload)?["boolean"],
        true
    );
    Ok(())
}

/// Spec: the partial rules of OWL 2 Profiles §4.3 tables 6 and 9 derive no
/// `cls-int1` membership at zero members, where its subject has no binding.
/// OWL 2 Structural Specification §8.1.1 requires at least two operands.
/// A server rebuilding the closure starts and serves those answers.
#[tokio::test]
async fn empty_intersection_allows_the_server_to_start() -> Result<(), Box<dyn std::error::Error>> {
    const DATA: &str = ":C owl:intersectionOf rdf:nil . :x a :D .";
    let dir = tempfile::tempdir()?;
    let data_dir = dir.path().join("data");
    let config = configure(dir.path(), &data_dir)?;
    let file = dir.path().join("intersection.ttl");
    std::fs::write(&file, format!("{EMPTY_LIST_PREFIXES}{DATA}"))?;
    let output = load(&config, &["--set", "reasoner.mode=disabled"], &file)?;
    assert!(
        output.status.success(),
        "unreasoned load: {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let store = StoreService::new(StoreConfig::on_disk(&data_dir))?;
    assert_empty_list_input(&store, DATA, 1)?;
    drop(store);

    let server = Server::start(&config).await?;
    assert_eq!(
        server
            .query("nrese", &format!("{EMPTY_LIST_PREFIXES}ASK {{ :x a :C }}"))
            .await?,
        ["false"]
    );
    let types = server
        .query(
            "nrese",
            &format!("{EMPTY_LIST_PREFIXES}SELECT ?c {{ :x a ?c }}"),
        )
        .await?;
    assert!(
        types.iter().any(|row| row == "<http://example.com/D>"),
        "{types:?}"
    );
    assert!(
        types.iter().all(|row| row != "<http://example.com/C>"),
        "{types:?}"
    );
    Ok(())
}

/// Spec: OWL 2 Profiles §4.3 tables 6 and 9 supply no subject binding for
/// `cls-int1` on an empty list, including one reached by equality. These partial
/// rules derive no membership, and OWL 2 Structural Specification §8.1.1
/// requires at least two operands. The admin rebuild succeeds with those answers.
#[tokio::test]
async fn empty_intersection_allows_admin_rematerialisation()
-> Result<(), Box<dyn std::error::Error>> {
    const DATA: &str = ":C owl:intersectionOf :l . :l owl:sameAs rdf:nil . :x a :D .";
    let store = StoreService::new(StoreConfig::in_memory())?;
    let writer = MutationPipeline::new(
        Arc::new(store.clone()),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    );
    writer.execute_update_str(&format!("{EMPTY_LIST_PREFIXES}INSERT DATA {{ {DATA} }}"))?;
    assert_empty_list_input(&store, DATA, 2)?;
    let app = test_app_with_store(
        store.clone(),
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/repositories/nrese/reasoning/rematerialise")
                .method(Method::POST)
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let body = body_text(response).await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let answer = |query| store.execute_query_str(&format!("{EMPTY_LIST_PREFIXES}{query}"));
    let ask: serde_json::Value = serde_json::from_slice(&answer("ASK { :x a :C }")?.payload)?;
    assert_eq!(ask["boolean"], false);
    let select: serde_json::Value =
        serde_json::from_slice(&answer("SELECT ?c { :x a ?c }")?.payload)?;
    let rows = select["results"]["bindings"].as_array().expect("types");
    assert!(
        rows.iter()
            .any(|row| row["c"]["value"] == "http://example.com/D"),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .all(|row| row["c"]["value"] != "http://example.com/C"),
        "{rows:?}"
    );
    Ok(())
}

/// `:C owl:intersectionOf ( :A1 … :An )` and an `:x` in every `:Ai` (Turtle, full IRIs).
fn long_intersection(n: usize) -> String {
    let members: Vec<String> = (1..=n).map(|i| format!("<{EX}A{i}>")).collect();
    let mut data = format!(
        "<{EX}C> <http://www.w3.org/2002/07/owl#intersectionOf> ( {} ) .\n",
        members.join(" ")
    );
    for member in &members {
        data.push_str(&format!("<{EX}x> a {member} .\n"));
    }
    data
}

/// An intersection longer than one rule may hold (one atom per member, numbered by a
/// byte), through `load`: the load ends as a load of consistent data does, under
/// `owl2-rl` and `owl2-dl`, never with a panic.
#[test]
fn load_takes_a_long_intersection() -> Result<(), Box<dyn std::error::Error>> {
    let mut wrong = Vec::new();
    for mode in ["owl2-rl", "owl2-dl"] {
        for n in [256, 257] {
            let dir = tempfile::tempdir()?;
            let config = configure(dir.path(), &dir.path().join("data"))?;
            let file = dir.path().join("intersection.ttl");
            std::fs::write(&file, long_intersection(n))?;
            let output = load(&config, &["--set", &format!("reasoner.mode={mode}")], &file)?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let last: Vec<&str> = stderr.lines().rev().take(4).collect();
                wrong.push(format!("{mode}, {n} members: {}: {last:?}", output.status));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// A panic in the rebuild of the inferences (forced: `NRESE_TEST_PANIC_IN_REBUILD`, a
/// debug build's), after `load` and at the start: the process ends with an error that
/// names the store and how to start without reasoning. Only where the server is built with
/// debug assertions: a release build compiles the forced panic out, so its rebuild succeeds.
#[cfg(debug_assertions)]
#[test]
fn a_panic_in_the_rebuild_ends_in_a_message_naming_the_store()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let data = dir.path().join("data");
    let config = configure(dir.path(), &data)?;
    let file = dir.path().join("one.ttl");
    std::fs::write(&file, format!("<{EX}a> <{EX}b> <{EX}c> .\n"))?;
    let base = |command: &mut Command| {
        command
            .arg("--config")
            .arg(&config)
            .env("NRESE_TEST_PANIC_IN_REBUILD", "1")
            .env("NO_COLOR", "1")
            .env_remove("RUST_LOG");
    };
    let mut load = Command::new(env!("CARGO_BIN_EXE_nrese-server"));
    base(load.arg("load"));
    let loaded = load.arg(&file).output()?;
    let mut start = Command::new(env!("CARGO_BIN_EXE_nrese-server"));
    base(&mut start);
    let mut child = start
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() > deadline {
            child.kill()?;
            panic!("the server still runs after a panic in its start's rebuild");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let started = child.wait_with_output()?;
    for (place, output) in [("load", loaded), ("start", started)] {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success()
                && stderr.contains(&data.display().to_string().replace('\\', "/"))
                && stderr.contains("reasoner.mode = \"disabled\""),
            "{place}: {}:\n{stderr}",
            output.status
        );
    }
    Ok(())
}
