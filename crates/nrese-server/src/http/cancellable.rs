//! Cancellation ownership for a handler waiting on blocking work or a streaming body.

use std::time::Duration;

use nrese_store::CancellationToken;
use tokio::task::{AbortHandle, JoinError};
use tokio::time::error::Elapsed;

/// Dropping a request stops its worker; a streaming body also owns its watchdog.
pub(super) struct CancelOnDrop(pub CancellationToken, pub Option<AbortHandle>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
        if let Some(watchdog) = &self.1 {
            watchdog.abort();
        }
    }
}

/// The guard is created before spawning and lives across the await. Timeout and future
/// Drop cancel the worker without relying on aborting a blocking JoinHandle.
pub(super) async fn blocking<T: Send + 'static>(
    seconds: Duration,
    cancellation: CancellationToken,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<Result<T, JoinError>, Elapsed> {
    let _guard = CancelOnDrop(cancellation, None);
    tokio::time::timeout(seconds, tokio::task::spawn_blocking(work)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dropped handler and an elapsed deadline both cancel the actual blocking
    /// worker. Channels order the observation; the worker does no busy polling.
    #[tokio::test]
    async fn handler_drop_and_timeout_cancel_the_blocking_worker() {
        for dropped in [true, false] {
            let cancellation = CancellationToken::new();
            let token = cancellation.clone();
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let (observed, finished) = tokio::sync::oneshot::channel();
            let handler = tokio::spawn(blocking(
                if dropped {
                    Duration::from_secs(60)
                } else {
                    Duration::ZERO
                },
                cancellation,
                move || {
                    let _ = started.send(());
                    wait.recv().unwrap();
                    let _ = observed.send(token.is_cancelled());
                },
            ));
            tokio::time::timeout(Duration::from_secs(5), ready)
                .await
                .unwrap()
                .unwrap();
            if dropped {
                handler.abort();
                assert!(handler.await.unwrap_err().is_cancelled());
            } else {
                assert!(handler.await.unwrap().is_err());
            }
            release.send(()).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(5), finished)
                    .await
                    .unwrap()
                    .unwrap(),
                "worker stopped after handler drop/timeout"
            );
        }
    }
}
