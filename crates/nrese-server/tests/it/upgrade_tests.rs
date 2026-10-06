//! The upgrade path (v2 merge checklist §4): a data directory as `main`'s server left it
//! (`tests/fixtures/main-store`, written by its `make.sh` with `main` at d9a4b24), opened
//! by this build's server binary as an operator would: the same configuration, the same
//! command.
//!
//! The fixture holds a checkpoint and a WAL tail no checkpoint covers (the server was
//! stopped hard), the reasoning markers, the default repository's settings and
//! namespaces, seven repositories (rdfs, owl2-rl with N3 rules, owl2-ql, owl-horst, and
//! ids main accepted that new repositories can't take), the access state (settings, a
//! role, users with a local login, a workspace, saved queries) and an image backup.
//! Sessions and the result cache live in memory and leave nothing behind.
//!
//! Checked: the same data and answers as main gave (`expected/`, less the differences v2
//! makes on purpose, `v2-differences.tsv`), every setting main reported still reported
//! the same, each inferred stack current under v2's rules (kept where they didn't change,
//! rebuilt where they did), writes after the upgrade surviving a restart, and main's image
//! backup restoring.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nrese_reasoner::rulesets::Ruleset;

const TOKEN: &str = "fixture-admin";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/main-store")
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The configuration `make.sh` gave main, on a free port, over `data`.
fn configure(dir: &Path, data: &Path) -> std::io::Result<(PathBuf, String)> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let data = data.display().to_string().replace('\\', "/");
    let file = dir.join(format!("config-{port}.toml"));
    std::fs::write(
        &file,
        format!(
            "[server]\nbind_addr = \"127.0.0.1:{port}\"\ndeployment_posture = \"internal-authenticated\"\n\
             [store]\nmode = \"on-disk\"\ndata_dir = \"{data}\"\nwal_archive = true\n\
             [reasoner]\nmode = \"owl2-rl\"\n\
             [auth]\nmode = \"bearer-static\"\n\
             [auth.bearer_static]\nadmin_token = \"{TOKEN}\"\nread_token = \"fixture-read\"\n"
        ),
    )?;
    Ok((file, format!("http://127.0.0.1:{port}")))
}

