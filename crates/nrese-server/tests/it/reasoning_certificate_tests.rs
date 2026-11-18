//! A store's reasoning certificate (`reasoning.state`) lets its start skip the reasoning. It
//! must describe the data the store opens with, however the store came to hold that data.
//!
//! Each row ends with a store that holds two statements whose consequence the configured
//! rules derive: `x a C` and `C rdfs:subClassOf D`, so `x a D`. A store that keeps its
//! certificate there must answer `ASK { x a D }` with `true`, and `entails` must not say the
//! consequence is not entailed. Every row lists its wrong cells, one per mode, and asserts
//! that there are none.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{StoreConfig, StoreService, WalDamage};
use tower::util::ServiceExt;

use crate::support::body_text;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// The `custom` mode's program: the subclass rule alone.
const CUSTOM: &str = "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
{ ?x rdf:type ?c . ?c rdfs:subClassOf ?d } => { ?x rdf:type ?d } .
";

const ALL: [ReasoningMode; 3] = [
    ReasoningMode::Rdfs,
    ReasoningMode::Owl2Rl,
    ReasoningMode::Custom,
];

/// The modes a start with reasoning off can leave with nothing for a later rematerialisation
/// to add: under `owl2-rl` the closure of any class axiom holds more than its statements.
/// What a restore refused for a leftover `reasoning.state` tells its operator.
const WHAT_TO_DO: &str = "remove it, or restore into an empty data directory";

const CLOSED_BY_ASSERTIONS: [ReasoningMode; 2] = [ReasoningMode::Rdfs, ReasoningMode::Custom];

fn reasoner(mode: ReasoningMode) -> TestResult<ReasonerConfig> {
    let config = ReasonerConfig::for_mode(mode);
    if !matches!(mode, ReasoningMode::Custom) {
        return Ok(config);
    }
    let rules = nrese_reasoner::UserRules::n3("custom.n3", CUSTOM)?;
    Ok(config.with_rules(Some(std::sync::Arc::new(rules)))?)
}

/// A store under `mode` with `config`, as the server builds it.
fn state_with(mode: ReasoningMode, config: StoreConfig) -> TestResult<AppState> {
    let state = AppState::new(
        StoreService::new(config)?,
        ReasonerService::new(reasoner(mode)?),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    );
    state.mark_ready();
    Ok(state)
}

fn state(dir: &Path, mode: ReasoningMode) -> TestResult<AppState> {
    state_with(mode, StoreConfig::on_disk(dir))
}

fn with_ontology(dir: &Path, mode: ReasoningMode, ontology: &Path) -> TestResult<AppState> {
    let config = StoreConfig {
        ontology_path: Some(ontology.to_path_buf()),
        ..StoreConfig::on_disk(dir)
    };
    state_with(mode, config)
}

/// What the server's start does before it serves: the inferences brought up to date with
/// the configured reasoning, or cleared without reasoning.
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

fn certified(state: &AppState) -> bool {
    let program = state.pipeline().reasoner().config().materialised_program();
    program.is_some_and(|program| state.store().reasoning_is_current(program))
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

async fn holds(state: &AppState, pattern: &str) -> TestResult<bool> {
    let response = build_app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .header("accept", "application/sparql-results+json")
                .body(Body::from(format!("{PREFIXES}ASK {{ {pattern} }}")))?,
        )
        .await?;
    Ok(body_text(response).await?.contains("\"boolean\":true"))
}

/// The started store's wrong answers about `x`: none where it answers `x a D` and `entails`
/// doesn't deny it. Its premises must be there.
async fn wrong_about(cell: &str, x: &str, state: &AppState) -> TestResult<Vec<String>> {
    if !holds(state, &format!("ex:{x} a ex:C")).await?
        || !holds(state, "ex:C rdfs:subClassOf ex:D").await?
    {
        return Err(format!("{cell}: the store doesn't hold the premises").into());
    }
    let mut wrong = Vec::new();
    let kept = certified(state);
    if !holds(state, &format!("ex:{x} a ex:D")).await? {
        wrong.push(format!(
            "{cell}: ex:{x} a ex:D answered false, certificate kept {kept}"
        ));
    }
    if let Some(program) = state.pipeline().reasoner().config().materialised_program() {
        let ex = |n: &str| nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/{n}"));
        let conclusion = nrese_rdf::Triple::new(ex(x), nrese_rdf::vocab::rdf::TYPE, ex("D"));
        if let Ok(nrese_store::Entailment::NotEntailed) =
            state.store().entails(program, &[conclusion])
        {
            wrong.push(format!(
                "{cell}: entails says ex:{x} a ex:D is not entailed"
            ));
        }
    }
    Ok(wrong)
}

