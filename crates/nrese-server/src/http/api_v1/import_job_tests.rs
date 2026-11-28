use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::jobs::JobState;
use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};

use super::start_import;
use crate::ai::AiSuggestionService;
use crate::policy::PolicyConfig;
use crate::runtime_posture::DeploymentPosture;
use crate::state::AppState;

// Spec: docs/ops/http-api.md says an import job ends `done`, `failed` or `cancelled`, its
// error with retry advice. One whose task the runtime discarded before it ran -> `failed`,
// saying nothing of it ran and that it is safe to load again; nothing of it is stored.
#[test]
fn an_import_discarded_before_it_ran_ends_failed_as_never_started() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.nt");
    std::fs::write(&file, "<urn:t:job> <urn:t:p> <urn:t:o> .\n").unwrap();
    let state = AppState::new(
        StoreService::new(StoreConfig::in_memory()).unwrap(),
        ReasonerService::new(ReasonerConfig::default()),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    );
    let request = BulkLoadRequest {
        files: vec![file],
        replace: false,
        graph: GraphTarget::DefaultGraph,
        invalid: None,
    };
    // A blocking task submitted once its runtime is shutting down is dropped unrun, as at a
    // server's stop (tokio's blocking pool).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    let handle = runtime.handle().clone();
    runtime.shutdown_background();
    let (_, _, started) = {
        let _entered = handle.enter();
        start_import(&state, request, "data.nt".to_owned(), None)
    };

    let job = state.jobs().get(started.0.job).unwrap();
    assert_eq!(job.state, JobState::Failed, "{job:?}");
    assert_eq!(
        job.error.as_deref(),
        Some(
            "the server stopped before the import started; nothing of it ran. It's safe to load it again."
        ),
        "{job:?}"
    );
    assert_eq!(job.report, None, "{job:?}");
    assert_eq!(
        state.store().stats().unwrap().quad_count,
        0,
        "nothing of it ran"
    );
}
