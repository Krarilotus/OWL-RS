//! Every way a store comes to hold its data, under every setting that changes how stored data
//! is read, answers as a freshly loaded store does: the same rows for a fixed set of reads,
//! the same completeness status for each, the same consistency and readiness. Reader state
//! that derives from the reasoning state (the equality reading, the QL rewriting, the status,
//! consistency) must follow the data however it arrived, a read replica's included.
//!
//! One test walks the whole table and lists every wrong cell, one line each
//! (`WRONG way | column | aspect`), so a run shows which cells are wrong and no other.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::replication::{self, ReplicationConfig, ReplicationMode};
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{QlRewritingMode, StoreConfig, StoreService};
use tower::util::ServiceExt;

use crate::support::body_text;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> ";

/// The data, in two parts: a subclass, an existential no rule materialises, an `owl:sameAs`
/// pair.
const FIRST: &str = "ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog . \
     ex:Employee rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:worksFor ; \
     owl:someValuesFrom owl:Thing ] .";
const SECOND: &str = "ex:a a ex:Dog . ex:a owl:sameAs ex:b . ex:bob a ex:Employee .";

/// The reads: a variable predicate, the schema vocabulary, instances (an `owl:sameAs` class
/// among them), and the members of an existential (only the QL rewriting answers it).
const READS: [(&str, &str); 4] = [
    ("variable predicate", "ASK { ex:Dog ?p ex:Animal }"),
    ("schema", "ASK { ex:Dog rdfs:subClassOf ex:Animal }"),
    (
        "instances and sameAs",
        "SELECT ?x WHERE { ?x a ex:Animal } ORDER BY ?x",
    ),
    (
        "existential",
        "SELECT ?x WHERE { ?x ex:worksFor ?y } ORDER BY ?x",
    ),
];

const WAYS: [&str; 12] = [
    "load",
    "commit",
    "reopen",
    "bulk load",
    "rematerialisation",
    "change of rules",
    "image restore",
    "restore by time",
    "bootstrapped replica",
    "replica applying the log",
    "replica opened as primary",
    "replica across a change of rules",
];

#[derive(Clone, Copy)]
enum Equality {
    Representatives,
    Compact,
    Replicate,
}

impl Equality {
    fn name(self) -> &'static str {
        match self {
            Self::Representatives => "representatives",
            Self::Compact => "compact",
            Self::Replicate => "replicate",
        }
    }
}

/// A setting that changes how stored data is read: the mode, `reasoner.equality`, and
/// `reasoner.ql_rewriting`.
#[derive(Clone)]
struct Column {
    name: String,
    mode: ReasoningMode,
    equality: Equality,
    ql: QlRewritingMode,
}

impl Column {
    fn new(mode: ReasoningMode, equality: Equality, ql: QlRewritingMode) -> Self {
        Self {
            name: format!("{} {} ql={}", mode.as_str(), equality.name(), ql.name()),
            mode,
            equality,
            ql,
        }
    }

    fn config(&self, dir: &Path) -> StoreConfig {
        let (equality_by_representatives, equality_compact) = match self.equality {
            Equality::Representatives => (true, false),
            Equality::Compact => (true, true),
            Equality::Replicate => (false, false),
        };
        StoreConfig {
            equality_by_representatives,
            equality_compact,
            ql_rewriting: self.ql,
            wal_archive: true,
            ..StoreConfig::on_disk(dir)
        }
    }

    fn state(
        &self,
        dir: &Path,
        mode: ReasoningMode,
        replication: Option<ReplicationConfig>,
    ) -> TestResult<AppState> {
        let state = AppState::new(
            StoreService::new(self.config(dir))?,
            ReasonerService::new(ReasonerConfig::for_mode(mode)),
            PolicyConfig::default(),
            AiSuggestionService::disabled(),
            DeploymentPosture::OpenWorkbench,
        );
        let state = match replication {
            Some(replication) => state.with_replication(replication),
            None => state,
        };
        state.mark_ready();
        Ok(state)
    }
}

