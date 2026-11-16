//! Read replicas over HTTP (`replication.mode`): a replica started from its primary's
//! image follows the primary's log and answers as the primary does, inferences
//! included; it takes no writes; its status tells how far behind it is; only a primary
//! serves its log.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::replication::{self, ReplicationConfig, ReplicationMode};
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

use crate::support::{body_text, query_text};

const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

fn state(store: StoreService, replication: ReplicationConfig) -> AppState {
    let state = AppState::new(
        store,
        ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Rdfs)),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(replication);
    state.mark_ready();
    state
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

/// Pulls until the replica has caught up.
async fn catch_up(primary: &str, replica: &AppState) {
    let client = reqwest::Client::new();
    while replication::pull_once(&client, primary, replica)
        .await
        .expect("pull")
        > 0
    {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_follows_its_primary() -> Result<(), Box<dyn std::error::Error>> {
    // The primary, on a real port, with RDFS reasoning.
    let primary_dir = tempfile::tempdir()?;
    let store = StoreService::new(StoreConfig::on_disk(primary_dir.path()))?;
    let program = ReasonerConfig::for_mode(ReasoningMode::Rdfs)
        .materialised_program()
        .expect("RDFS reasons");
    store.rematerialise(&program)?;
    let primary_state = state(
        store,
        ReplicationConfig {
            mode: ReplicationMode::Primary,
            ..ReplicationConfig::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let primary_app = build_app(primary_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, primary_app).await.expect("primary");
    });
    update(
        &base,
        "INSERT DATA { ex:a a ex:C . ex:C rdfs:subClassOf ex:D }",
    )
    .await;

    // The replica starts from the primary's image.
    let replica_dir = tempfile::tempdir()?;
    let config = ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(base.clone()),
        poll: std::time::Duration::from_millis(20),
        ..ReplicationConfig::default()
    };
    let revision = replication::bootstrap(&config, replica_dir.path())
        .await?
        .expect("an empty data directory takes the image");
    assert_eq!(revision, primary_state.store().current_revision());
    // A data directory holding a store keeps it.
    assert_eq!(
        replication::bootstrap(&config, replica_dir.path()).await?,
        None
    );
    let replica_state = state(
        StoreService::new(StoreConfig::on_disk(replica_dir.path()))?,
        config,
    );
    let replica_app = build_app(replica_state.clone());

    // Commits on the primary reach the replica, inferences with them.
    update(
        &base,
        "INSERT DATA { ex:b a ex:D . ex:D rdfs:subClassOf ex:E }",
    )
    .await;
    update(&base, "DELETE DATA { ex:a a ex:C }").await;
    update(&base, "INSERT DATA { GRAPH ex:g { ex:c a ex:C } }").await;
    catch_up(&base, &replica_state).await;
    assert_eq!(
        replica_state.store().current_revision(),
        primary_state.store().current_revision()
    );
    let primary_app = build_app(primary_state.clone());
    for query in [
        "SELECT ?x ?t WHERE { ?x a ?t } ORDER BY ?x ?t",
        "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
        "SELECT ?g ?x WHERE { GRAPH ?g { ?x a ex:C } } ORDER BY ?g ?x",
    ] {
        let query = format!("{PREFIXES}{query}");
        let (want, got) = (
            query_text(primary_app.clone(), &query).await?,
            query_text(replica_app.clone(), &query).await?,
        );
        assert_eq!(got, want, "{query}");
    }
    let inferred = query_text(
        replica_app.clone(),
        &format!("{PREFIXES}ASK {{ ex:b a ex:E }}"),
    )
    .await?;
    assert!(
        inferred.contains("true"),
        "the primary's inference: {inferred}"
    );

    // The replica takes no writes.
    let response = replica_app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(format!(
                    "{PREFIXES}INSERT DATA {{ ex:z a ex:Z }}"
                )))?,
        )
        .await?;
    assert!(!response.status().is_success(), "{}", response.status());

    // Its status: caught up; only a primary serves the log.
    let response = replica_app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/replication/status")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let status: replication::ReplicationStatus = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(status.mode, "replica");
    assert_eq!(status.lag_revisions, Some(0));
    assert!(status.records_applied >= 3, "{status:?}");
    let response = replica_app
        .oneshot(
            Request::builder()
                .uri("/api/v1/replication/log?after=0")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // The replica's store survives a restart at the revision it reached.
    let reached = replica_state.store().current_revision();
    drop(replica_state);

    let reopened = StoreService::new(StoreConfig::on_disk(replica_dir.path()))?;
    assert_eq!(reopened.current_revision(), reached);
    Ok(())
}

