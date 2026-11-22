use std::cell::Cell;
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
use tokio::sync::oneshot;

use super::*;
use crate::ai::AiSuggestionService;
use crate::auth::Identity;
use crate::policy::PolicyConfig;
use crate::{AppState, DeploymentPosture};

thread_local! {
    // Each test installs its own pause on its current-thread runtime. Only its next
    // worker takes the pause with it.
    pub(super) static PAUSE_AFTER_END: Cell<Option<PauseAfterEnd>> = const { Cell::new(None) };
}

pub(super) struct PauseAfterEnd {
    reached: oneshot::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl PauseAfterEnd {
    pub(super) fn wait(self) {
        let _ = self.reached.send(());
        let _ = self.release.recv_timeout(Duration::from_secs(30));
    }
}

fn pause_next_worker() -> (oneshot::Receiver<()>, mpsc::Sender<()>) {
    let (reached, waiting) = oneshot::channel();
    let (resume, release) = mpsc::channel();
    PAUSE_AFTER_END.set(Some(PauseAfterEnd { reached, release }));
    (waiting, resume)
}

async fn resume_worker(waiting: oneshot::Receiver<()>, resume: mpsc::Sender<()>) {
    tokio::time::timeout(Duration::from_secs(30), waiting)
        .await
        .expect("worker did not reach the pause in time")
        .expect("worker dropped the pause");
    resume.send(()).expect("worker was still paused");
}

// Spec: after an answered write, dropping the state allows an immediate reopen.
#[tokio::test]
async fn request_release_allows_reopening_the_system_store() {
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        AppState::try_new(
            StoreService::new(StoreConfig::on_disk(dir.path()))?,
            ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Disabled)),
            PolicyConfig::default(),
            AiSuggestionService::disabled(),
            DeploymentPosture::OpenWorkbench,
        )
    };
    let state = open().unwrap();
    let access = state.access_view(&Identity {
        admin: true,
        ..Default::default()
    });
    let requester = access.requester();
    let ticket = MutationTicket::checking(Some(access.rights.expect("checking ticket")));
    let pipeline = state.pipeline();
    let (waiting, resume) = pause_next_worker();
    let report = run(
        ticket,
        Duration::from_secs(30),
        "timeout",
        LostCancel::AwaitEnd,
        None::<()>,
        move |ticket, _| {
            pipeline
                .apply(
                    MutationCommand::Update(SparqlUpdateRequest::new(
                        "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
                    )),
                    &requester,
                    ticket,
                )
                .map_err(|error| ApiError::internal(error.to_string()))
        },
    )
    .await;
    drop(state);
    let reopened = open();
    resume_worker(waiting, resume).await;
    assert!(matches!(
        report.unwrap(),
        MutationCommitReport::Applied { .. }
    ));
    reopened.expect("reopen after the write answer");
}

// Spec: lifecycle rule 5 keeps the held value until after the normal end.
#[tokio::test]
async fn held_value_is_dropped_after_the_normal_end() {
    struct HeldValue(Option<oneshot::Sender<()>>);

    impl Drop for HeldValue {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _ = dropped.send(());
            }
        }
    }

    let (notify_drop, mut dropped) = oneshot::channel();
    let (waiting, resume) = pause_next_worker();
    let answer = run(
        MutationTicket::new(),
        Duration::from_secs(30),
        "timeout",
        LostCancel::AwaitEnd,
        Some(HeldValue(Some(notify_drop))),
        |_, _| Err(ApiError::bad_request("write refused")),
    )
    .await;
    let held_at_the_answer = matches!(dropped.try_recv(), Err(oneshot::error::TryRecvError::Empty));
    resume_worker(waiting, resume).await;
    assert!(matches!(answer, Err(ApiError::BadRequest(_))));
    assert!(
        held_at_the_answer,
        "the worker released the value before answering"
    );
    tokio::time::timeout(Duration::from_secs(30), dropped)
        .await
        .expect("worker did not drop the held value in time")
        .unwrap();
}

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
    tokio::time::timeout(Duration::from_secs(30), waiting)
        .await
        .expect("worker did not reach the cancellation gate in time")
        .unwrap();
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

// Spec: lifecycle rule 5 records the fallback end before the held value drops on panic.
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
                .is_ok_and(|terminal| matches!(terminal.get(), Some(End::Fallback(None))));
            let _ = self.recorded.send(recorded);
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
    let _ = send_terminal.send(end.clone());
    let _ = release.send(());
    let answer = end
        .wait_or(tokio::time::sleep(Duration::from_secs(30)))
        .await;
    assert!(matches!(answer, Some(End::Fallback(None))));
    assert!(
        tokio::time::timeout(Duration::from_secs(30), observed.recv())
            .await
            .expect("worker did not drop the held value in time")
            .unwrap(),
        "the worker released the value before recording the fallback end"
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