/// Every mode; each `reasoner.equality` value where the rules close equality (`eq-rep-s`);
/// QL rewriting on and off where it can apply.
fn columns() -> Vec<Column> {
    use Equality::{Compact, Replicate, Representatives};
    use QlRewritingMode::{Auto, Off, On};
    use ReasoningMode::{Disabled, Owl2Dl, Owl2Ql, Owl2Rl, OwlHorst, Rdfs, RdfsFull, RdfsPlus};
    let equalities = [Representatives, Compact, Replicate];
    let mut columns = vec![
        Column::new(Disabled, Representatives, Auto),
        Column::new(Rdfs, Representatives, Auto),
        Column::new(RdfsFull, Representatives, Auto),
    ];
    for mode in [RdfsPlus, OwlHorst] {
        for equality in equalities {
            columns.push(Column::new(mode, equality, Auto));
        }
    }
    for ql in [Auto, Off] {
        columns.push(Column::new(Owl2Ql, Representatives, ql));
    }
    for equality in equalities {
        for ql in [Auto, On] {
            columns.push(Column::new(Owl2Rl, equality, ql));
        }
    }
    for equality in equalities {
        columns.push(Column::new(Owl2Dl, equality, Auto));
    }
    columns
}

/// Another mode, for a change of rules to `mode`.
fn other(mode: ReasoningMode) -> ReasoningMode {
    match mode {
        ReasoningMode::Rdfs => ReasoningMode::OwlHorst,
        _ => ReasoningMode::Rdfs,
    }
}

fn primary() -> ReplicationConfig {
    ReplicationConfig {
        mode: ReplicationMode::Primary,
        ..ReplicationConfig::default()
    }
}

fn both() -> String {
    format!("{FIRST} {SECOND}")
}

/// What a store was given to keep for a cell: its directories and the primaries it serves.
#[derive(Default)]
struct Keep {
    dirs: Vec<tempfile::TempDir>,
    served: Vec<Served>,
}

impl Keep {
    fn dir(&mut self) -> TestResult<PathBuf> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().to_path_buf();
        self.dirs.push(dir);
        Ok(path)
    }

    async fn finish(self) {
        for served in self.served {
            served.end().await;
        }
    }
}

/// A primary served on a port of its own, until [`Served::end`].
struct Served {
    base: String,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Served {
    async fn start(state: &AppState) -> TestResult<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let app = build_app(state.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("serve");
        });
        Ok(Self { base, stop, task })
    }

    async fn end(self) {
        let _ = self.stop.send(());
        let task = self.task;
        let abort = task.abort_handle();
        if tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .is_err()
        {
            abort.abort();
        }
    }
}

async fn update(state: &AppState, triples: &str) -> TestResult<()> {
    let response = build_app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(format!("{PREFIXES}INSERT DATA {{ {triples} }}")))?,
        )
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("update: {status}: {}", body_text(response).await?).into());
    }
    Ok(())
}

/// Loads `triples` from a Turtle file, as an import's first phase does.
fn bulk_load(state: &AppState, files: &Path, triples: &str) -> TestResult<()> {
    let file = files.join("data.ttl");
    std::fs::write(
        &file,
        format!(
            "@prefix ex: <http://example.com/> .\n\
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             @prefix owl: <http://www.w3.org/2002/07/owl#> .\n{triples}\n"
        ),
    )?;
    state.store().bulk_load(&nrese_store::BulkLoadRequest {
        files: vec![file],
        replace: false,
        graph: nrese_store::GraphTarget::DefaultGraph,
        invalid: None,
    })?;
    Ok(())
}

/// Reasons over the stored data, as an import's second phase does.
fn reason(state: &AppState) -> TestResult<()> {
    if let Some(program) = state.pipeline().reasoner().config().materialised_program() {
        state.store().rematerialise(program)?;
    }
    Ok(())
}

/// What the server's start does before it serves (not on a replica): the inferences brought
/// up to date with the configured reasoning.
fn start(state: &AppState) -> TestResult<()> {
    let store = state.store();
    match state.pipeline().reasoner().config().materialised_program() {
        Some(program) if store.reasoning_is_current(program.clone()) => {}
        Some(program) => {
            store.rematerialise(program)?;
        }
        None => {
            store.clear_inferred()?;
        }
    }
    Ok(())
}

fn reopened(column: &Column, dir: &Path) -> TestResult<AppState> {
    let state = column.state(dir, column.mode, None)?;
    start(&state)?;
    Ok(state)
}

async fn catch_up(primary: &str, replica: &AppState) -> TestResult<()> {
    let client = reqwest::Client::new();
    while replication::pull_once(&client, primary, replica).await? > 0 {}
    Ok(())
}

/// A replica of the primary at `base`, configured as `column`, bootstrapped from its image.
async fn replica(column: &Column, base: &str, keep: &mut Keep) -> TestResult<(AppState, PathBuf)> {
    let dir = keep.dir()?;
    let config = ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(base.to_owned()),
        poll: Duration::from_millis(20),
        ..ReplicationConfig::default()
    };
    replication::bootstrap(&config, &dir).await?;
    let state = column.state(&dir, column.mode, Some(config))?;
    Ok((state, dir))
}

