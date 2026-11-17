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
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{RuleProgram, UserRules};

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

/// The longest a server may take to open main's store and say where it listens.
const START: Duration = Duration::from_secs(120);
/// The longest one request to the server may take.
const REQUEST: Duration = Duration::from_secs(120);
/// The longest `nrese-server restore` may take.
const RESTORE: Duration = Duration::from_secs(300);
/// The longest `nrese-server query` may take, its inferences brought up to date first.
const QUERY: Duration = Duration::from_secs(300);

/// The configuration `make.sh` gave main, over `data`, on port 0: the server binds a free
/// port and says which.
pub(super) fn configure(dir: &Path, data: &Path) -> std::io::Result<PathBuf> {
    let data = data.display().to_string().replace('\\', "/");
    let file = dir.join("config.toml");
    std::fs::write(
        &file,
        format!(
            "[server]\nbind_addr = \"127.0.0.1:0\"\ndeployment_posture = \"internal-authenticated\"\n\
             [store]\nmode = \"on-disk\"\ndata_dir = \"{data}\"\nwal_archive = true\n\
             [reasoner]\nmode = \"owl2-rl\"\n\
             [auth]\nmode = \"bearer-static\"\n\
             [auth.bearer_static]\nadmin_token = \"{TOKEN}\"\nread_token = \"fixture-read\"\n"
        ),
    )?;
    Ok(file)
}

/// A process of this build's binary, killed and reaped when dropped. What it writes is
/// kept for the failure messages, and its lines are passed on as they come.
struct Process {
    child: Child,
    output: Arc<Mutex<String>>,
    /// Closed once both its standard output and its standard error are.
    lines: mpsc::Receiver<String>,
}

