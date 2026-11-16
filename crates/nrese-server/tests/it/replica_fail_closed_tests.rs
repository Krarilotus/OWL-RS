//! A read replica never reads more than it can show (the fail-closed form,
//! docs/ops/replication.md): a primary whose rows are stored compact serves it nothing,
//! and the replica says why; a directory once started as a primary doesn't vouch for the
//! records it applied as a replica.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::replication::{self, ReplicationConfig, ReplicationMode};
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

use super::upgrade_tests::{Server, configure};
use crate::support::body_text;

const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// What a refusal says, in part: the sentence the replica reports.
const REFUSED: &str = "serves no replica";

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn server(store: StoreService, mode: ReasoningMode, replication: ReplicationConfig) -> AppState {
    let state = AppState::new(
        store,
        ReasonerService::new(ReasonerConfig::for_mode(mode)),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(replication);
    state.mark_ready();
    state
}

/// `state` served on a real port; its base URL.
async fn serve(state: &AppState) -> TestResult<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = build_app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("primary");
    });
    Ok(base)
}

fn primary() -> ReplicationConfig {
    ReplicationConfig {
        mode: ReplicationMode::Primary,
        ..ReplicationConfig::default()
    }
}

fn replica_of(base: &str) -> ReplicationConfig {
    ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(base.to_owned()),
        poll: std::time::Duration::from_millis(20),
        ..ReplicationConfig::default()
    }
}

/// `config` for a replica's store: opened with the role ([`replica_of`]'s state takes it).
fn as_replica(config: StoreConfig) -> StoreConfig {
    StoreConfig {
        replica: true,
        ..config
    }
}

async fn update(base: &str, text: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/dataset/update"))
        .header("content-type", "application/sparql-update")
        .body(format!("{PREFIXES}{text}"))
        .send()
        .await
        .expect("update");
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
}

