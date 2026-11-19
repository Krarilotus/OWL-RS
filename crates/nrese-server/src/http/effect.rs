//! Blocking work whose HTTP answer comes from its record.

use std::future::pending;
use std::time::Duration;

use nrese_exec::terminal::{Guard, Terminal, terminal};
use nrese_store::mutation::{MutationCommitReport, MutationState, MutationTicket};
use tokio::time::{Instant, sleep_until};

use crate::error::ApiError;

#[derive(Clone)]
enum End<T> {
    Returned(Result<T, ApiError>),
    Cancelled,
    Fallback(Option<MutationState>),
}

// Fields drop in declaration order, including when the queued task never runs.
struct Held<T, H> {
    guard: Guard<End<T>>,
    value: Option<H>,
}

impl<T, H> Held<T, H> {
    fn finish(mut self, work: impl FnOnce(Option<&mut H>) -> End<T>) {
        self.guard.end(work(self.value.as_mut()));
    }
}

fn spawn<T: Clone + Send + Sync + 'static, H: Send + 'static>(
    held: Option<H>,
    fallback: impl FnOnce() -> End<T> + Send + 'static,
    work: impl FnOnce(Option<&mut H>) -> End<T> + Send + 'static,
) -> Terminal<End<T>> {
    let (end, guard) = terminal(fallback);
    let held = Held { guard, value: held };
    drop(tokio::task::spawn_blocking(move || held.finish(work)));
    end
}

fn unknown() -> ApiError {
    ApiError::internal("whether it was applied is unknown: read the data before sending it again")
}

pub(super) enum LostCancel<'a> {
    AwaitEnd,
    // M1 will select this for requests whose retries can look up the same operation.
    #[allow(dead_code)]
    Grace(Duration, &'a str),
}

pub(super) async fn run<H: Send + 'static>(
    ticket: MutationTicket,
    timeout: Duration,
    timeout_message: &str,
    after_cancel: LostCancel<'_>,
    held: Option<H>,
    work: impl FnOnce(&MutationTicket, Option<&mut H>) -> Result<MutationCommitReport, ApiError>
    + Send
    + 'static,
) -> Result<MutationCommitReport, ApiError> {
    let deadline = Instant::now() + timeout;
    let fallback = ticket.clone();
    let worker = ticket.clone();
    let end = spawn(
        held,
        move || End::Fallback(Some(fallback.end_pending())),
        move |held| {
            let result = work(&worker, held);
            if result.is_err() && worker.end_pending() == MutationState::Cancelled {
                End::Cancelled
            } else {
                End::Returned(result)
            }
        },
    );
    let record = match end.wait_or(sleep_until(deadline)).await {
        Some(record) => Some(record),
        None if ticket.cancel() => Some(End::Cancelled),
        None => match after_cancel {
            LostCancel::AwaitEnd => end.wait_or(pending()).await,
            LostCancel::Grace(duration, message) => {
                match end.wait_or(sleep_until(Instant::now() + duration)).await {
                    Some(record) => Some(record),
                    None => {
                        return ticket.published().ok_or_else(|| ApiError::busy(message));
                    }
                }
            }
        },
    };
    match record {
        Some(End::Returned(result)) => result,
        Some(End::Cancelled | End::Fallback(Some(MutationState::Cancelled))) => {
            Err(ApiError::timeout(timeout_message))
        }
        Some(End::Fallback(Some(MutationState::Pending))) => Err(ApiError::internal(
            "not run: nothing of this write was applied; send it again",
        )),
        Some(End::Fallback(Some(MutationState::Committing | MutationState::Ended))) => {
            Err(ApiError::internal("nothing of this write was published"))
        }
        Some(End::Fallback(Some(MutationState::Published))) => {
            ticket.published().ok_or_else(unknown)
        }
        Some(End::Fallback(Some(MutationState::Entered) | None)) | None => Err(unknown()),
    }
}

/// The load carries no request deadline or publication marks. A lost worker leaves an
/// unknown outcome even if it might have changed the data.
pub(super) async fn untracked<T: Clone + Send + Sync + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    let end = spawn(
        None::<()>,
        || End::Fallback(None),
        |_| End::Returned(work()),
    );
    match end.wait_or(pending()).await {
        Some(End::Returned(result)) => result,
        _ => Err(unknown()),
    }
}

#[cfg(test)]
mod tests;
