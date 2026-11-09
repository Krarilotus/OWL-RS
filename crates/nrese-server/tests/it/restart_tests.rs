//! A repository answers after a restart what it answered before, on the same configuration
//! and data (P2-B5 catalog slice, design note `2026-11-08-p2-b5-r1o1-design.md`: invariant 2,
//! restart equivalence). This build's server binary, started twice on one data directory
//! ([`super::upgrade_tests::Server`]), so the start path is the one an operator runs.

use reqwest::Method;

use super::upgrade_tests::{Server, configure};

type Outcome = Result<(), Box<dyn std::error::Error>>;

/// Bob is an employee; employees work for someone (an existential the OWL 2 QL rewriting
/// answers, and `owl2-rl`'s rules alone don't).
const EMPLOYEE: &str = "PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA {
        <urn:Employee> rdfs:subClassOf [ a owl:Restriction ;
            owl:onProperty <urn:worksFor> ; owl:someValuesFrom owl:Thing ] .
        <urn:bob> a <urn:Employee> .
    }";
const WORKS: &str = "SELECT ?x WHERE { ?x <urn:worksFor> ?y }";

/// Whether `rows` answer `urn:bob`.
fn bob(rows: &[String]) -> bool {
    rows.iter().any(|row| row.contains("urn:bob"))
}

/// The configuration of a server on `data` under `dir`, with `owl2-rl` reasoning and the QL
/// rewriting `ql` (the triage's input: off).
fn configured(dir: &std::path::Path, ql: &str) -> std::io::Result<std::path::PathBuf> {
    let data = dir.join("data");
    std::fs::create_dir_all(&data)?;
    let file = configure(dir, &data)?;
    let text = std::fs::read_to_string(&file)?.replace(
        "[reasoner]\nmode = \"owl2-rl\"\n",
        &format!("[reasoner]\nmode = \"owl2-rl\"\nql_rewriting = \"{ql}\"\n"),
    );
    std::fs::write(&file, text)?;
    Ok(file)
}

/// I-01 (triage 2026-11-09, N2), red first: the server's configuration turns the QL
/// rewriting off; the default repository's settings turn it on, and its existential query
/// answers bob. Specified (restart equivalence): after a restart on the same configuration
/// and data, the same query answers bob again and the settings still say on. A named
/// repository with the same setting is the control (it already holds).
#[tokio::test(flavor = "multi_thread")]
async fn a_repository_s_ql_setting_holds_after_a_restart() -> Outcome {
    let dir = tempfile::tempdir()?;
    let config = configured(dir.path(), "off")?;
    let on = r#"{"ql_rewriting": "on"}"#;
    let server = Server::start(&config).await?;
    server
        .send(Method::PATCH, "/api/v1/repositories/nrese", on)
        .await?;
    let named = r#"{"reasoning": "owl2-rl", "ql_rewriting": "on"}"#;
    server
        .send(Method::PUT, "/api/v1/repositories/r2", named)
        .await?;
    let mut wrong = Vec::new();
    for repo in ["nrese", "r2"] {
        server.update(repo, EMPLOYEE).await?;
        if !bob(&server.query(repo, WORKS).await?) {
            wrong.push(format!(
                "{repo} before the restart: no bob (the setup is wrong)"
            ));
        }
    }
    drop(server);
    let server = Server::start(&config).await?;
    for repo in ["nrese", "r2"] {
        if !bob(&server.query(repo, WORKS).await?) {
            wrong.push(format!("{repo} after the restart: no bob"));
        }
        let view: serde_json::Value =
            serde_json::from_str(&server.get(&format!("/api/v1/repositories/{repo}")).await?)?;
        if view["settings"]["ql_rewriting"] != "on" {
            wrong.push(format!(
                "{repo} after the restart: settings {}",
                view["settings"]
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// p is the inverse of q: `owl2-rl` derives `b q a`, `rdfs` doesn't.
const INVERSE: &str = "PREFIX owl: <http://www.w3.org/2002/07/owl#>
    INSERT DATA { <urn:p> owl:inverseOf <urn:q> . <urn:a> <urn:p> <urn:b> . }";
const INVERTED: &str = "SELECT ?s WHERE { ?s <urn:q> <urn:a> }";

/// Mechanism 5 of the design note, red first if it is reachable: the server's
/// configuration reasons with `owl2-rl`; a named repository without a reasoning of its own
/// takes the server's; the default repository's settings choose `rdfs`. Specified: the named
/// repository reasons with the server's `owl2-rl` before and after a restart (its answer
/// and its reported reasoning), whatever the default repository chose.
#[tokio::test(flavor = "multi_thread")]
async fn a_repository_without_its_own_reasoning_keeps_the_server_s_after_a_restart() -> Outcome {
    let dir = tempfile::tempdir()?;
    let config = configured(dir.path(), "auto")?;
    let server = Server::start(&config).await?;
    server
        .send(Method::PUT, "/api/v1/repositories/r2", r#"{"title": "r2"}"#)
        .await?;
    server
        .send(
            Method::PATCH,
            "/api/v1/repositories/nrese",
            r#"{"reasoning": "rdfs"}"#,
        )
        .await?;
    server.update("r2", INVERSE).await?;
    let derived = |rows: &[String]| rows.iter().any(|row| row.contains("urn:b"));
    let mut wrong = Vec::new();
    if !derived(&server.query("r2", INVERTED).await?) {
        wrong.push("before the restart: r2 doesn't derive b q a (the setup is wrong)".to_owned());
    }
    drop(server);
    let server = Server::start(&config).await?;
    if !derived(&server.query("r2", INVERTED).await?) {
        wrong.push("after the restart: r2 doesn't derive b q a".to_owned());
    }
    let view: serde_json::Value =
        serde_json::from_str(&server.get("/api/v1/repositories/r2").await?)?;
    if view["reasoning"] != "owl2-rl" {
        wrong.push(format!(
            "after the restart: r2 reasons with {}",
            view["reasoning"]
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}
