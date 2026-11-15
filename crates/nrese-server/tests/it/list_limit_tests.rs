//! A key longer than one rule holds (docs/spec/reasoning-semantics.md, "Lists"), through
//! the server: a write that holds one, `nrese-server load` of one, and a store that holds
//! one (loaded with reasoning off, as a store main wrote may) started with reasoning on.

use std::process::Command;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_settings};
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