/// A query's body and its completeness header.
async fn answered(
    app: axum::Router,
    query: &str,
) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .header("accept", "application/sparql-results+json")
                .body(Body::from(query.to_owned()))?,
        )
        .await?;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let status = header("nrese-completeness");
    Ok((body_text(response).await?, status))
}

/// Under `owl2-dl` a replica gives its primary's answers, `not-certified`: no certificate
/// comes with the records it applies. It builds the bounds from the records, interning no
/// term (its dictionary continues the primary's, which interns the bounds' terms in its
/// commits).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_gives_its_primarys_dl_answers() -> Result<(), Box<dyn std::error::Error>> {
    let dl = || ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Owl2Dl));
    let primary_dir = tempfile::tempdir()?;
    let primary_state = AppState::new(
        StoreService::new(StoreConfig::on_disk(primary_dir.path()))?,
        dl(),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(ReplicationConfig {
        mode: ReplicationMode::Primary,
        ..ReplicationConfig::default()
    });
    primary_state.mark_ready();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let primary_app = build_app(primary_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, primary_app).await.expect("primary");
    });
    let owl = "PREFIX owl: <http://www.w3.org/2002/07/owl#> ";
    update(
        &base,
        &format!(
            "{owl}INSERT DATA {{ ex:A rdfs:subClassOf [ owl:unionOf ( ex:B ex:C ) ] . \
             ex:B rdfs:subClassOf ex:D . ex:C rdfs:subClassOf ex:D . ex:x a ex:A }}"
        ),
    )
    .await;

    let replica_dir = tempfile::tempdir()?;
    let config = ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(base.clone()),
        poll: std::time::Duration::from_millis(20),
        ..ReplicationConfig::default()
    };
    replication::bootstrap(&config, replica_dir.path()).await?;
    let replica_state = AppState::new(
        StoreService::new(StoreConfig::on_disk(replica_dir.path()))?,
        dl(),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(config);
    replica_state.mark_ready();
    assert!(replica_state.store().dl().replica());
    // Commits after the image: an assertion of a union, and new schema (a new U1).
    update(
        &base,
        &format!("{owl}INSERT DATA {{ ex:w a [ owl:unionOf ( ex:B ex:C ) ] }}"),
    )
    .await;
    update(
        &base,
        "INSERT DATA { ex:D rdfs:subClassOf ex:E . ex:y a ex:B }",
    )
    .await;
    catch_up(&base, &replica_state).await;
    let replica_app = build_app(replica_state.clone());
    let primary_app = build_app(primary_state.clone());
    for query in [
        "SELECT ?x WHERE { ?x a ex:E } ORDER BY ?x",
        "SELECT ?x WHERE { ?x a ex:C } ORDER BY ?x",
        "SELECT ?x WHERE { { ?x a ex:D } UNION { ?x a ex:C } } ORDER BY ?x",
    ] {
        let query = format!("{PREFIXES}{query}");
        let want = answered(primary_app.clone(), &query).await?;
        let got = answered(replica_app.clone(), &query).await?;
        assert_eq!(got.0, want.0, "{query}");
        assert!(want.1.is_some(), "a status: {query}");
    }
    let (text, status) = answered(
        replica_app.clone(),
        &format!("{PREFIXES}SELECT ?x WHERE {{ ?x a ex:E }}"),
    )
    .await?;
    assert!(
        status
            .as_deref()
            .is_some_and(|s| s.starts_with("not-certified")),
        "{status:?}"
    );
    for x in ["x", "w", "y"] {
        assert!(text.contains(&format!("example.com/{x}")), "{text}");
    }
    // More commits still apply: the replica interned nothing of its own.
    update(&base, "INSERT DATA { ex:z a ex:B }").await;
    catch_up(&base, &replica_state).await;
    assert_eq!(
        replica_state.store().current_revision(),
        primary_state.store().current_revision()
    );
    // At one revision, one U1: the replica's, built from the records, is the primary's,
    // maintained by its commits.
    let (primary, replica) = (primary_state.store(), replica_state.store());
    assert_eq!(replica.dl_bounds().revision, replica.current_revision());
    assert_eq!(
        replica.dl_upper_facts(false).expect("the replica's U1"),
        primary.dl_upper_facts(false).expect("the primary's U1")
    );
    Ok(())
}