/// A store holding the data, reached by `way` under `column`.
async fn reach(way: &str, column: &Column, keep: &mut Keep) -> TestResult<AppState> {
    let dir = keep.dir()?;
    let fresh = |mode| column.state(&dir, mode, None);
    match way {
        "load" => {
            let state = fresh(column.mode)?;
            update(&state, &both()).await?;
            Ok(state)
        }
        "commit" => {
            let state = fresh(column.mode)?;
            update(&state, FIRST).await?;
            update(&state, SECOND).await?;
            Ok(state)
        }
        "reopen" => {
            {
                let state = fresh(column.mode)?;
                update(&state, &both()).await?;
            }
            reopened(column, &dir)
        }
        "bulk load" => {
            let state = fresh(column.mode)?;
            bulk_load(&state, &keep.dir()?, &both())?;
            reason(&state)?;
            Ok(state)
        }
        "rematerialisation" => {
            let state = fresh(column.mode)?;
            update(&state, &both()).await?;
            reason(&state)?;
            Ok(state)
        }
        "change of rules" => {
            {
                let state = fresh(other(column.mode))?;
                update(&state, &both()).await?;
            }
            reopened(column, &dir)
        }
        "image restore" => {
            let image = keep.dir()?.join("image");
            {
                let state = fresh(column.mode)?;
                update(&state, &both()).await?;
                state.store().backup_image(&image)?;
            }
            let target = keep.dir()?.join("store");
            nrese_store::restore_image(&image, &target)?;
            reopened(column, &target)
        }
        "restore by time" => {
            let image = keep.dir()?.join("image");
            {
                let state = fresh(column.mode)?;
                update(&state, FIRST).await?;
                state.store().backup_image(&image)?;
                update(&state, SECOND).await?;
            }
            let target = keep.dir()?.join("store");
            let logs = [dir.join("wal-archive"), dir.join("wal")];
            nrese_store::restore_until(&image, &target, &logs, None, None)?;
            reopened(column, &target)
        }
        "bootstrapped replica" => {
            let primary_state = column.state(&dir, column.mode, Some(primary()))?;
            update(&primary_state, &both()).await?;
            let served = Served::start(&primary_state).await?;
            let (state, _) = replica(column, &served.base, keep).await?;
            catch_up(&served.base, &state).await?;
            keep.served.push(served);
            Ok(state)
        }
        "replica applying the log" | "replica opened as primary" => {
            let primary_state = column.state(&dir, column.mode, Some(primary()))?;
            update(&primary_state, FIRST).await?;
            let served = Served::start(&primary_state).await?;
            let (state, replica_dir) = replica(column, &served.base, keep).await?;
            update(&primary_state, SECOND).await?;
            catch_up(&served.base, &state).await?;
            keep.served.push(served);
            if way == "replica applying the log" {
                return Ok(state);
            }
            // There is no promotion: the replica's directory is started as a primary.
            drop(state);
            reopened(column, &replica_dir)
        }
        "replica across a change of rules" => {
            let (state, served) = {
                let primary_state = column.state(&dir, other(column.mode), Some(primary()))?;
                update(&primary_state, &both()).await?;
                let served = Served::start(&primary_state).await?;
                let (state, _) = replica(column, &served.base, keep).await?;
                catch_up(&served.base, &state).await?;
                (state, served)
            };
            served.end().await;
            // The primary restarts under the column's rules: its start reasons again, and
            // the replica follows its log.
            let primary_state = column.state(&dir, column.mode, Some(primary()))?;
            start(&primary_state)?;
            let served = Served::start(&primary_state).await?;
            catch_up(&served.base, &state).await?;
            keep.served.push(served);
            Ok(state)
        }
        _ => Err(format!("no way {way}").into()),
    }
}

/// What a reader sees of a store.
#[derive(Debug, PartialEq)]
struct Seen {
    reads: Vec<(String, Option<String>)>,
    consistency: nrese_store::ConsistencyStatus,
    quarantined: bool,
}

async fn answered(state: &AppState, query: &str) -> TestResult<(String, Option<String>)> {
    let response = build_app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .header("accept", "application/sparql-results+json")
                .body(Body::from(format!("{PREFIXES}{query}")))?,
        )
        .await?;
    let status = response
        .headers()
        .get("nrese-completeness")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    Ok((body_text(response).await?, status))
}

