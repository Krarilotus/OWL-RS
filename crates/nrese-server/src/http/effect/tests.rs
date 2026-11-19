use std::future::poll_fn;
use std::pin::pin;
use std::sync::{Arc, mpsc};
use std::task::Poll;

use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, Requester, SparqlUpdateRequest, StoreConfig, StoreService,
};

use super::*;

fn publish_at_gate(
    ticket: &MutationTicket,
    published: tokio::sync::oneshot::Sender<u64>,
    release: mpsc::Receiver<()>,
) -> MutationCommitReport {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
    let reasoner = Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
        ReasoningMode::Disabled,
    )));
    let report = MutationPipeline::new(store, reasoner)
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(
                "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
            )),
            &Requester::all(),
            ticket,
        )
        .unwrap();
    let MutationCommitReport::Applied { revision } = report else {
        unreachable!("update report")
    };
    let _ = published.send(revision);
    // Hold the worker at a gate after publication and before the terminal ends.
    let _ = release.recv_timeout(Duration::from_secs(5));
    report
}

// Spec: lifecycle rule 3 says a disconnect before the worker claims the write cancels it.
#[tokio::test]
async fn a_disconnected_request_does_not_publish() {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
    let before = store.current_revision();
    let reasoner = Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
        ReasoningMode::Disabled,
    )));
    let pipeline = MutationPipeline::new(Arc::clone(&store), reasoner);
    let (arrived, waiting) = tokio::sync::oneshot::channel();
    let (release, gate) = mpsc::channel();
    let (finished, result) = tokio::sync::oneshot::channel();
    let mut request = Box::pin(run(
        MutationTicket::new(),
        Duration::from_secs(30),
        "timeout",
        LostCancel::AwaitEnd,
        None::<()>,
        move |ticket, _| {
            let _ = arrived.send(());
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            let applied = pipeline.apply(
                MutationCommand::Update(SparqlUpdateRequest::new(
                    "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
                )),
                &Requester::all(),
                ticket,
            );
            let _ = finished.send(applied);
            Err(ApiError::internal("test worker finished"))
        },
    ));
    let _ = poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx))).await;
    waiting.await.unwrap();
    drop(request);
    release.send(()).unwrap();
    let applied = result.await.unwrap();
    assert_eq!(
        store.current_revision(),
        before,
        "disconnected write published"
    );
    assert!(matches!(
        applied,
        Err(nrese_store::MutationError::Cancelled)
    ));
}

// Spec: lifecycle rule 5 says that even a simulated failure records its end before release.
#[tokio::test]
async fn held_value_is_dropped_after_the_end_is_recorded() {
    struct HeldValue {
        terminal: mpsc::Receiver<Terminal<End<()>>>,
        recorded: tokio::sync::mpsc::UnboundedSender<bool>,
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
    let (recorded, mut observed) = tokio::sync::mpsc::unbounded_channel();
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
        observed.recv().await.unwrap(),
        "release must observe the fallback end"
    );
}

// Spec: the package's AwaitEnd row says a lost cancel waits through the deadline for the end.
#[tokio::test(start_paused = true)]
async fn plain_write_awaits_the_record_after_losing_cancel() {
    let timeout = Duration::from_secs(2);
    let ticket = MutationTicket::new();
    let (release, gate) = mpsc::channel();
    let (published, original_revision) = tokio::sync::oneshot::channel();
    let mut answer = pin!(run(
        ticket,
        timeout,
        "timeout",
        LostCancel::AwaitEnd,
        None::<()>,
        move |ticket, _| {
            publish_at_gate(ticket, published, gate);
            panic!("simulated failure after publication");
        }
    ));
    let _ = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    let original_revision = original_revision.await.unwrap();
    tokio::time::advance(timeout * 3).await;
    let after_deadline = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    tokio::time::advance(timeout * 3).await;
    let after_grace = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    let _ = release.send(());
    assert!(matches!(
        (after_deadline, after_grace),
        (Poll::Pending, Poll::Pending)
    ));
    assert!(
        matches!(answer.await, Ok(MutationCommitReport::Applied { revision }) if revision == original_revision)
    );
}

// Spec: the package's Grace row says an unresolved claim answers retryable 503 after one grace.
#[tokio::test(start_paused = true)]
async fn grace_ends_with_the_callers_retryable_answer() {
    let timeout = Duration::from_secs(2);
    let ticket = MutationTicket::new();
    let _ = ticket.end_pending();
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
    let _ = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    let tick = Duration::from_millis(1);
    tokio::time::advance(timeout + tick).await;
    let at_deadline = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    tokio::time::advance(timeout + tick).await;
    let at_grace = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
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

// Spec: after grace, publication yields its original report even while the worker is held.
#[tokio::test(start_paused = true)]
async fn grace_reads_the_original_report_while_the_worker_is_held() {
    let timeout = Duration::from_secs(2);
    let (release, gate) = mpsc::channel();
    let (published, original_revision) = tokio::sync::oneshot::channel();
    let mut answer = pin!(run(
        MutationTicket::new(),
        timeout,
        "timeout",
        LostCancel::Grace(timeout, "commit in progress"),
        None::<()>,
        move |ticket, _| Ok(publish_at_gate(ticket, published, gate)),
    ));
    let _ = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    let original_revision = original_revision.await.unwrap();
    let tick = Duration::from_millis(1);
    tokio::time::advance(timeout + tick).await;
    let before_grace = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    tokio::time::advance(timeout + tick).await;
    let after_grace = poll_fn(|cx| Poll::Ready(answer.as_mut().poll(cx))).await;
    let _ = release.send(());
    assert!(
        matches!(after_grace, Poll::Ready(Ok(MutationCommitReport::Applied { revision })) if revision == original_revision)
    );
    assert!(before_grace.is_pending());
}