/// Under `owl2-rl`, a replica gives its primary's status, over the data of its image and of
/// a commit after it. A read of the inferred statements is `sound-only` on the primary
/// (the rules' closure, not every certain answer under OWL 2 DL), so the replica's answer
/// may not say `complete`. Every read is checked; the wrong ones are listed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_gives_its_primarys_status_under_owl2_rl()
-> Result<(), Box<dyn std::error::Error>> {
    let rl = || ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Owl2Rl));
    let config = |dir: &std::path::Path| StoreConfig {
        ql_rewriting: nrese_store::QlRewritingMode::Off,
        ..StoreConfig::on_disk(dir)
    };
    let primary_dir = tempfile::tempdir()?;
    let primary_state = AppState::new(
        StoreService::new(config(primary_dir.path()))?,
        rl(),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(ReplicationConfig {
        mode: ReplicationMode::Primary,
        ..ReplicationConfig::default()
    });
    primary_state.mark_ready();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let primary_app = build_app(primary_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, primary_app).await.expect("primary");
    });
    update(
        &base,
        "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog }",
    )
    .await;

    let replica_dir = tempfile::tempdir()?;
    let replication = ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(base.clone()),
        poll: std::time::Duration::from_millis(20),
        ..ReplicationConfig::default()
    };
    replication::bootstrap(&replication, replica_dir.path()).await?;
    let replica_state = AppState::new(
        StoreService::new(config(replica_dir.path()))?,
        rl(),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(replication);
    replica_state.mark_ready();
    update(&base, "INSERT DATA { ex:Cat rdfs:subClassOf ex:Animal }").await;
    catch_up(&base, &replica_state).await;
    let (replica_app, primary_app) = (
        build_app(replica_state.clone()),
        build_app(primary_state.clone()),
    );
    let inferred = format!("{PREFIXES}ASK {{ ex:rex a ex:Animal }}");
    let (_, status) = answered(primary_app.clone(), &inferred).await?;
    assert!(
        status
            .as_deref()
            .is_some_and(|s| s.starts_with("sound-only; regime=owl2-rl")),
        "the primary's status of an inferred read: {status:?}"
    );
    let mut wrong = Vec::new();
    for query in [
        "ASK { ex:Dog ?p ex:Animal }",
        "ASK { ex:Dog rdfs:subClassOf ex:Animal }",
        "ASK { ex:rex a ex:Animal }",
    ] {
        let query = format!("{PREFIXES}{query}");
        let want = answered(primary_app.clone(), &query).await?;
        let got = answered(replica_app.clone(), &query).await?;
        if got != want {
            wrong.push(format!(
                "{query}: the primary {want:?}, the replica {got:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// A replica of a primary that keeps its closure over representatives
/// (`reasoner.equality = "compact"`) answers with its primary's rows: a read expands each
/// `owl:sameAs` class, on the replica as on the primary, over the data of its image and of a
/// commit applied after it. Both stores are configured alike. The rows are checked first,
/// the status second.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_of_a_compact_primary_gives_its_primarys_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let rl = || ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Owl2Rl));
    let config = |dir: &std::path::Path| StoreConfig {
        equality_by_representatives: true,
        equality_compact: true,
        ..StoreConfig::on_disk(dir)
    };
    let primary_dir = tempfile::tempdir()?;
    let primary_state = AppState::new(
        StoreService::new(config(primary_dir.path()))?,
        rl(),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(ReplicationConfig {
        mode: ReplicationMode::Primary,
        ..ReplicationConfig::default()
    });
    primary_state.mark_ready();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let primary_app = build_app(primary_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, primary_app).await.expect("primary");
    });
    let owl = "PREFIX owl: <http://www.w3.org/2002/07/owl#> ";
    update(
        &base,
        &format!(
            "{owl}INSERT DATA {{ ex:Dog rdfs:subClassOf ex:Animal . ex:a a ex:Dog . \
             ex:a owl:sameAs ex:b }}"
        ),
    )
    .await;

    let replica_dir = tempfile::tempdir()?;
    let replication = ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(base.clone()),
        poll: std::time::Duration::from_millis(20),
        ..ReplicationConfig::default()
    };
    replication::bootstrap(&replication, replica_dir.path()).await?;
    let replica_state = AppState::new(
        StoreService::new(config(replica_dir.path()))?,
        rl(),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(replication);
    replica_state.mark_ready();
    update(
        &base,
        &format!("{owl}INSERT DATA {{ ex:c owl:sameAs ex:a }}"),
    )
    .await;
    catch_up(&base, &replica_state).await;
    let query = format!("{PREFIXES}SELECT ?x WHERE {{ ?x a ex:Animal }} ORDER BY ?x");
    let (rows, status) = answered(build_app(primary_state.clone()), &query).await?;
    for x in ["a", "b", "c"] {
        assert!(
            rows.contains(&format!("example.com/{x}\"")),
            "the primary's rows: {rows}"
        );
    }
    let (replica_rows, replica_status) = answered(build_app(replica_state.clone()), &query).await?;
    assert_eq!(replica_rows, rows, "the replica's rows");
    assert_eq!(replica_status, status, "the replica's status");
    Ok(())
}

/// With the QL rewriting on (`owl2-ql`, where `auto` switches it on, and `owl2-rl` with it
/// on), a replica answers through it as its primary does, over the data of its image and of
/// a commit applied after it: an existential no rule materialises gives each member of its
/// class a row only through the rewriting. Each row's primary must give those rows first;
/// then the replica's rows, and its status, must equal the primary's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replica_answers_through_the_ql_rewriting_as_its_primary()
-> Result<(), Box<dyn std::error::Error>> {
    use nrese_store::QlRewritingMode::{Auto, On};
    let owl = "PREFIX owl: <http://www.w3.org/2002/07/owl#> ";
    let query = format!("{PREFIXES}SELECT ?x WHERE {{ ?x ex:worksFor ?y }} ORDER BY ?x");
    let (mut unmet, mut wrong) = (Vec::new(), Vec::new());
    for (label, mode, ql) in [
        ("owl2-ql, auto", ReasoningMode::Owl2Ql, Auto),
        ("owl2-rl, on", ReasoningMode::Owl2Rl, On),
    ] {
        let reasoner = || ReasonerService::new(ReasonerConfig::for_mode(mode));
        let config = |dir: &std::path::Path| StoreConfig {
            ql_rewriting: ql,
            ..StoreConfig::on_disk(dir)
        };
        let primary_dir = tempfile::tempdir()?;
        let primary_state = AppState::new(
            StoreService::new(config(primary_dir.path()))?,
            reasoner(),
            PolicyConfig::default(),
            AiSuggestionService::disabled(),
            DeploymentPosture::OpenWorkbench,
        )
        .with_replication(ReplicationConfig {
            mode: ReplicationMode::Primary,
            ..ReplicationConfig::default()
        });
        primary_state.mark_ready();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let primary_app = build_app(primary_state.clone());
        tokio::spawn(async move {
            axum::serve(listener, primary_app).await.expect("primary");
        });
        update(
            &base,
            &format!(
                "{owl}INSERT DATA {{ ex:Employee rdfs:subClassOf [ a owl:Restriction ; \
                 owl:onProperty ex:worksFor ; owl:someValuesFrom owl:Thing ] . \
                 ex:bob a ex:Employee }}"
            ),
        )
        .await;

        let replica_dir = tempfile::tempdir()?;
        let replication = ReplicationConfig {
            mode: ReplicationMode::Replica,
            primary: Some(base.clone()),
            poll: std::time::Duration::from_millis(20),
            ..ReplicationConfig::default()
        };
        replication::bootstrap(&replication, replica_dir.path()).await?;
        let replica_state = AppState::new(
            StoreService::new(config(replica_dir.path()))?,
            reasoner(),
            PolicyConfig::default(),
            AiSuggestionService::disabled(),
            DeploymentPosture::OpenWorkbench,
        )
        .with_replication(replication);
        replica_state.mark_ready();
        update(&base, "INSERT DATA { ex:alice a ex:Employee }").await;
        catch_up(&base, &replica_state).await;

        let (rows, status) = answered(build_app(primary_state.clone()), &query).await?;
        if !["alice", "bob"]
            .iter()
            .all(|x| rows.contains(&format!("example.com/{x}\"")))
        {
            unmet.push(format!("{label}: the primary's rows: {rows}"));
            continue;
        }
        let (replica_rows, replica_status) =
            answered(build_app(replica_state.clone()), &query).await?;
        if replica_rows != rows {
            wrong.push(format!(
                "{label}: rows: the primary {rows}, the replica {replica_rows}"
            ));
        }
        if replica_status != status {
            wrong.push(format!(
                "{label}: status: the primary {status:?}, the replica {replica_status:?}"
            ));
        }
    }
    assert!(unmet.is_empty(), "{unmet:#?}");
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// A replica's data directory, made by its bootstrap, is durably named (sol-loop round 3 on
/// DUR-3 review r1, S1): into `sealed/a`, where `sealed` exists but can't be opened (mode
/// 0o311), so its sync, which must name `a`, fails, the bootstrap must refuse. Before the fix it
/// made `a` itself, and the placement then took it for a directory provisioned beforehand. Guard:
/// into `open/a` it takes the image and the store opens with it. Unix only (permissions); as root,
/// where the mode doesn't keep the directory closed, the first row isn't checked.
#[cfg(unix)]
#[tokio::test]
async fn a_bootstrap_names_the_data_directory_it_makes() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let primary_dir = tempfile::tempdir()?;
    let primary_state = state(
        StoreService::new(StoreConfig::on_disk(primary_dir.path()))?,
        ReplicationConfig {
            mode: ReplicationMode::Primary,
            ..ReplicationConfig::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let primary_app = build_app(primary_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, primary_app).await.expect("primary");
    });
    update(&base, "INSERT DATA { ex:a a ex:C }").await;
    let config = ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(base.clone()),
        ..ReplicationConfig::default()
    };
    let replicas = tempfile::tempdir()?;
    let mut wrong = Vec::new();

    let sealed = replicas.path().join("sealed");
    std::fs::create_dir(&sealed)?;
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o311))?;
    if std::fs::read_dir(&sealed).is_ok() {
        eprintln!("row sealed: not checked (the directory opens: root?)");
    } else {
        let taken = replication::bootstrap(&config, &sealed.join("a")).await;
        eprintln!(
            "row sealed: {}",
            if taken.is_err() { "ok" } else { "FAILED" }
        );
        if let Ok(taken) = taken {
            wrong.push(format!(
                "bootstrapped under a directory whose sync fails: {taken:?}"
            ));
        }
    }
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755))?;

    let open = replicas.path().join("open").join("a");
    let taken = replication::bootstrap(&config, &open).await;
    let ok = matches!(taken, Ok(Some(revision)) if revision == primary_state.store().current_revision())
        && StoreService::new(StoreConfig::on_disk(&open))?
            .stats()?
            .quad_count
            > 0;
    eprintln!("row open (guard): {}", if ok { "ok" } else { "FAILED" });
    if !ok {
        wrong.push(format!("guard: bootstrap into open/a: {taken:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}
