//! A server's state, once gone, keeps nothing of its data directory: the same process can open
//! that directory again.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nrese_engine::fault_points::CloseHold;
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::{AppState, DeploymentPosture};
use nrese_store::{StoreConfig, StoreService};

fn open(dir: &Path) -> anyhow::Result<AppState> {
    AppState::try_new(
        StoreService::new(StoreConfig::on_disk(dir))?,
        ReasonerService::new(ReasonerConfig::default()),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
}

/// Spec: a state releases its system store (`system/`, the access state) before its served
/// default store, so once the served default store is released (its last handle gone, which is
/// what a caller that served the state can see), the data directory opens again in this
/// process. Named repositories' stores are not covered.
///
/// The state's last clone is dropped on a thread of its own, as an HTTP connection's task can
/// drop it after the server's end (axum 0.8.8, `serve/mod.rs:414-415`: `close_rx`, which ends
/// `serve`, goes before the connection that holds the app). Its system store's close is held
/// before it lets the directory go ([`CloseHold`]). While it is held, either the served store
/// is still the state's (the caller waits) or the caller, seeing it released, opens the
/// directory again, which must succeed. The hold must have lasted until the test released it:
/// a close that went on by itself first proves nothing. Guard: once the close goes on, the
/// drop ends and the directory opens.
#[test]
fn a_dropped_states_directory_opens_again_once_its_served_store_is_released() {
    let deadline = Duration::from_secs(30);
    let dir = tempfile::tempdir().expect("dir");
    let state = open(dir.path()).expect("the first open");
    let served: Arc<StoreService> = state.store();
    let hold = CloseHold::arm(&dir.path().join("system"));
    let (done, dropped) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(state);
        let _ = done.send(());
    });
    hold.wait(deadline);
    let reopened = (Arc::strong_count(&served) == 1).then(|| {
        drop(served);
        open(dir.path()).map(drop)
    });
    assert!(
        hold.release(deadline),
        "the system store's close went on by itself before the test released it"
    );
    dropped
        .recv_timeout(deadline)
        .expect("the state's drop did not end once its close was released");
    dropper.join().expect("the state's drop");
    if let Some(Err(error)) = reopened {
        panic!(
            "the served store was released while the state's system store still held the \
             directory: {error:#}"
        );
    }
    open(dir.path()).expect("the directory once the state is gone");
}