fn assert_none(wrong: &[String]) {
    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
}

/// A certified store in `dir` holding `ex:C rdfs:subClassOf ex:D`.
async fn certified_subclass(dir: &Path, mode: ReasoningMode) -> TestResult<()> {
    let state = state(dir, mode)?;
    start(&state)?;
    update(&state, "ex:C rdfs:subClassOf ex:D .").await?;
    assert!(
        certified(&state),
        "{}: the first store is certified",
        mode.as_str()
    );
    Ok(())
}

/// The newest segment of the store's log.
fn newest_segment(dir: &Path) -> TestResult<PathBuf> {
    let mut segments: Vec<PathBuf> = std::fs::read_dir(dir.join("wal"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "wal"))
        .collect();
    segments.sort();
    segments.pop().ok_or_else(|| "no WAL segment".into())
}

#[derive(Clone, Copy)]
enum Damage {
    /// The last record's final byte flipped: a complete record failing its check.
    Checksum,
    /// The last record's length made to run past the file's end: it reads as a torn write.
    LengthField,
}

/// A certificate written after two commits made with reasoning off, then the log's last
/// record damaged so that recovery opens one revision earlier (`damage`, opened under
/// `policy`): the second commit's `ex:y a ex:D` is gone, the first's `ex:y a ex:C` stays.
async fn cut_cell(
    mode: ReasoningMode,
    damage: Damage,
    policy: WalDamage,
) -> TestResult<Vec<String>> {
    let cell = mode.as_str();
    let temp = tempfile::tempdir()?;
    let dir = temp.path();
    certified_subclass(dir, mode).await?;
    let before_cut = {
        let state = state(dir, ReasoningMode::Disabled)?;
        start(&state)?;
        update(&state, "ex:y a ex:C .").await?;
        let at = state.store().current_revision();
        update(&state, "ex:y a ex:D .").await?;
        at
    };
    {
        // Nothing for the rematerialisation to add: it publishes no revision and certifies.
        let state = state(dir, mode)?;
        let at = state.store().current_revision();
        start(&state)?;
        assert_eq!(
            state.store().current_revision(),
            at,
            "{cell}: the start published"
        );
        assert!(certified(&state), "{cell}: the start certified");
    }
    let segment = newest_segment(dir)?;
    let mut bytes = std::fs::read(&segment)?;
    match damage {
        Damage::Checksum => {
            let last = bytes.len() - 1;
            bytes[last] ^= 0xff;
        }
        Damage::LengthField => {
            // The frames after the 8-byte segment header: `len: u32 | crc: u32 | payload`.
            let (mut at, mut last) = (8, None);
            while at + 8 <= bytes.len() {
                let len = u32::from_le_bytes(bytes[at..at + 4].try_into()?) as usize;
                last = Some((at, len));
                at += 8 + len;
            }
            assert_eq!(
                at,
                bytes.len(),
                "{cell}: the segment ends at a record's end"
            );
            let (frame, len) = last.ok_or("no record")?;
            bytes[frame..frame + 4].copy_from_slice(&(len as u32 + 4096).to_le_bytes());
        }
    }
    std::fs::write(&segment, &bytes)?;
    let config = StoreConfig {
        wal_damage: policy,
        ..StoreConfig::on_disk(dir)
    };
    let state = state_with(mode, config)?;
    assert_eq!(
        state.store().current_revision(),
        before_cut,
        "{cell}: recovery cut one record"
    );
    start(&state)?;
    wrong_about(cell, "y", &state).await
}

/// D5: a damaged last record, cut when the operator lets recovery keep the log's prefix.
#[tokio::test(flavor = "multi_thread")]
async fn a_log_cut_back_by_a_damaged_record_leaves_no_certificate() -> TestResult<()> {
    let mut wrong = Vec::new();
    for mode in CLOSED_BY_ASSERTIONS {
        wrong.extend(cut_cell(mode, Damage::Checksum, WalDamage::KeepPrefix).await?);
    }
    assert_none(&wrong);
    Ok(())
}

/// D5 at the default policy: a damaged length field reads as a torn write, cut by default.
#[tokio::test(flavor = "multi_thread")]
async fn a_log_cut_back_by_a_damaged_length_field_leaves_no_certificate() -> TestResult<()> {
    let mut wrong = Vec::new();
    for mode in CLOSED_BY_ASSERTIONS {
        wrong.extend(cut_cell(mode, Damage::LengthField, WalDamage::Refuse).await?);
    }
    assert_none(&wrong);
    Ok(())
}