/// This build's server, stopped (by its own handle) when dropped.
struct Server {
    child: Child,
    url: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(config: &Path, url: String) -> Result<Self, Box<dyn std::error::Error>> {
        let child = Command::new(env!("CARGO_BIN_EXE_nrese-server"))
            .arg("--config")
            .arg(config)
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let server = Self {
            child,
            url,
            http: reqwest::Client::new(),
        };
        let started = Instant::now();
        loop {
            if let Ok(response) = server
                .http
                .get(format!("{}/readyz", server.url))
                .send()
                .await
                && response.status().is_success()
            {
                return Ok(server);
            }
            if started.elapsed() > Duration::from_secs(120) {
                return Err("the server didn't become ready in 120 s".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn get(&self, path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let response = self
            .http
            .get(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(format!("GET {path}: {status} {text}").into());
        }
        Ok(text)
    }

    /// A query's TSV results: the header, then the rows sorted.
    async fn query(
        &self,
        repo: &str,
        query: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let response = self
            .http
            .post(format!("{}/api/v1/repositories/{repo}/query", self.url))
            .bearer_auth(TOKEN)
            .header("Accept", "text/tab-separated-values")
            .form(&[("query", query)])
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(format!("{repo}: {query}: {status} {text}").into());
        }
        Ok(sorted(&text))
    }

    async fn update(&self, repo: &str, update: &str) -> Result<(), Box<dyn std::error::Error>> {
        let response = self
            .http
            .post(format!("{}/api/v1/repositories/{repo}/update", self.url))
            .bearer_auth(TOKEN)
            .header("Content-Type", "application/sparql-update")
            .body(update.to_owned())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(format!("{repo}: {update}: {}", response.status()).into());
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn sorted(tsv: &str) -> Vec<String> {
    let mut lines = tsv.lines().map(str::to_owned);
    let header = lines.next().unwrap_or_default();
    let mut rows: Vec<String> = lines.filter(|line| !line.is_empty()).collect();
    rows.sort();
    std::iter::once(header).chain(rows).collect()
}

/// `queries.tsv`: repository, name, query.
fn queries() -> Vec<(String, String, String)> {
    std::fs::read_to_string(fixture().join("queries.tsv"))
        .expect("queries.tsv")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.splitn(3, '\t');
            let mut next = || parts.next().expect("three columns").to_owned();
            (next(), next(), next())
        })
        .collect()
}

/// Main's answer to a query, with the rows v2 adds or drops on purpose applied.
fn expected(repo: &str, name: &str) -> Vec<String> {
    let path = fixture().join(format!("expected/{repo}-{name}.tsv"));
    let mut rows = sorted(&std::fs::read_to_string(&path).expect("main's answer"));
    let differences = std::fs::read_to_string(fixture().join("v2-differences.tsv")).unwrap();
    for line in differences
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts[0] != repo || parts[1] != name {
            continue;
        }
        match parts[2].split_at(1) {
            ("+", row) => rows.push(row.to_owned()),
            ("-", row) => rows.retain(|r| r != row),
            _ => panic!("v2-differences.tsv: {line}"),
        }
    }
    let header = rows.remove(0);
    rows.sort();
    std::iter::once(header).chain(rows).collect()
}

/// Whether everything `main` says is in `v2` the same (v2 may say more).
fn contains(main: &serde_json::Value, v2: &serde_json::Value, at: &str) -> Vec<String> {
    use serde_json::Value;
    match (main, v2) {
        (Value::Object(a), Value::Object(b)) => a
            .iter()
            .flat_map(|(key, value)| match b.get(key) {
                Some(other) => contains(value, other, &format!("{at}.{key}")),
                None => vec![format!("{at}.{key} missing")],
            })
            .collect(),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .flat_map(|(i, (x, y))| contains(x, y, &format!("{at}[{i}]")))
            .collect(),
        _ if main == v2 => Vec::new(),
        _ => vec![format!("{at}: {main} became {v2}")],
    }
}

/// `ruleset` and `fingerprint` of a store's reasoning marker.
fn marker(store: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(store.join("reasoning.state"))
        .expect("a reasoning marker")
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// Main's answers (less `skip`), in repository `only` or in all of them.
async fn same_answers(
    server: &Server,
    only: Option<&str>,
    skip: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    for (repo, name, query) in queries() {
        if only.is_some_and(|only| only != repo) || skip.contains(&name.as_str()) {
            continue;
        }
        assert_eq!(
            server.query(&repo, &query).await?,
            expected(&repo, &name),
            "{repo} {name}: {query}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_written_by_main_opens_with_its_data_answers_and_settings()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let data = dir.path().join("data");
    copy_dir(&fixture().join("data"), &data)?;
    let main_markers: BTreeMap<String, BTreeMap<String, String>> =
        std::fs::read_dir(data.join("repositories"))?
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    marker(&entry.path()),
                )
            })
            .collect();
    let (config, url) = configure(dir.path(), &data)?;
    let server = Server::start(&config, url).await?;

    // The data, with the WAL tail main never checkpointed.
    let ready: serde_json::Value = serde_json::from_str(&server.get("/readyz").await?)?;
    let main_ready: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        fixture().join("expected/readyz.json"),
    )?)?;
    for key in [
        "revision",
        "quad_count",
        "named_graph_count",
        "reasoning_mode",
    ] {
        assert_eq!(ready[key], main_ready[key], "{key}");
    }
    // Every answer, in every repository (those with ids new ones can't take included).
    same_answers(&server, None, &[]).await?;
    // Every setting main reported: repositories, their reasoning and rules, namespaces,
    // the access state, saved queries.
    for entry in std::fs::read_dir(fixture().join("expected"))? {
        let file = entry?.file_name().to_string_lossy().into_owned();
        let Some(stem) = file.strip_suffix(".json").filter(|s| *s != "readyz") else {
            continue;
        };
        let path = format!("/api/v1/{}", stem.replacen("__", "/~", 1).replace('_', "/"));
        let main: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            fixture().join("expected").join(&file),
        )?)?;
        let v2: serde_json::Value = serde_json::from_str(&server.get(&path).await?)?;
        let lost = contains(&main, &v2, &path);
        assert!(lost.is_empty(), "{lost:#?}");
    }
    // A local login main created still logs in.
    let login = server
        .http
        .post(format!("{}/api/v1/access/login", server.url))
        .json(&serde_json::json!({"user": "alice", "password": "correct horse battery"}))
        .send()
        .await?;
    assert!(login.status().is_success(), "{}", login.status());

    // Each inferred stack is current under this build's rules: kept where the rules are
    // unchanged (owl2-rl, rdfs, the N3 rules), rebuilt where they changed (owl2-ql,
    // owl-horst).
    for (id, before) in &main_markers {
        let after = marker(&data.join("repositories").join(id));
        assert_eq!(after["ruleset"], before["ruleset"], "{id}");
        match Ruleset::from_name(&after["ruleset"]) {
            Some(ruleset) => assert_eq!(
                after["fingerprint"],
                format!("{:016x}", ruleset.fingerprint()),
                "{id}: not current"
            ),
            None => assert_eq!(after["fingerprint"], before["fingerprint"], "{id}"),
        }
    }
    let default = marker(&data);
    assert_eq!(
        default["fingerprint"],
        format!("{:016x}", Ruleset::Owl2Rl.fingerprint())
    );

    // Writes after the upgrade go on main's log and survive a restart.
    server
        .update(
            "nrese",
            "INSERT DATA { <http://e/after> <http://e/upgrade> \"v2\" }",
        )
        .await?;
    drop(server);
    let (config, url) = configure(dir.path(), &data)?;
    let server = Server::start(&config, url).await?;
    assert_eq!(
        server
            .query(
                "nrese",
                "SELECT ?v WHERE { <http://e/after> <http://e/upgrade> ?v }"
            )
            .await?,
        ["?v", "\"v2\""]
    );
    let ready: serde_json::Value = serde_json::from_str(&server.get("/readyz").await?)?;
    assert_eq!(
        ready["quad_count"],
        main_ready["quad_count"].as_u64().unwrap() + 1
    );
    same_answers(&server, Some("rdfs"), &[]).await?;
    Ok(())
}

/// Main's image backup restores with this build (`nrese-server restore`) and serves what
/// main served.
#[tokio::test(flavor = "multi_thread")]
async fn an_image_backup_written_by_main_restores() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let backup = dir.path().join("backup");
    copy_dir(&fixture().join("backup"), &backup)?;
    let data = dir.path().join("data");
    let (config, url) = configure(dir.path(), &data)?;
    let restored = Command::new(env!("CARGO_BIN_EXE_nrese-server"))
        .arg("restore")
        .arg(&backup)
        .arg("--config")
        .arg(&config)
        .env("RUST_LOG", "warn")
        .output()?;
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let server = Server::start(&config, url).await?;
    // An image holds the store, not its namespaces (`ex:` of the `prefixed` query), in
    // main as here.
    same_answers(&server, Some("nrese"), &["prefixed"]).await
}
