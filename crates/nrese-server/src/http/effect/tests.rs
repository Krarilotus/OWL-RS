use std::future::{poll_fn, ready};
use std::pin::pin;
use std::sync::mpsc;
use std::task::Poll;

use axum::http::{StatusCode, header};
use axum::response::IntoResponse;

use super::*;

// Spec: lifecycle rule 5 says that even a simulated failure records its end before release.
#[tokio::test]
async fn held_value_is_dropped_after_the_end_is_recorded() {
    struct HeldValue {
        terminal: mpsc::Receiver<Terminal<End<()>>>,
        recorded: tokio::sync::oneshot::Sender<bool>,
    }

    impl Drop for HeldValue {
        fn drop(&mut self) {
            let recorded = self
                .terminal
                .try_recv()
                .ok()
                .and_then(|terminal| terminal.get());
            let _ = self
                .recorded
                .send(matches!(recorded, Some(End::Fallback(None))));
        }
    }

    let (send_terminal, terminal) = mpsc::channel();
    let (recorded, observed) = tokio::sync::oneshot::channel();
    let (release, gate) = mpsc::channel();
    let end = spawn(
        Some(HeldValue { terminal, recorded }),
        || End::Fallback(None),
        move |_| {
            // The work is held at a gate until the observer has the terminal.
            let _ = gate.recv_timeout(Duration::from_secs(5));
            panic!("simulated failure");
        },
    );
    let _ = send_terminal.send(end);
    let _ = release.send(());
    assert!(
        observed.await.unwrap(),
        "release must observe the fallback end"
    );
}

// Spec: the package's AwaitEnd row says a lost cancel waits through the deadline for the end.
#[tokio::test(start_paused = true)]
async fn plain_write_awaits_the_record_after_losing_cancel() {
    let timeout = Duration::from_secs(2);
    let ticket = MutationTicket::new();
    let _ = ticket.begin_commit();
    let (release, gate) = mpsc::channel();
    let mut answer = pin!(run(
        ticket,
        timeout,
        "timeout",
        LostCancel::AwaitEnd,
        None::<()>,
        |_, _| {
            let _ = gate.recv_timeout(Duration::from_secs(5));
            Err(ApiError::bad_request("known error"))
        }
    ));
    let _ = poll_fn(|cx| ready(answer.as_mut().poll(cx))).await;
    tokio::time::advance(timeout * 3).await;
    let after_deadline = poll_fn(|cx| ready(answer.as_mut().poll(cx))).await;
    tokio::time::advance(timeout * 3).await;
    let after_grace = poll_fn(|cx| ready(answer.as_mut().poll(cx))).await;
    let _ = release.send(());
    assert!(matches!(
        (after_deadline, after_grace),
        (Poll::Pending, Poll::Pending)
    ));
    assert!(matches!(answer.await, Err(ApiError::BadRequest(message)) if message == "known error"));
}

// Spec: the package's Grace row says an unresolved claim answers retryable 503 after one grace.
#[tokio::test(start_paused = true)]
async fn grace_ends_with_the_callers_retryable_answer() {
    let timeout = Duration::from_secs(2);
    let ticket = MutationTicket::new();
    let _ = ticket.begin_commit();
    let (release, gate) = mpsc::channel();
    let mut answer = pin!(run(
        ticket,
        timeout,
        "timeout",
        LostCancel::Grace(timeout, "commit in progress"),
        None::<()>,
        move |_, _| {
            // Hold the work at a gate so only the request's two timers can finish the wait.
            let _ = gate.recv_timeout(Duration::from_secs(5));
            Err(ApiError::bad_request("known error"))
        },
    ));
    let _ = poll_fn(|cx| ready(answer.as_mut().poll(cx))).await;
    tokio::time::advance(timeout).await;
    let at_deadline = poll_fn(|cx| ready(answer.as_mut().poll(cx))).await;
    tokio::time::advance(timeout).await;
    let at_grace = poll_fn(|cx| ready(answer.as_mut().poll(cx))).await;
    let _ = release.send(());
    let Poll::Ready(answer) = at_grace else {
        panic!("the grace did not end")
    };
    assert!(matches!(&answer, Err(ApiError::Busy(message)) if message == "commit in progress"));
    assert!(at_deadline.is_pending());
    let response = answer.unwrap_err().into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::RETRY_AFTER], "1");
}
