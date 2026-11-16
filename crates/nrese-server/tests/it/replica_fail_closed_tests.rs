//! A read replica never reads more than it can show (the fail-closed form,
//! docs/ops/replication.md): a primary whose rows are stored compact serves it nothing,
//! and the replica says why; a directory once started as a primary doesn't vouch for the
//! records it applied as a replica.

use axum::body::Body;
use axum::http::{Method, Request};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::replication::{self, ReplicationConfig, ReplicationMode};
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

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
    let replica_state = server(StoreService::new(config(dirs[1].path()))?, mode, following);
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
            StoreService::new(compact(replica_dir.path()))?,
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
            StoreService::new(StoreConfig::on_disk(replica_dir.path()))?,
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
