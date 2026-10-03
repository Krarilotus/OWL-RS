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
