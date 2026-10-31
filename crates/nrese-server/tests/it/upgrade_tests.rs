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
fn configure(dir: &Path, data: &Path) -> std::io::Result<PathBuf> {
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

/// This build's server, stopped (by its own handle) when dropped.
struct Server {
    process: Process,
    url: String,
    http: reqwest::Client,
}

impl Server {
    /// Starts the server with `config` and waits for the address it reports once it
    /// serves, then for it to answer ready there.
    async fn start(config: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut process = Process::spawn(
            Command::new(env!("CARGO_BIN_EXE_nrese-server"))
                .arg("--config")
                .arg(config)
                .env("RUST_LOG", "warn,nrese_server=info"),
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

    /// `what` failed, with what the server wrote.
    fn failed(&self, what: &str) -> Box<dyn std::error::Error> {
        format!(
            "{what}\n--- the server's output:\n{}",
            self.process.output()
        )
        .into()
    }

    async fn get(&self, path: &str) -> Result<String, Box<dyn std::error::Error>> {
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

    async fn update(&self, repo: &str, update: &str) -> Result<(), Box<dyn std::error::Error>> {
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