/// The rows and the `nrese-completeness` header of `query`'s answer.
async fn answered(app: axum::Router, query: &str) -> TestResult<(String, Option<String>)> {
    let response = app
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

/// The replica's `last_error`, as `/api/v1/replication/status` gives it.
async fn last_error(replica: &AppState) -> TestResult<Option<String>> {
    let response = build_app(replica.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/replication/status")
                .body(Body::empty())?,
        )
        .await?;
    let status: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    Ok(status["last_error"].as_str().map(str::to_owned))
}

/// A primary under `mode` given `first` and served; its replica bootstrapped from the image
/// and caught up after `second`. Both stores configured by `config`; the directories go with
/// them.
async fn primary_and_replica(
    mode: ReasoningMode,
    config: impl Fn(&std::path::Path) -> StoreConfig,
    first: &str,
    second: &str,
) -> TestResult<(AppState, AppState, [tempfile::TempDir; 2])> {
    let dirs = [tempfile::tempdir()?, tempfile::tempdir()?];
    let primary_state = server(StoreService::new(config(dirs[0].path()))?, mode, primary());
    let base = serve(&primary_state).await?;
    update(&base, first).await;
    let following = replica_of(&base);
    replication::bootstrap(&following, dirs[1].path()).await?;
    let replica_state = server(
        StoreService::new(as_replica(config(dirs[1].path())))?,
        mode,
        following,
    );
    update(&base, second).await;
    let client = reqwest::Client::new();
    while replication::pull_once(&client, &base, &replica_state).await? > 0 {}
    Ok((primary_state, replica_state, dirs))
}

/// The values a result's rows bind `?x` to.
fn bound(rows: &str) -> Vec<String> {
    let rows: serde_json::Value = serde_json::from_str(rows).unwrap_or_default();
    rows["results"]["bindings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["x"]["value"].as_str().map(str::to_owned))
        .collect()
}

/// Spec (the replica's fail-closed form, half page point 3; master 12:15 and 14:38): a
/// primary whose served version is stored compact (`reasoner.equality = "compact"` under
/// `owl2-rl`, which closes equality) serves no replica. A replica starting from it fails
/// with the sentence (`bootstrap`); one following its log from a store it holds fails with
/// it (a round), and its `/replication/status` gives it as `last_error` (`follow`). Under
/// `rdfs` the same configuration stores rows as they are: the replica starts from the image
/// and follows the next commit. Each case is printed. (It takes the place of the test that
/// had such a replica give its primary's rows.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_of_a_compact_primary_fails_with_the_sentence() -> TestResult<()> {
    let compact = |dir: &std::path::Path| StoreConfig {
        equality_by_representatives: true,
        equality_compact: true,
        ..StoreConfig::on_disk(dir)
    };
    let mut wrong = Vec::new();
    for (mode, stored_compact) in [(ReasoningMode::Owl2Rl, true), (ReasoningMode::Rdfs, false)] {
        let primary_dir = tempfile::tempdir()?;
        let primary_state = server(
            StoreService::new(compact(primary_dir.path()))?,
            mode,
            primary(),
        );
        let base = serve(&primary_state).await?;
        update(
            &base,
            "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:a a ex:Dog . \
             ex:a owl:sameAs ex:b }",
        )
        .await;
        let config = replica_of(&base);
        let mut check = |case: &str, right: bool, seen: String| {
            let verdict = if right { "right" } else { "WRONG" };
            println!("CASE {mode:?} {case}: {verdict} {seen}");
            if !right {
                wrong.push(format!("{mode:?} {case}: {seen}"));
            }
        };

        let replica_dir = tempfile::tempdir()?;
        let booted = replication::bootstrap(&config, replica_dir.path())
            .await
            .map_err(|error| format!("{error:#}"));
        check(
            "bootstrap",
            match &booted {
                Err(error) => stored_compact && error.contains(REFUSED),
                Ok(started) => !stored_compact && started.is_some(),
            },
            format!("{booted:?}"),
        );

        // Refused, the replica's directory stays empty: it follows from its own empty store.
        let replica_state = server(
            StoreService::new(as_replica(compact(replica_dir.path())))?,
            mode,
            config,
        );
        update(&base, "INSERT DATA { ex:c owl:sameAs ex:a }").await;
        let round = replication::pull_once(&reqwest::Client::new(), &base, &replica_state)
            .await
            .map_err(|error| format!("{error:#}"));
        check(
            "a round",
            match &round {
                Err(error) => stored_compact && error.contains(REFUSED),
                Ok(applied) => {
                    !stored_compact
                        && *applied > 0
                        && replica_state.store().current_revision()
                            == primary_state.store().current_revision()
                }
            },
            format!("{round:?}"),
        );

        if stored_compact {
            let follower = replication::follow(replica_state.clone())?;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            let mut reported = last_error(&replica_state).await?;
            while reported.is_none() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                reported = last_error(&replica_state).await?;
            }
            follower.stop().await;
            check(
                "/replication/status",
                reported
                    .as_deref()
                    .is_some_and(|error| error.contains(REFUSED)),
                format!("last_error {reported:?}"),
            );
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
    Ok(())
}

/// Spec (the replica's fail-closed form, half page point 1; master 14:38): a directory
/// started once as a primary (reasoned at its start, as `serve` does, so its marker vouches
/// for that reasoning) and then started as a replica of a primary at the same revision,
/// which applies the primary's records and restarts, doesn't vouch for what it applied: a
/// reasoned read after the restart gives the primary's rows and says `not-certified`
/// ("every reasoned read on a replica says `not-certified`"), where the primary's read,
/// certified, says its regime's status (under `rdfs` never `complete`, so that is no test).
/// Before that, it is a working replica: caught up, with the primary's rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_once_a_primary_does_not_vouch_for_records_it_applied() -> TestResult<()> {
    let rdfs = ReasonerConfig::for_mode(ReasoningMode::Rdfs)
        .materialised_program()
        .expect("RDFS reasons");
    let query = "SELECT ?x WHERE { ?x a ex:Animal } ORDER BY ?x";
    // Both directories start alike, as `serve` starts a store with reasoning: reasoned.
    let primary_dir = tempfile::tempdir()?;
    let primary_store = StoreService::new(StoreConfig::on_disk(primary_dir.path()))?;
    primary_store.rematerialise(&rdfs)?;
    let primary_state = server(primary_store, ReasoningMode::Rdfs, primary());
    let base = serve(&primary_state).await?;
    let replica_dir = tempfile::tempdir()?;
    {
        let once_a_primary = StoreService::new(StoreConfig::on_disk(replica_dir.path()))?;
        once_a_primary.rematerialise(&rdfs)?;
    }
    let open = || -> TestResult<AppState> {
        Ok(server(
            StoreService::new(as_replica(StoreConfig::on_disk(replica_dir.path())))?,
            ReasoningMode::Rdfs,
            replica_of(&base),
        ))
    };

    let replica_state = open()?;
    assert_eq!(
        replica_state.store().current_revision(),
        primary_state.store().current_revision(),
        "the precondition: both directories at one revision before any record"
    );
    update(
        &base,
        "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog }",
    )
    .await;
    let client = reqwest::Client::new();
    while replication::pull_once(&client, &base, &replica_state).await? > 0 {}
    let (want, certified) = answered(build_app(primary_state.clone()), query).await?;
    assert!(
        want.contains("example.com/rex\""),
        "the primary's rows: {want}"
    );
    assert!(
        certified.as_deref().is_some_and(|s| s != "not-certified"),
        "the precondition: the primary's read is certified: {certified:?}"
    );
    let (rows, _) = answered(build_app(replica_state.clone()), query).await?;
    assert_eq!(rows, want, "the precondition: the replica caught up");
    drop(replica_state);

    let reopened = open()?;
    let (rows, status) = answered(build_app(reopened.clone()), query).await?;
    assert_eq!(rows, want, "the reopened replica's rows");
    assert_eq!(
        status.as_deref(),
        Some("not-certified"),
        "the reopened replica vouches for records it applied"
    );
    Ok(())
}

/// Spec (the replica's fail-closed form, half page point 1; master 14:38): under `owl2-rl`,
/// a replica over the data of its image and of a commit after it answers each read with its
/// primary's rows, and says `not-certified` where the primary gives a status ("every
/// reasoned read on a replica says `not-certified`"). The precondition: the primary's read
/// of an inferred statement is certified (`sound-only` under `owl2-rl`). The wrong reads are
/// listed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_s_reads_under_owl2_rl_say_not_certified() -> TestResult<()> {
    let (primary_state, replica_state, _dirs) = primary_and_replica(
        ReasoningMode::Owl2Rl,
        |dir| StoreConfig {
            ql_rewriting: nrese_store::QlRewritingMode::Off,
            ..StoreConfig::on_disk(dir)
        },
        "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog }",
        "INSERT DATA { ex:Cat rdfs:subClassOf ex:Animal }",
    )
    .await?;
    let (primary_app, replica_app) = (build_app(primary_state), build_app(replica_state));
    let (_, status) = answered(primary_app.clone(), "ASK { ex:rex a ex:Animal }").await?;
    assert!(
        status
            .as_deref()
            .is_some_and(|s| s.starts_with("sound-only; regime=owl2-rl")),
        "the precondition: the primary's inferred read is certified: {status:?}"
    );
    let mut wrong = Vec::new();
    for query in [
        "ASK { ex:Dog ?p ex:Animal }",
        "ASK { ex:Dog rdfs:subClassOf ex:Animal }",
        "ASK { ex:rex a ex:Animal }",
    ] {
        let want = answered(primary_app.clone(), query).await?;
        let got = answered(replica_app.clone(), query).await?;
        let status = want.1.as_ref().map(|_| "not-certified".to_owned());
        if got != (want.0.clone(), status) {
            wrong.push(format!(
                "{query}: the primary {want:?}, the replica {got:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// Spec (half page points 1 and 2): with the QL rewriting on (`owl2-ql`, where `auto`
/// switches it on, and `owl2-rl` with it on), the members of an existential no rule
/// materialises get their rows on the primary only through the rewriting, which rides on
/// the reasoning certificate. A replica over the data of its image and of a commit after it
/// gives no row its primary doesn't (fewer are allowed) and says `not-certified`. The
/// precondition: the primary gives both members.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_through_the_ql_rewriting_says_not_certified() -> TestResult<()> {
    use nrese_store::QlRewritingMode::{Auto, On};
    let query = "SELECT ?x WHERE { ?x ex:worksFor ?y } ORDER BY ?x";
    let (mut unmet, mut wrong) = (Vec::new(), Vec::new());
    for (label, mode, ql) in [
        ("owl2-ql, auto", ReasoningMode::Owl2Ql, Auto),
        ("owl2-rl, on", ReasoningMode::Owl2Rl, On),
    ] {
        let (primary_state, replica_state, _dirs) = primary_and_replica(
            mode,
            |dir| StoreConfig {
                ql_rewriting: ql,
                ..StoreConfig::on_disk(dir)
            },
            "INSERT DATA { ex:Employee rdfs:subClassOf [ a owl:Restriction ; \
             owl:onProperty ex:worksFor ; owl:someValuesFrom owl:Thing ] . ex:bob a ex:Employee }",
            "INSERT DATA { ex:alice a ex:Employee }",
        )
        .await?;
        let (rows, _) = answered(build_app(primary_state), query).await?;
        let want = bound(&rows);
        if !["alice", "bob"]
            .iter()
            .all(|x| want.contains(&format!("http://example.com/{x}")))
        {
            unmet.push(format!("{label}: the primary's rows: {rows}"));
            continue;
        }
        let (replica_rows, status) = answered(build_app(replica_state), query).await?;
        if !bound(&replica_rows).iter().all(|x| want.contains(x))
            || status.as_deref() != Some("not-certified")
        {
            wrong.push(format!(
                "{label}: the primary {rows}, the replica {replica_rows} {status:?}"
            ));
        }
    }
    assert!(unmet.is_empty(), "{unmet:#?}");
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// Spec (master 18:14, Q2): a primary whose served version is stored compact
/// (`reasoner.equality = "compact"` under `owl2-rl`, which closes equality) refuses a
/// replica its image as it refuses its log: 409, with the sentence. The refusal is the
/// primary's state, not a failure of the server (500).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compact_primary_refuses_its_image_with_409_and_the_sentence() -> TestResult<()> {
    let primary_dir = tempfile::tempdir()?;
    let primary_state = server(
        StoreService::new(StoreConfig {
            equality_by_representatives: true,
            equality_compact: true,
            ..StoreConfig::on_disk(primary_dir.path())
        })?,
        ReasoningMode::Owl2Rl,
        primary(),
    );
    let base = serve(&primary_state).await?;
    update(
        &base,
        "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:a a ex:Dog . \
         ex:a owl:sameAs ex:b }",
    )
    .await;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/replication/image"))
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    println!("CASE image: {status} {text}");
    assert!(text.contains(REFUSED), "the sentence: {status} {text}");
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{text}");
    Ok(())
}

/// Spec (master 18:25 (c)): a directory once started as a primary (its marker vouching for
/// its reasoning), then a replica that applied its primary's records, started again as a
/// store of its own (the promotion docs/ops/replication.md describes) is not current under
/// its rules: its start reasons again, and no marker made before the records vouches for
/// them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_promoted_replica_is_not_current_until_it_reasons() -> TestResult<()> {
    let rdfs = ReasonerConfig::for_mode(ReasoningMode::Rdfs)
        .materialised_program()
        .expect("RDFS reasons");
    let primary_dir = tempfile::tempdir()?;
    let primary_store = StoreService::new(StoreConfig::on_disk(primary_dir.path()))?;
    primary_store.rematerialise(&rdfs)?;
    let base = serve(&server(primary_store, ReasoningMode::Rdfs, primary())).await?;
    let replica_dir = tempfile::tempdir()?;
    {
        let once_a_primary = StoreService::new(StoreConfig::on_disk(replica_dir.path()))?;
        once_a_primary.rematerialise(&rdfs)?;
        assert!(
            once_a_primary.reasoning_is_current(rdfs.clone()),
            "the precondition: its marker vouches for its reasoning"
        );
    }
    {
        let replica_state = server(
            StoreService::new(as_replica(StoreConfig::on_disk(replica_dir.path())))?,
            ReasoningMode::Rdfs,
            replica_of(&base),
        );
        update(
            &base,
            "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog }",
        )
        .await;
        let client = reqwest::Client::new();
        let mut applied = 0;
        loop {
            match replication::pull_once(&client, &base, &replica_state).await? {
                0 => break,
                n => applied += n,
            }
        }
        assert!(
            applied > 0,
            "the precondition: the replica applied its primary's records"
        );
    }
    let promoted = StoreService::new(StoreConfig::on_disk(replica_dir.path()))?;
    assert!(
        !promoted.reasoning_is_current(rdfs),
        "the promoted directory's marker vouches for the records it applied"
    );
    Ok(())
}

/// The data the started directories are given: `ex:rex a ex:Animal` is inferred under `rdfs`.
const ANIMALS: &str = "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog }";

/// The reasoned read the started stores are asked; its one row is inferred.
const ANIMAL_QUERY: &str = "PREFIX ex: <http://example.com/> SELECT ?x WHERE { ?x a ex:Animal }";

/// A data directory as this build's server leaves it under `rdfs`, in `dir`: started as a
/// primary (its start reasons and writes the reasoning marker), given [`ANIMALS`], stopped.
/// Its configuration file, its data directory, and what its [`ANIMAL_QUERY`] read gave.
async fn reasoned_directory(
    dir: &Path,
) -> TestResult<(PathBuf, PathBuf, Vec<String>, Option<String>)> {
    let data = dir.join("data");
    std::fs::create_dir_all(&data)?;
    let file = configure(dir, &data)?;
    let owl2_rl = "[reasoner]\nmode = \"owl2-rl\"\n";
    let text = std::fs::read_to_string(&file)?;
    if !text.contains(owl2_rl) {
        return Err(format!("the configuration has no {owl2_rl:?}:\n{text}").into());
    }
    std::fs::write(
        &file,
        text.replace(owl2_rl, "[reasoner]\nmode = \"rdfs\"\n"),
    )?;
    let primary = Server::start(&file).await?;
    primary
        .update("nrese", &format!("{PREFIXES}{ANIMALS}"))
        .await?;
    let (rows, status) = primary.query_status("nrese", ANIMAL_QUERY).await?;
    drop(primary);
    Ok((file, data, rows, status))
}

/// The result cache's hits, as `/api/v1/repositories/nrese/cache` counts them.
async fn cache_hits(server: &Server) -> TestResult<u64> {
    let view: serde_json::Value =
        serde_json::from_str(&server.get("/api/v1/repositories/nrese/cache").await?)?;
    view["hits"]
        .as_u64()
        .ok_or_else(|| format!("no hits in {view}").into())
}

/// The marker `text` with its fingerprint changed: one of another semantics.
fn other_semantics(text: &str) -> String {
    text.lines()
        .map(|line| {
            match line
                .strip_prefix("fingerprint ")
                .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            {
                Some(fingerprint) => format!("fingerprint {:016x}\n", fingerprint ^ 1),
                None => format!("{line}\n"),
            }
        })
        .collect()
}

/// Spec (master 18:18 and 18:25): a replica started by this build's server over a
/// directory holding a reasoning marker takes no certificate from it. Its reasoned read,
/// before it applies any batch (its primary is unreachable), gives the rows as stored and
/// says `not-certified`. Two markers: the one this build wrote (`as accepted`), and one of
/// another semantics (`after an upgrade`: its fingerprint changed). Then a query pinned at
/// the replica's open (`NRESE_PINNED_QUERIES`, `pinned`), answered from the result cache,
/// says `not-certified` too, as a fresh read does (`pinned, fresh`). The preconditions: the
/// directory's own read as a primary is certified (`sound-only` under `rdfs`), its marker
/// is in place, and the pinned answer comes from the cache. Each case is printed.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_started_over_a_marker_says_not_certified() -> TestResult<()> {
    let (mut wrong, mut unmet) = (Vec::new(), Vec::new());
    for case in ["as accepted", "after an upgrade", "pinned"] {
        let dir = tempfile::tempdir()?;
        let (file, data, want, certified) = reasoned_directory(dir.path()).await?;
        if !certified
            .as_deref()
            .is_some_and(|s| s.starts_with("sound-only"))
        {
            unmet.push(format!(
                "{case}: the primary's read is certified: {certified:?}"
            ));
            continue;
        }
        let marker = data.join("reasoning.state");
        let Ok(text) = std::fs::read_to_string(&marker) else {
            unmet.push(format!("{case}: the directory holds its marker"));
            continue;
        };
        if case == "after an upgrade" {
            let changed = other_semantics(&text);
            if changed == text {
                unmet.push(format!(
                    "{case}: the marker names its fingerprint: {text:?}"
                ));
                continue;
            }
            std::fs::write(&marker, changed)?;
        }
        let replica = std::fs::read_to_string(&file)?
            + "[replication]\nmode = \"replica\"\nprimary = \"http://127.0.0.1:9\"\n";
        std::fs::write(&file, replica)?;
        let pinned = dir.path().join("animals.rq");
        std::fs::write(&pinned, ANIMAL_QUERY)?;
        let pinned = pinned.display().to_string();
        let env = match case {
            "pinned" => vec![("NRESE_PINNED_QUERIES", pinned.as_str())],
            _ => Vec::new(),
        };
        let server = Server::start_with(&file, &env).await?;
        let before = cache_hits(&server).await?;
        let (rows, status) = server.query_status("nrese", ANIMAL_QUERY).await?;
        let after = cache_hits(&server).await?;
        if case == "pinned" && after <= before {
            unmet.push(format!(
                "{case}: the answer came from the cache: hits {before} -> {after}"
            ));
        }
        let right = rows == want && status.as_deref() == Some("not-certified");
        println!(
            "CASE {case}: {} {status:?} {rows:?}",
            if right { "right" } else { "WRONG" }
        );
        if !right {
            wrong.push(format!("{case}: {status:?} {rows:?}, want {want:?}"));
        }
        if case == "pinned" {
            let fresh = ANIMAL_QUERY.replace("?x", "?y");
            let (_, status) = server.query_status("nrese", &fresh).await?;
            let right = status.as_deref() == Some("not-certified");
            println!(
                "CASE pinned, fresh: {} {status:?}",
                if right { "right" } else { "WRONG" }
            );
            if !right {
                wrong.push(format!("pinned, fresh: {status:?}"));
            }
        }
    }
    for line in &unmet {
        eprintln!("UNMET {line}");
    }
    assert!(unmet.is_empty(), "{unmet:#?}");
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// The result cache's hits, as `state`'s `/api/v1/repositories/nrese/cache` counts them.
async fn state_hits(state: &AppState) -> TestResult<u64> {
    let response = build_app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/repositories/nrese/cache")
                .body(Body::empty())?,
        )
        .await?;
    let view: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    view["hits"]
        .as_u64()
        .ok_or_else(|| format!("no hits in {view}").into())
}

/// What `with_replication` says when the role it is given isn't its store's.
const OTHER_ROLE: &str = "the store's role";

/// Spec (master 19:37, S1): a server state's role in replication is its store's, from the
/// store's open (`StoreConfig::replica`), checked at construction in both directions. Over a
/// directory whose marker vouches for its `rdfs` reasoning, with an unreachable primary and
/// before any batch: a store opened as a primary built into a replica's state is refused, as
/// a store opened as a replica built into a primary's is; a store opened as a replica built
/// into a replica's state reads [`ANIMAL_QUERY`]'s stored rows, says `not-certified`, fresh
/// and from the result cache, and its consistency is `Unknown`. The precondition: the
/// directory's marker is current for `rdfs`. Each case is printed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_state_takes_its_role_in_replication_from_its_store_s_open() -> TestResult<()> {
    let rdfs = ReasonerConfig::for_mode(ReasoningMode::Rdfs)
        .materialised_program()
        .expect("RDFS reasons");
    let dir = tempfile::tempdir()?;
    {
        let store = StoreService::new(StoreConfig::on_disk(dir.path()))?;
        nrese_store::MutationPipeline::new(
            std::sync::Arc::new(store.clone()),
            std::sync::Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Rdfs,
            ))),
        )
        .execute_update(&nrese_store::SparqlUpdateRequest::new(format!(
            "{PREFIXES}{ANIMALS}"
        )))?;
        store.rematerialise(&rdfs)?;
        assert!(
            store.reasoning_is_current(rdfs.clone()),
            "the precondition: the marker vouches for the reasoning"
        );
    }
    let opened = |replica: bool| -> TestResult<StoreService> {
        Ok(StoreService::new(StoreConfig {
            replica,
            ..StoreConfig::on_disk(dir.path())
        })?)
    };
    let mut wrong = Vec::new();
    for (case, replica, role) in [
        (
            "opened as a primary, built as a replica",
            false,
            replica_of("http://127.0.0.1:9"),
        ),
        ("opened as a replica, built as a primary", true, primary()),
    ] {
        let store = opened(replica)?;
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            server(store, ReasoningMode::Rdfs, role)
        }));
        match built {
            Err(panic) => {
                let text = panic.downcast_ref::<String>().cloned().unwrap_or_default();
                let right = text.contains(OTHER_ROLE);
                println!(
                    "CASE {case}: {} refused: {text:?}",
                    if right { "right" } else { "WRONG" }
                );
                if !right {
                    wrong.push(format!("{case}: refused otherwise: {text:?}"));
                }
            }
            Ok(state) => {
                let (rows, status) = answered(build_app(state.clone()), ANIMAL_QUERY).await?;
                let consistency = state.store().consistency();
                println!("CASE {case}: WRONG built: {status:?} {consistency:?} {rows}");
                wrong.push(format!("{case}: built: {status:?} {consistency:?}"));
            }
        }
    }
    let state = server(
        opened(true)?,
        ReasoningMode::Rdfs,
        replica_of("http://127.0.0.1:9"),
    );
    let (rows, status) = answered(build_app(state.clone()), ANIMAL_QUERY).await?;
    let before = state_hits(&state).await?;
    let (cached, again) = answered(build_app(state.clone()), ANIMAL_QUERY).await?;
    let after = state_hits(&state).await?;
    assert!(
        after > before,
        "the precondition: the second answer came from the cache: hits {before} -> {after}"
    );
    let consistency = state.store().consistency();
    let right = rows.contains("example.com/rex\"")
        && cached == rows
        && status.as_deref() == Some("not-certified")
        && again.as_deref() == Some("not-certified")
        && consistency == nrese_store::ConsistencyStatus::Unknown;
    println!(
        "CASE opened as a replica, built as one: {} {status:?} {again:?} {consistency:?} {rows}",
        if right { "right" } else { "WRONG" }
    );
    if !right {
        wrong.push(format!(
            "built as one: {status:?} {again:?} {consistency:?} {rows}"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// Spec (review r2, S2): the store a replica's state was built over and checked
/// ([`a_state_takes_its_role_in_replication_from_its_store_s_open`]) stays its store. Before any
/// batch, with an unreachable primary, its default repository's write path replaced from
/// outside by one over a store opened as a primary, whose marker is current for `rdfs`: its
/// reasoned read of [`ANIMAL_QUERY`] says `not-certified`, and its consistency is `Unknown`.
/// Sol's witness: red while the slot can be written from outside; once it can't, this no
/// longer compiles and leaves with the fix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_s_write_path_is_not_replaced_from_outside() -> TestResult<()> {
    let rdfs = ReasonerConfig::for_mode(ReasoningMode::Rdfs)
        .materialised_program()
        .expect("RDFS reasons");
    let marked = tempfile::tempdir()?;
    let pipeline = nrese_store::MutationPipeline::new(
        std::sync::Arc::new(StoreService::new(StoreConfig::on_disk(marked.path()))?),
        std::sync::Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Rdfs,
        ))),
    );
    pipeline.execute_update(&nrese_store::SparqlUpdateRequest::new(format!(
        "{PREFIXES}{ANIMALS}"
    )))?;
    pipeline.store().rematerialise(&rdfs)?;
    assert!(
        pipeline.store().reasoning_is_current(rdfs),
        "the precondition: the marker vouches for the reasoning"
    );
    let replica_dir = tempfile::tempdir()?;
    let state = server(
        StoreService::new(as_replica(StoreConfig::on_disk(replica_dir.path())))?,
        ReasoningMode::Rdfs,
        replica_of("http://127.0.0.1:9"),
    );
    *state.repositories().default_repository().pipeline.write() = std::sync::Arc::new(pipeline);
    let (rows, status) = answered(build_app(state.clone()), ANIMAL_QUERY).await?;
    let consistency = state.store().consistency();
    let right = status.as_deref() == Some("not-certified")
        && consistency == nrese_store::ConsistencyStatus::Unknown;
    println!(
        "CASE a replica's write path replaced: {} {status:?} {consistency:?} {rows}",
        if right { "right" } else { "WRONG" }
    );
    assert!(
        right,
        "the replaced write path's store answers: {status:?} {consistency:?}"
    );
    Ok(())
}