async fn seen(state: &AppState) -> TestResult<Seen> {
    let mut reads = Vec::new();
    for (_, query) in READS {
        reads.push(answered(state, query).await?);
    }
    Ok(Seen {
        reads,
        consistency: state.store().consistency(),
        quarantined: state.is_quarantined(),
    })
}

/// Lists each aspect of `got` that differs from `want`, with what each was.
fn compare(cell: &str, got: &Seen, want: &Seen, wrong: &mut Vec<String>) {
    for (i, (name, _)) in READS.iter().enumerate() {
        let ((got_rows, got_status), (want_rows, want_status)) = (&got.reads[i], &want.reads[i]);
        if got_rows != want_rows {
            eprintln!("DETAIL {cell} | {name}: rows {got_rows}, want {want_rows}");
            wrong.push(format!("{cell} | {name}: rows"));
        }
        if got_status != want_status {
            eprintln!("DETAIL {cell} | {name}: status {got_status:?}, want {want_status:?}");
            wrong.push(format!("{cell} | {name}: status"));
        }
    }
    if got.consistency != want.consistency {
        eprintln!(
            "DETAIL {cell} | consistency {:?}, want {:?}",
            got.consistency, want.consistency
        );
        wrong.push(format!("{cell} | consistency"));
    }
    if got.quarantined != want.quarantined {
        eprintln!(
            "DETAIL {cell} | quarantined {}, want {}",
            got.quarantined, want.quarantined
        );
        wrong.push(format!("{cell} | readiness"));
    }
}