/// An ontology file holding `ex:x a ex:C`, beside the store.
fn ontology(temp: &Path, statements: &str) -> TestResult<PathBuf> {
    let file = temp.join("ontology.ttl");
    std::fs::write(
        &file,
        format!(
            "@prefix ex: <http://example.com/> .\n\
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n{statements}\n"
        ),
    )?;
    Ok(file)
}

/// D4: a certified store reopened with an ontology file; its open fails after the preload
/// committed (a namespaces file that doesn't parse). Repaired and reopened, the preload adds
/// nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_preload_committed_in_a_failed_open_leaves_no_certificate() -> TestResult<()> {
    let mut wrong = Vec::new();
    for mode in ALL {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("store");
        std::fs::create_dir(&dir)?;
        certified_subclass(&dir, mode).await?;
        let file = ontology(temp.path(), "ex:x a ex:C .")?;
        let namespaces = dir.join("namespaces.json");
        std::fs::write(&namespaces, b"{\"ex\": ")?;
        assert!(
            with_ontology(&dir, mode, &file).is_err(),
            "{}: the open fails",
            mode.as_str()
        );
        std::fs::remove_file(&namespaces)?;
        let state = with_ontology(&dir, mode, &file)?;
        start(&state)?;
        wrong.extend(wrong_about(mode.as_str(), "x", &state).await?);
    }
    assert_none(&wrong);
    Ok(())
}

/// D4 where the open fails at the certificate's own removal: a store certified over
/// `ex:x a ex:C` reopened with an ontology file adding `ex:C rdfs:subClassOf ex:D`, its data
/// directory read-only (the log's directory stays writable). Repaired and reopened, the
/// preload adds nothing.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_preload_whose_certificate_removal_failed_leaves_no_certificate() -> TestResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions =
        |dir: &Path, bits| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(bits));
    let mut wrong = Vec::new();
    for mode in ALL {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("store");
        std::fs::create_dir(&dir)?;
        {
            let state = state(&dir, mode)?;
            start(&state)?;
            update(&state, "ex:x a ex:C .").await?;
            assert!(
                certified(&state),
                "{}: the first store is certified",
                mode.as_str()
            );
        }
        let file = ontology(temp.path(), "ex:C rdfs:subClassOf ex:D .")?;
        permissions(&dir, 0o555)?;
        let refused = std::fs::write(dir.join("probe"), b"").is_err();
        let opened = with_ontology(&dir, mode, &file);
        permissions(&dir, 0o755)?;
        assert!(
            refused,
            "the data directory refuses entries (run as an unprivileged user)"
        );
        assert!(opened.is_err(), "{}: the open fails", mode.as_str());
        let state = with_ontology(&dir, mode, &file)?;
        start(&state)?;
        wrong.extend(wrong_about(mode.as_str(), "x", &state).await?);
    }
    assert_none(&wrong);
    Ok(())
}

/// D6: a directory whose certified store was removed but for its `reasoning.state`, and an
/// image of another store restored into it; that store holds `ex:x a ex:C`, committed with
/// reasoning off. The restore refuses, as it refuses a directory holding a store: its error
/// names the file and says what to do.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_beside_a_leftover_certificate_refuses_and_says_what_to_do() -> TestResult<()> {
    let mut wrong = Vec::new();
    for mode in ALL {
        let temp = tempfile::tempdir()?;
        let (target, other, backup) = (
            temp.path().join("target"),
            temp.path().join("other"),
            temp.path().join("backup"),
        );
        for dir in [&target, &other, &backup] {
            std::fs::create_dir(dir)?;
        }
        certified_subclass(&target, mode).await?;
        certified_subclass(&other, mode).await?;
        {
            let state = state(&other, ReasoningMode::Disabled)?;
            start(&state)?;
            update(&state, "ex:x a ex:C .").await?;
            state.store().backup_image(&backup)?;
        }
        for entry in std::fs::read_dir(&target)? {
            let entry = entry?;
            if entry.file_name() == "reasoning.state" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        let cell = mode.as_str();
        match nrese_store::restore_image(&backup, &target) {
            Err(error) => {
                let text = error.to_string();
                if !(text.contains("reasoning.state") && text.contains(WHAT_TO_DO)) {
                    wrong.push(format!(
                        "{cell}: refused without naming the file and what to do: {text}"
                    ));
                }
            }
            Ok(_) => {
                wrong.push(format!(
                    "{cell}: restored beside a leftover reasoning.state"
                ));
                let state = state(&target, mode)?;
                start(&state)?;
                wrong.extend(wrong_about(cell, "x", &state).await?);
            }
        }
    }
    assert_none(&wrong);
    Ok(())
}
