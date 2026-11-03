//! Blocking work a handler answers with, under the policy's deadline.

use std::time::Duration;

use nrese_store::CancellationToken;

use crate::error::ApiError;

/// Cancels its token when dropped.
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Runs `work` on a blocking thread and waits for it until `deadline`: its value, or a
/// timeout answered with `timeout_message`. `cancellation` is the token `work` runs with;
/// it is cancelled when this future ends, however it ends: at the deadline, when the work
/// is done (harmless), or dropped because the client is gone. So work nobody waits for
/// stops at its next check instead of running on unobserved (`JoinHandle::abort` can't stop
/// a blocking task).
pub(crate) async fn within<T: Send + 'static>(
    deadline: Duration,
    cancellation: CancellationToken,
    timeout_message: &'static str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    let _cancel = CancelOnDrop(cancellation);
    tokio::time::timeout(deadline, tokio::task::spawn_blocking(work))
        .await
        .map_err(|_| ApiError::timeout(timeout_message))?
        .map_err(|error| ApiError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use nrese_store::CancellationToken;

    use super::within;

    /// Work that waits for `token` to be cancelled and says whether it was. It gives up after
    /// about ten seconds: a guard against hanging the test, not a measurement.
    fn waiting(
        token: CancellationToken,
        started: tokio::sync::oneshot::Sender<()>,
        seen: mpsc::Sender<bool>,
    ) -> impl FnOnce() + Send + 'static {
        move || {
            let _ = started.send(());
            for _ in 0..10_000 {
                if token.is_cancelled() {
                    let _ = seen.send(true);
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = seen.send(false);
        }
    }

    /// SS-22: the work a handler stops waiting for is cancelled, both when its deadline
    /// passes and when the handler is dropped (the client is gone), so it doesn't run on
    /// unobserved.
    #[tokio::test]
    async fn work_is_cancelled_when_the_handler_stops_waiting() {
        // The deadline passes while the work runs.
        let token = CancellationToken::new();
        let (started, _) = tokio::sync::oneshot::channel();
        let (seen, observed) = mpsc::channel();
        let answer = within(
            Duration::from_millis(10),
            token.clone(),
            "too late",
            waiting(token, started, seen),
        )
        .await;
        assert!(answer.is_err(), "the deadline is answered");
        assert_eq!(
            observed.recv(),
            Ok(true),
            "the work ran on past the deadline"
        );

        // The handler is dropped while the work runs.
        let token = CancellationToken::new();
        let (started, running) = tokio::sync::oneshot::channel();
        let (seen, observed) = mpsc::channel();
        let handler = tokio::spawn(within(
            Duration::from_secs(3600),
            token.clone(),
            "too late",
            waiting(token, started, seen),
        ));
        running.await.expect("the work started");
        handler.abort();
        assert!(handler.await.is_err_and(|error| error.is_cancelled()));
        assert_eq!(
            observed.recv(),
            Ok(true),
            "the work ran on after its handler was dropped"
        );
    }
}