/// An import's window: between a bulk load's commit and its reasoning, a store with
/// reasoning on doesn't know what its inferred statements were reasoned with, so every read
/// says `not-certified`; once the reasoning has run, it answers as a loaded store does.
async fn import_window(column: &Column, want: &Seen, wrong: &mut Vec<String>) -> TestResult<()> {
    let mut keep = Keep::default();
    let dir = keep.dir()?;
    let state = column.state(&dir, column.mode, None)?;
    bulk_load(&state, &keep.dir()?, &both())?;
    let during = seen(&state).await?;
    let reasons = !matches!(column.mode, ReasoningMode::Disabled);
    for (i, (name, _)) in READS.iter().enumerate() {
        let status = &during.reads[i].1;
        let right = if reasons {
            status
                .as_deref()
                .is_some_and(|s| s.starts_with("not-certified"))
        } else {
            *status == want.reads[i].1
        };
        if !right {
            eprintln!(
                "DETAIL import window | {} | {name}: status {status:?}",
                column.name
            );
            wrong.push(format!("import window | {} | {name}: status", column.name));
        }
    }
    reason(&state)?;
    let after = seen(&state).await?;
    compare(
        &format!("import window, after its reasoning | {}", column.name),
        &after,
        want,
        wrong,
    );
    drop(state);
    keep.finish().await;
    Ok(())
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// EXPLAIN's status for `query`, with `infer=false` when `infer` is false.
async fn explained(state: &AppState, query: &str, infer: bool) -> TestResult<serde_json::Value> {
    let infer = if infer { "" } else { "&infer=false" };
    let uri = format!(
        "/dataset/query?query={}&explain=true{infer}",
        encode(&format!("{PREFIXES}{query}"))
    );
    let response = build_app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    let explanation: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    Ok(explanation["completeness"].clone())
}

/// EXPLAIN's two narrow cases, where no reasoning path can leave answers out (a store with no
/// reasoning; a read of the asserted statements alone): a `complete` status object with no
/// regime, never `null`.
async fn explain_cells(wrong: &mut Vec<String>) -> TestResult<()> {
    let want = serde_json::json!({
        "status": "complete",
        "regime": null,
        "sound": true,
        "complete": true,
        "reasons": [],
        "bounds": null,
        "decided_by": [],
    });
    let read = READS[2].1;
    for (cell, column, infer) in [
        (
            "explain | no reasoning | completeness",
            Column::new(
                ReasoningMode::Disabled,
                Equality::Representatives,
                QlRewritingMode::Auto,
            ),
            true,
        ),
        (
            "explain | owl2-rl, infer=false | completeness",
            Column::new(
                ReasoningMode::Owl2Rl,
                Equality::Representatives,
                QlRewritingMode::Auto,
            ),
            false,
        ),
    ] {
        let mut keep = Keep::default();
        let state = column.state(&keep.dir()?, column.mode, None)?;
        update(&state, &both()).await?;
        let got = explained(&state, read, infer).await?;
        if got != want {
            eprintln!("DETAIL {cell}: {got}");
            wrong.push(cell.to_owned());
        }
        drop(state);
        keep.finish().await;
    }
    Ok(())
}

/// A replica of an inconsistent primary: the primary is quarantined (readable, not ready),
/// and the replica says what the primary says.
async fn quarantine_cell(wrong: &mut Vec<String>, unmet: &mut Vec<String>) -> TestResult<()> {
    let column = Column::new(
        ReasoningMode::Owl2Rl,
        Equality::Representatives,
        QlRewritingMode::Auto,
    );
    let mut keep = Keep::default();
    let dir = keep.dir()?;
    let primary_state = column.state(&dir, column.mode, Some(primary()))?;
    update(&primary_state, &both()).await?;
    bulk_load(
        &primary_state,
        &keep.dir()?,
        "ex:C owl:disjointWith ex:D . ex:x a ex:C , ex:D .",
    )?;
    reason(&primary_state)?;
    if !primary_state.is_quarantined() {
        unmet.push(format!(
            "quarantine: the primary isn't quarantined ({:?})",
            primary_state.store().consistency()
        ));
    }
    let served = Served::start(&primary_state).await?;
    let (state, _) = replica(&column, &served.base, &mut keep).await?;
    catch_up(&served.base, &state).await?;
    keep.served.push(served);
    let cell = format!("quarantine | {}", column.name);
    let (consistency, want) = (
        state.store().consistency(),
        primary_state.store().consistency(),
    );
    if consistency != want {
        eprintln!("DETAIL {cell} | consistency {consistency:?}, want {want:?}");
        wrong.push(format!("{cell} | consistency"));
    }
    let (quarantined, want) = (state.is_quarantined(), primary_state.is_quarantined());
    if quarantined != want {
        eprintln!("DETAIL {cell} | quarantined {quarantined}, want {want}");
        wrong.push(format!("{cell} | readiness"));
    }
    drop((state, primary_state));
    keep.finish().await;
    Ok(())
}

/// The table: every way × every column against a freshly loaded store of that column, the
/// import window, EXPLAIN's two narrow cases and the quarantine cell. Every wrong cell is
/// printed (`WRONG …`), every cell that couldn't run too (`ERROR …`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_way_to_a_store_reads_as_a_loaded_store() -> TestResult<()> {
    let limit = Duration::from_secs(120);
    let (mut wrong, mut errors, mut unmet) = (Vec::new(), Vec::new(), Vec::new());
    for column in columns() {
        let mut keep = Keep::default();
        let want = match tokio::time::timeout(limit, async {
            let state = reach("load", &column, &mut keep).await?;
            seen(&state).await
        })
        .await
        {
            Ok(Ok(want)) => want,
            Ok(Err(error)) => {
                errors.push(format!("load | {} | {error}", column.name));
                continue;
            }
            Err(_) => {
                errors.push(format!("load | {} | timed out", column.name));
                continue;
            }
        };
        keep.finish().await;
        for way in &WAYS[1..] {
            let mut keep = Keep::default();
            let cell = format!("{way} | {}", column.name);
            let got = tokio::time::timeout(limit, async {
                let state = reach(way, &column, &mut keep).await?;
                seen(&state).await
            })
            .await;
            match got {
                Ok(Ok(got)) => compare(&cell, &got, &want, &mut wrong),
                Ok(Err(error)) => errors.push(format!("{cell} | {error}")),
                Err(_) => errors.push(format!("{cell} | timed out")),
            }
            keep.finish().await;
        }
        match tokio::time::timeout(limit, import_window(&column, &want, &mut wrong)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(format!("import window | {} | {error}", column.name)),
            Err(_) => errors.push(format!("import window | {} | timed out", column.name)),
        }
    }
    if let Err(error) = explain_cells(&mut wrong).await {
        errors.push(format!("explain | {error}"));
    }
    if let Err(error) = quarantine_cell(&mut wrong, &mut unmet).await {
        errors.push(format!("quarantine | {error}"));
    }
    for line in &wrong {
        eprintln!("WRONG {line}");
    }
    for line in &errors {
        eprintln!("ERROR {line}");
    }
    for line in &unmet {
        eprintln!("UNMET {line}");
    }
    assert!(
        wrong.is_empty() && errors.is_empty() && unmet.is_empty(),
        "{} wrong cells, {} errors, {} unmet preconditions (the lines above)",
        wrong.len(),
        errors.len(),
        unmet.len()
    );
    Ok(())
}