impl Process {
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        let child = command
            // Plain log lines, without terminal colours.
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (sender, lines) = mpsc::channel();
        // Owned from here on: whatever fails below kills and reaps the child (Drop).
        let mut process = Self {
            child,
            output: Arc::default(),
            lines,
        };
        let stdout = process.child.stdout.take().expect("piped");
        let stderr = process.child.stderr.take().expect("piped");
        for stream in [
            Box::new(stdout) as Box<dyn std::io::Read + Send>,
            Box::new(stderr),
        ] {
            let output = Arc::clone(&process.output);
            let sender = sender.clone();
            std::thread::Builder::new().spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    let mut kept = output.lock().unwrap_or_else(|p| p.into_inner());
                    kept.push_str(&line);
                    kept.push('\n');
                    drop(kept);
                    let _ = sender.send(line);
                }
            })?;
        }
        Ok(process)
    }

    fn output(&self) -> String {
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The first line `pick` takes, within `limit`; an error, with the output, if the
    /// process ends first or nothing comes in time.
    fn first_line<T>(
        &mut self,
        limit: Duration,
        pick: impl Fn(&str) -> Option<T>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let until = Instant::now() + limit;
        loop {
            match self
                .lines
                .recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                Ok(line) => {
                    if let Some(found) = pick(&line) {
                        return Ok(found);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Both streams closed: the process has ended.
                    let status = self.child.wait()?;
                    return Err(format!("ended ({status}):\n{}", self.output()).into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("nothing in {limit:?}:\n{}", self.output()).into());
                }
            }
        }
    }

    /// Waits, within `limit`, for the process to end: its status and output.
    fn finish(
        mut self,
        limit: Duration,
    ) -> Result<(std::process::ExitStatus, String), Box<dyn std::error::Error>> {
        let until = Instant::now() + limit;
        loop {
            match self
                .lines
                .recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                Ok(_) => {}
                // Both streams closed: the process has ended.
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Ok((self.child.wait()?, self.output()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("still running after {limit:?}:\n{}", self.output()).into());
                }
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// This build's server, stopped (by its own handle) when dropped. Also the restart tests'
/// ([`super::restart_tests`]).
pub(super) struct Server {
    process: Process,
    url: String,
    http: reqwest::Client,
}

impl Server {
    /// Starts the server with `config` and waits for the address it reports once it
    /// serves, then for it to answer ready there.
    pub(super) async fn start(config: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_with(config, &[]).await
    }

    /// [`Self::start`], with `env` set for the server.
    pub(super) async fn start_with(
        config: &Path,
        env: &[(&str, &str)],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut process = Process::spawn(
            Command::new(env!("CARGO_BIN_EXE_nrese-server"))
                .arg("--config")
                .arg(config)
                .env("RUST_LOG", "warn,nrese_server=info")
                .envs(env.iter().copied()),
        )?;
        let address = process
            .first_line(START, |line| {
                line.contains("nrese-server bootstrap complete")
                    .then(|| {
                        line.split_whitespace()
                            .find_map(|field| field.strip_prefix("bind_address="))
                            .and_then(|address| address.parse::<std::net::SocketAddr>().ok())
                    })
                    .flatten()
            })
            .map_err(|error| format!("the server reported no address: {error}"))?;
        if address.port() == 0 {
            return Err(format!(
                "the server reported its configured address ({address}), not the one it bound:\n{}",
                process.output()
            )
            .into());
        }
        let server = Self {
            process,
            url: format!("http://{address}"),
            http: reqwest::Client::builder().timeout(REQUEST).build()?,
        };
        let ready = server
            .http
            .get(format!("{}/readyz", server.url))
            .send()
            .await
            .map_err(|error| server.failed(&format!("/readyz: {error}")))?;
        if !ready.status().is_success() {
            return Err(server.failed(&format!("/readyz: {}", ready.status())));
        }
        Ok(server)
    }

    /// Sends the server SIGTERM, as `docker stop` does, and waits within `limit` for it to
    /// end: its status and what it wrote.
    #[cfg(unix)]
    pub(super) fn terminate(
        self,
        limit: Duration,
    ) -> Result<(std::process::ExitStatus, String), Box<dyn std::error::Error>> {
        let pid = self.process.child.id().to_string();
        let sent = Command::new("kill").args(["-TERM", &pid]).status()?;
        if !sent.success() {
            return Err(self.failed(&format!("kill -TERM {pid}: {sent}")));
        }
        self.process.finish(limit)
    }

    /// `what` failed, with what the server wrote.
    fn failed(&self, what: &str) -> Box<dyn std::error::Error> {
        format!(
            "{what}\n--- the server's output:\n{}",
            self.process.output()
        )
        .into()
    }

    pub(super) async fn get(&self, path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let response = self
            .http
            .get(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .send()
            .await
            .map_err(|error| self.failed(&format!("GET {path}: {error}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| self.failed(&format!("GET {path}: {error}")))?;
        if !status.is_success() {
            return Err(self.failed(&format!("GET {path}: {status} {text}")));
        }
        Ok(text)
    }

    /// A query's TSV results: the header, then the rows sorted.
    pub(super) async fn query(
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
            .await
            .map_err(|error| self.failed(&format!("{repo}: {query}: {error}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| self.failed(&format!("{repo}: {query}: {error}")))?;
        if !status.is_success() {
            return Err(self.failed(&format!("{repo}: {query}: {status} {text}")));
        }
        Ok(sorted(&text))
    }

    /// [`Self::query`]'s TSV results, and the status header its answer carries
    /// (`nrese-completeness`; `None` without one).
    pub(super) async fn query_status(
        &self,
        repo: &str,
        query: &str,
    ) -> Result<(Vec<String>, Option<String>), Box<dyn std::error::Error>> {
        let response = self
            .http
            .post(format!("{}/api/v1/repositories/{repo}/query", self.url))
            .bearer_auth(TOKEN)
            .header("Accept", "text/tab-separated-values")
            .form(&[("query", query)])
            .send()
            .await
            .map_err(|error| self.failed(&format!("{repo}: {query}: {error}")))?;
        let status = response.status();
        let header = response
            .headers()
            .get("nrese-completeness")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let text = response
            .text()
            .await
            .map_err(|error| self.failed(&format!("{repo}: {query}: {error}")))?;
        if !status.is_success() {
            return Err(self.failed(&format!("{repo}: {query}: {status} {text}")));
        }
        Ok((sorted(&text), header))
    }

    /// `PUT`s or `PATCH`es (`method`) the JSON `body` to `path`: fails unless it succeeds.
    pub(super) async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let response = self
            .http
            .request(method.clone(), format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .header("Content-Type", "application/json")
            .body(body.to_owned())
            .send()
            .await
            .map_err(|error| self.failed(&format!("{method} {path}: {error}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| self.failed(&format!("{method} {path}: {error}")))?;
        if !status.is_success() {
            return Err(self.failed(&format!("{method} {path}: {status} {text}")));
        }
        Ok(text)
    }

    pub(super) async fn update(
        &self,
        repo: &str,
        update: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = self
            .http
            .post(format!("{}/api/v1/repositories/{repo}/update", self.url))
            .bearer_auth(TOKEN)
            .header("Content-Type", "application/sparql-update")
            .body(update.to_owned())
            .send()
            .await
            .map_err(|error| self.failed(&format!("{repo}: {update}: {error}")))?;
        if !response.status().is_success() {
            return Err(self.failed(&format!("{repo}: {update}: {}", response.status())));
        }
        Ok(())
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
    let config = configure(dir.path(), &data)?;
    let server = Server::start(&config).await?;

    // The data, with the WAL tail main never checkpointed.
    let ready: serde_json::Value = serde_json::from_str(&server.get("/dataset/info").await?)?;
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

    // Each inferred stack is current under this build's rules: kept where neither the rules
    // nor the base semantics version changed since main, rebuilt where one did (this build's
    // version 3 isn't main's 2, so here every one is).
    for (id, before) in &main_markers {
        let after = marker(&data.join("repositories").join(id));
        assert_eq!(after["ruleset"], before["ruleset"], "{id}");
        let program = match Ruleset::from_name(&after["ruleset"]) {
            Some(ruleset) => RuleProgram::builtin(ruleset),
            // User rules: the program this build makes of the repository's rules.
            None => {
                let repository: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(
                        fixture().join(format!("expected/repositories_{id}.json")),
                    )?)?;
                let rules = &repository["settings"]["rules"];
                assert_eq!(rules["format"], "n3", "{id}");
                let text = |key: &str| rules[key].as_str().expect("rules text").to_owned();
                let user = UserRules::n3(text("name"), text("text"))?;
                let base = Ruleset::from_name(repository["reasoning"].as_str().expect("mode"));
                RuleProgram::new(base, Some(std::sync::Arc::new(user))).expect("a program")
            }
        };
        assert_eq!(
            after["fingerprint"],
            format!("{:016x}", program.fingerprint()),
            "{id}: not current"
        );
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
    let server = Server::start(&config).await?;
    assert_eq!(
        server
            .query(
                "nrese",
                "SELECT ?v WHERE { <http://e/after> <http://e/upgrade> ?v }"
            )
            .await?,
        ["?v", "\"v2\""]
    );
    let ready: serde_json::Value = serde_json::from_str(&server.get("/dataset/info").await?)?;
    assert_eq!(
        ready["quad_count"],
        main_ready["quad_count"].as_u64().unwrap() + 1
    );
    same_answers(&server, Some("rdfs"), &[]).await?;
    Ok(())
}

/// A store whose closure the plain `owl2-rl` program wrote, as `owl2-dl` stores were written
/// before their keys bound named individuals only, opened by this build's server twice. As
/// `owl2-rl` it is current at both starts; as `owl2-dl` its closure is rebuilt once, at the
/// first start, under this build's `owl2-dl` program, and kept at the second. Not main's
/// fixture: main has no `owl2-dl`, so the only such store is one this engine wrote before.
#[tokio::test(flavor = "multi_thread")]
async fn an_owl2_dl_store_rebuilds_its_closure_once_for_its_keys()
-> Result<(), Box<dyn std::error::Error>> {
    for (mode, kept) in [("owl2-rl", [true, true]), ("owl2-dl", [false, true])] {
        let dir = tempfile::tempdir()?;
        let data = dir.path().join("data");
        let written = nrese_store::StoreService::new(nrese_store::StoreConfig::on_disk(&data))?;
        written.rematerialise(RuleProgram::builtin(Ruleset::Owl2Rl))?;
        drop(written);
        let config = configure(dir.path(), &data)?;
        let text = std::fs::read_to_string(&config)?.replace("\"owl2-rl\"", &format!("\"{mode}\""));
        std::fs::write(&config, text)?;
        let program = nrese_reasoner::ReasonerConfig::for_mode(
            nrese_reasoner::ReasoningMode::from_name(mode).expect("a mode"),
        )
        .materialised_program()
        .expect("rules");
        let mut starts = Vec::new();
        for _ in 0..2 {
            let server = Server::start(&config).await?;
            starts.push(
                server
                    .process
                    .output()
                    .contains("inferred stack is current"),
            );
            assert_eq!(
                marker(&data)["fingerprint"],
                format!("{:016x}", program.fingerprint()),
                "{mode}: not current"
            );
        }
        assert_eq!(
            starts, kept,
            "{mode}: the closure kept at the first and second start"
        );
    }
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
    let config = configure(dir.path(), &data)?;
    let (status, output) = Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_nrese-server"))
            .arg("restore")
            .arg(&backup)
            .arg("--config")
            .arg(&config)
            .env("RUST_LOG", "warn"),
    )?
    .finish(RESTORE)?;
    assert!(status.success(), "{status}:\n{output}");
    let server = Server::start(&config).await?;
    // An image holds the store, not its namespaces (`ex:` of the `prefixed` query), in
    // main as here.
    same_answers(&server, Some("nrese"), &["prefixed"]).await
}

/// `nrese-server query` reads inferences, so on a store whose closure an older semantics
/// version wrote it brings them up to date first, as the server's start does (it answered
/// from that closure: PR #58, lane M's sol-loop). The store: `C rdfs:subClassOf C` and a
/// closure without `C owl:equivalentClass C` (materialised first, the axiom bulk-loaded
/// after, which doesn't maintain the inferred stack), and the marker version 2 wrote,
/// pinned as written (nrese-store's `reasoning_state` test, tests audit DC-09).
#[test]
fn query_brings_a_closure_an_older_semantics_wrote_up_to_date()
-> Result<(), Box<dyn std::error::Error>> {
    const VERSION_2: &str =
        "ruleset owl2-rl\nfingerprint 9ba81328ee5cf5c7\nviolations 0\nlists nil-class\n";
    let dir = tempfile::tempdir()?;
    let data = dir.path().join("data");
    {
        let store = nrese_store::StoreService::new(nrese_store::StoreConfig::on_disk(&data))?;
        let rules = nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Owl2Rl)
            .materialised_program()
            .expect("rules");
        store.rematerialise(&rules)?;
        let axiom = dir.path().join("axiom.nt");
        std::fs::write(
            &axiom,
            "<urn:C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:C> .\n",
        )?;
        store.bulk_load(&nrese_store::BulkLoadRequest {
            files: vec![axiom],
            replace: false,
            graph: nrese_store::GraphTarget::DefaultGraph,
            invalid: None,
        })?;
    }
    std::fs::write(data.join("reasoning.state"), VERSION_2)?;
    let config = configure(dir.path(), &data)?;
    let (status, output) = Process::spawn(
        Command::new(env!("CARGO_BIN_EXE_nrese-server"))
            .arg("query")
            .arg("--config")
            .arg(&config)
            .arg("ASK { <urn:C> <http://www.w3.org/2002/07/owl#equivalentClass> <urn:C> }")
            .env("RUST_LOG", "warn"),
    )?
    .finish(QUERY)?;
    assert!(status.success(), "{status}:\n{output}");
    assert!(output.contains("true"), "{output}");
    Ok(())
}

/// A store under `reasoner.unnamed_classes = "skip"` whose closure the build before D16-773
/// row 1 wrote: that build made a hidden union class's members things because a rule was named
/// `scm-cls`, here a symmetry rule that makes nothing a thing (Sol's review r1, S1). Opened by
/// `nrese-server query` and by the server's start, the closure is rebuilt under this build's
/// rules, where `ex:x` is no `owl:Thing`. Opened by `nrese-server query` configured as a read
/// replica, which reasons nothing, the query is refused with the way out (rebuild the primary
/// and re-bootstrap the replica), not answered from that closure. Opened by the server's start
/// as a read replica, whose reads follow its primary's log, a reasoned read of that closure is
/// told `not-certified`: nothing on a replica vouches for it. Run by `nrese-server query` while
/// its directory is locked, the query is refused at the open and answers nothing. The older
/// closure is written by this build under the same rules and one more that makes things; its
/// marker is the older build's for these rules: their fingerprint with the value `skip` had
/// then, written down here.
#[tokio::test(flavor = "multi_thread")]
async fn a_skip_closure_a_rule_named_scm_cls_made_is_rebuilt_where_it_opens()
-> Result<(), Box<dyn std::error::Error>> {
    const OLD_SKIP: u64 = 0x5717_c1a5_5e5f_0007;
    const ASK: &str = "ASK { <http://example.com/x> a <http://www.w3.org/2002/07/owl#Thing> }";
    let pie = |more: &str| {
        format!(
            "Prefices\n{{\n  ex : http://example.com/\n  rdf : http://www.w3.org/1999/02/22-rdf-syntax-ns#\n  \
             rdfs : http://www.w3.org/2000/01/rdf-schema#\n  owl : http://www.w3.org/2002/07/owl#\n}}\n\
             Axioms\n{{\n}}\nRules\n{{\nId: prp-rng\n  p <rdfs:range> c\n  x p y\n  ---\n  y <rdf:type> c\n\n\
             Id: scm-cls\n  x <ex:knows> y\n  ---\n  y <ex:knows> x\n{more}}}\n"
        )
    };
    let things = "\nId: things\n  c <rdf:type> <owl:Class>\n  x <rdf:type> c\n  ---\n  x <rdf:type> <owl:Thing>\n";
    let program = |text: String| {
        let rules = UserRules::pie("user.pie", text).expect("the rules compile");
        RuleProgram::new(None, Some(Arc::new(rules))).expect("a program")
    };
    let mut wrong = Vec::new();
    let ways = [
        "query",
        "start",
        "replica query",
        "replica start",
        "locked query",
    ];
    for way in ways {
        let dir = tempfile::tempdir()?;
        let data = dir.path().join("data");
        {
            let store = nrese_store::StoreService::new(nrese_store::StoreConfig {
                hide_unnamed_classes: true,
                ..nrese_store::StoreConfig::on_disk(&data)
            })?;
            // Asserted only: written without reasoning, the older closure made below.
            let unreasoned = nrese_reasoner::ReasonerService::new(Default::default());
            nrese_store::MutationPipeline::new(Arc::new(store.clone()), Arc::new(unreasoned))
                .execute_update_str(
                "PREFIX ex: <http://example.com/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                 PREFIX owl: <http://www.w3.org/2002/07/owl#> \
                 INSERT DATA { ex:p rdfs:range [ a owl:Class ; owl:unionOf ( ex:A ex:B ) ] . ex:s ex:p ex:x }",
            )?;
            store.rematerialise(program(pie(things)))?;
            let older = String::from_utf8(store.execute_query_str(ASK)?.payload)?;
            assert!(
                older.contains("true"),
                "the older closure holds ex:x a owl:Thing"
            );
        }
        let old = format!(
            "fingerprint {:016x}",
            program(pie("")).fingerprint() ^ OLD_SKIP
        );
        let state = data.join("reasoning.state");
        let marker: Vec<String> = std::fs::read_to_string(&state)?
            .lines()
            .map(|line| {
                if line.starts_with("fingerprint ") {
                    old.clone()
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(&state, marker.join("\n") + "\n")?;
        let rules = dir.path().join("user.pie");
        std::fs::write(&rules, pie(""))?;
        let config = configure(dir.path(), &data)?;
        let reasoner = format!(
            "[reasoner]\nmode = \"custom\"\nrules = \"{}\"\nunnamed_classes = \"skip\"\n",
            rules.display().to_string().replace('\\', "/")
        );
        let text = std::fs::read_to_string(&config)?
            .replace("[reasoner]\nmode = \"owl2-rl\"\n", &reasoner);
        assert!(
            text.contains("unnamed_classes = \"skip\""),
            "the reasoner section was rewritten:\n{text}"
        );
        // A replica's directory that holds this marker, from its time as a primary: the
        // closure isn't the configured rules', and nothing reasons on a replica to rebuild
        // it. Its primary is never reached: `query` doesn't follow one, and the server's start
        // finds none at this address.
        let replica = "[replication]\nmode = \"replica\"\nprimary = \"http://127.0.0.1:9\"\n";
        std::fs::write(
            &config,
            text + if way.starts_with("replica") {
                replica
            } else {
                ""
            },
        )?;
        // The directory's lock as a running server holds it, the store's own: here a store
        // open in this process, its older closure as it was.
        let held = match way {
            "locked query" => Some(nrese_store::StoreService::new(nrese_store::StoreConfig {
                hide_unnamed_classes: true,
                ..nrese_store::StoreConfig::on_disk(&data)
            })?),
            _ => None,
        };
        // The marker as the query meets it, after that open.
        let marker = std::fs::read_to_string(&state)?;
        let (got, must, right) = if way.ends_with("start") {
            let server = Server::start(&config).await?;
            let (things, told) = server
                .query_status(
                    "nrese",
                    "SELECT ?x { ?x a <http://www.w3.org/2002/07/owl#Thing> }",
                )
                .await?;
            let thing = things.contains(&"<http://example.com/x>".to_owned());
            if way == "start" {
                (format!("ex:x a owl:Thing: {thing}"), "false", !thing)
            } else {
                let got = format!("ex:x a owl:Thing: {thing}, told {told:?}");
                let right = told.as_deref() == Some("not-certified");
                (got, "told Some(\"not-certified\")", right)
            }
        } else {
            let (status, output) = Process::spawn(
                Command::new(env!("CARGO_BIN_EXE_nrese-server"))
                    .arg("query")
                    .arg("--config")
                    .arg(&config)
                    .arg(ASK)
                    .env("RUST_LOG", "warn"),
            )?
            .finish(QUERY)?;
            if way == "query" {
                assert!(status.success(), "{status}:\n{output}");
                let thing = output.contains("true");
                (format!("ex:x a owl:Thing: {thing}"), "false", !thing)
            } else if way == "locked query" {
                // Refused at the open: nothing of that closure is answered. The closure met is
                // the older one: its marker still the older build's.
                let answered = output.contains("true");
                let first = output.lines().find(|line| !line.trim().is_empty());
                let fingerprint = marker.lines().find(|line| line.starts_with("fingerprint "));
                let older = fingerprint == Some(old.as_str());
                let got = format!(
                    "{status}, answered true: {answered}, first line {first:?}, \
                     marker {fingerprint:?}, the older build's: {older}"
                );
                let right = !status.success() && !answered && older;
                (got, "refused, nothing answered, the older marker", right)
            } else {
                // Refused, with the way out: not an answer from that closure.
                let refused = !status.success() && output.contains("re-bootstrap");
                let thing = status.success() && output.contains("true");
                let got = format!("{status}, ex:x a owl:Thing: {thing}, refused: {refused}");
                (got, "refused with the way out", refused)
            }
        };
        drop(held);
        let verdict = if right { "right" } else { "WRONG" };
        println!("CASE {way}: {verdict}: {got}, must be {must}");
        if !right {
            wrong.push(way);
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:?}");
    Ok(())
}
