//! Reasoning lifecycle over HTTP: a store whose data is inconsistent under its reasoning
//! is quarantined (not ready), but serves reads for diagnosis and writes for repair.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_server::policy::PolicyConfig;
use nrese_store::{MutationPipeline, SparqlUpdateRequest, StoreConfig, StoreService};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store};

#[cfg(feature = "fault-points")]
mod write_program {
    use super::*;
    use crate::support::test_state_with_store_config;
    use nrese_store::mutation::write_program_hooks::{Stage, set};
    use std::sync::mpsc;
    use std::time::Duration;

    // Each hold is owned by the test that creates the server, across the worker's spawn.
    fn hold(stage: Stage) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, seen) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        set(stage, move || {
            entered.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(10));
        });
        (seen, release)
    }

    /// Spec: the serving router answers ready while write preparation is held before
    /// its claim or inside grounding.
    #[tokio::test]
    async fn readiness_does_not_join_write_program_preparation()
    -> Result<(), Box<dyn std::error::Error>> {
        for stage in [Stage::BeforeClaim, Stage::DuringGrounding] {
            let (entered, release) = hold(stage);
            let state = test_state_with_store_config(
                StoreConfig::in_memory(),
                PolicyConfig::default(),
                ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
            )?;
            entered.recv_timeout(Duration::from_secs(10))?;
            let app = nrese_server::app::build_app(state);
            let ready =
                tokio::time::timeout(Duration::from_secs(2), app.oneshot(get("/readyz"))).await??;
            assert_eq!(ready.status(), StatusCode::OK);
            release.send(())?;
        }
        Ok(())
    }

    /// Spec: an update awaiting a preparation past its deadline gets a 408 and
    /// publishes nothing after the preparation resumes.
    #[tokio::test]
    async fn a_write_program_wait_uses_the_http_deadline() -> Result<(), Box<dyn std::error::Error>>
    {
        let (entered, release) = hold(Stage::BeforeInstall);
        let mut policy = PolicyConfig::default();
        policy.timeouts.update = Duration::from_millis(150);
        let state = test_state_with_store_config(
            StoreConfig::in_memory(),
            policy,
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
        )?;
        entered.recv_timeout(Duration::from_secs(10))?;
        let revision = state.store().current_revision();
        let pipeline = state.pipeline();
        let app = nrese_server::app::build_app(state);
        let response = app.oneshot(update(
            "INSERT DATA { <http://example.org/x> <http://example.org/p> <http://example.org/y> }",
        )).await?;
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        release.send(())?;
        tokio::time::timeout(Duration::from_secs(10), async {
            while pipeline.write_program_metrics().0 != 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        assert_eq!(pipeline.store().current_revision(), revision);
        assert_eq!(pipeline.write_program_metrics().2, 0);
        Ok(())
    }
}

const EX: &str = "http://example.com/";

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .method(Method::GET)
        .body(Body::empty())
        .unwrap()
}

fn update(text: &str) -> Request<Body> {
    Request::builder()
        .uri("/dataset/update")
        .method(Method::POST)
        .header("content-type", "application/sparql-update")
        .body(Body::from(text.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn inconsistent_data_is_quarantined_but_readable_and_repairable()
-> Result<(), Box<dyn std::error::Error>> {
    let store = StoreService::new(StoreConfig::in_memory())?;
    // Imported with reasoning off: through a pipeline without a reasoner.
    let import = MutationPipeline::new(
        std::sync::Arc::new(store.clone()),
        std::sync::Arc::new(ReasonerService::new(ReasonerConfig::default())),
    );
    import.execute_update(&SparqlUpdateRequest::new(format!(
        "INSERT DATA {{ <{EX}A> <http://www.w3.org/2002/07/owl#disjointWith> <{EX}B> .
                       <{EX}x> a <{EX}A> , <{EX}B> }}"
    )))?;
    store.rematerialise(Ruleset::Owl2Rl)?;
    let app = test_app_with_store(
        store,
        PolicyConfig::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;

    let ready = app.clone().oneshot(get("/readyz")).await?;
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_text(ready).await?;
    // D16-310: quarantine is a fixed readiness category; diagnostics require /info.
    let public: serde_json::Value = serde_json::from_str(&body)?;
    assert_eq!(public, serde_json::json!({"status": "quarantined"}));
    let info = app.clone().oneshot(get("/dataset/info")).await?;
    assert_eq!(info.status(), StatusCode::OK);
    assert!(body_text(info).await?.contains("\"violations\":1"));

    let query = format!("/dataset/query?query=ASK%20%7B%20%3C{EX}x%3E%20%3Fp%20%3Fo%20%7D");
    let answer = app.clone().oneshot(get(&query)).await?;
    assert_eq!(answer.status(), StatusCode::OK, "reads serve diagnosis");

    let repair = app
        .clone()
        .oneshot(update(&format!("DELETE DATA {{ <{EX}x> a <{EX}B> }}")))
        .await?;
    assert!(repair.status().is_success(), "{}", repair.status());
    let ready = app.oneshot(get("/readyz")).await?;
    assert_eq!(ready.status(), StatusCode::OK);
    Ok(())
}

/// Under `owl2-dl`, a rules clash in an ontology OWL 2 DL reads differently (an annotation
/// property's domain, disjoint with its subject's class) is undecided: quarantined as an
/// inconsistent store is, while the dataset's figures say `undecided`, with the violations
/// and why.
#[tokio::test]
async fn an_undecided_store_is_quarantined_and_says_why() -> Result<(), Box<dyn std::error::Error>>
{
    let store = StoreService::new(StoreConfig::in_memory())?;
    let import = MutationPipeline::new(
        std::sync::Arc::new(store.clone()),
        std::sync::Arc::new(ReasonerService::new(ReasonerConfig::default())),
    );
    import.execute_update(&SparqlUpdateRequest::new(format!(
        "INSERT DATA {{ <{EX}note> a <http://www.w3.org/2002/07/owl#AnnotationProperty> ;
                         <http://www.w3.org/2000/01/rdf-schema#domain> <{EX}C> .
                       <{EX}C> <http://www.w3.org/2002/07/owl#disjointWith> <{EX}D> .
                       <{EX}x> a <{EX}D> ; <{EX}note> \"v\" }}"
    )))?;
    let dl = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl);
    store.rematerialise(dl.materialised_program().expect("the OWL 2 RL closure"))?;
    let app = test_app_with_store(store, PolicyConfig::default(), dl)?;

    let ready = app.clone().oneshot(get("/readyz")).await?;
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    let info = body_text(app.oneshot(get("/dataset/info")).await?).await?;
    let info: serde_json::Value = serde_json::from_str(&info)?;
    assert_eq!(info["status"], "quarantined", "{info}");
    assert_eq!(info["consistency"], "undecided", "{info}");
    assert!(info["violations"].as_u64() > Some(0), "{info}");
    let why = info["consistency_reason"].as_str().unwrap_or_default();
    assert!(
        why.contains("annotation-property axioms or a punned name"),
        "{info}"
    );
    Ok(())
}
