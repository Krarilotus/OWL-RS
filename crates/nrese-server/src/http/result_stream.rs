//! Streams output produced on a blocking thread into an HTTP response body.
//!
//! - **Backpressure.** The producer writes through a [`ChannelWriter`]: chunks of at most
//!   64 KiB (a large write, such as a cached result, is split) over a bounded channel. A
//!   slow client slows the producer down instead of the server buffering the whole result.
//! - **Status.** The response status is chosen only when the first chunk (or the producer's
//!   error) arrives, so errors and timeouts before any output still get a proper status.
//! - **After the first chunk,** a failure can only abort the transfer: no final chunk on
//!   HTTP/1.1, a reset stream on HTTP/2, never a clean end. The 200 is already sent; clients
//!   see a truncated body they can tell from a complete one, as with QLever or Fuseki. How
//!   the producer ended is kept beside the channel ([`Outcome`]), not sent through it: a
//!   failure that comes while the channel is full (a client that stopped reading) can't be
//!   lost, and the body ends cleanly only after a producer that finished.
//! - **Trailers.** A producer may stage trailer fields ([`ChannelWriter::trailers`]).
//!   They go out after the producer and the final data write have succeeded.
//! - **Time ([`Pace`]).** A computation deadline for the whole stream (a query's timeout),
//!   or a deadline for the first chunk and then a bound on each chunk's stall (reads of
//!   stored statements, the export): a client that keeps reading is never cut off, one that
//!   stops is. Either way the producer waits for room in the channel only so long, so it
//!   lets go of its thread and snapshot when nobody reads. Under a stall bound, whether the
//!   first chunk was in time is decided once ([`Start`]): a chunk goes out only after the
//!   producer claimed the start, and the first deadline cancels only a stream nobody
//!   started.
//! - **Cancellation.** Reaching the deadline or a stall, or the client disconnecting (the
//!   body is dropped), cancels the producer's token, so evaluation stops promptly instead of
//!   running to completion for nobody.
//! - **HTTP/1.0** has no way to mark a streamed answer as cut: its body ends when the
//!   connection closes, cut or not. An HTTP/1.0 request's answer is held whole instead
//!   ([`answering`]), its status chosen at the end and sent with its `Content-Length`, so a
//!   stop before the end gets its own status. What is held is charged to the request's
//!   memory envelope (`budgets.query_memory`): past it, 413 ([`too_large`]). Its deadline is
//!   the first one for all of it, a stall bound's too. Sent, each frame must be taken
//!   within a stall bound (the read's own, or a query's time) or what is left is let go
//!   and the body ends short of its length ([`Held`]): a client that stops reading keeps
//!   no answer charged. A reader of some graphs uses HTTP/1.1 or HTTP/2 for result streams.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Version, header};
use axum::response::{IntoResponse, Response};
use http_body::{Frame, SizeHint};
use nrese_exec::{BudgetExceeded, Envelope};
use nrese_store::{CancellationToken, ReadScope, StoreError};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::config::units::format_size;
use crate::error::ApiError;
use crate::state::AppState;

tokio::task_local! {
    /// Whether the request being answered gets its answers whole ([`answering`]).
    static WHOLE: bool;
}

/// For an answer read without a query (stored statements, the export): an envelope of a
/// query's size (`budgets.query_memory`) for the answer held whole, opened only if it is.
/// A statement or graph read draws its working memory from it too (its view, a
/// transaction's replay: [`ChannelWriter::holding`]), as a query run does from its own.
pub fn read_envelope(state: &AppState) -> impl FnOnce() -> Envelope + use<> {
    let store = state.store();
    let limit = state.policy().limits.max_query_memory_bytes;
    move || store.envelope((limit > 0).then_some(limit))
}

/// Answers a request of `version` with `respond`: whole, if it is HTTP/1.0 or older. Every
/// request passes here (`app::screen`), so every route that streams through
/// [`stream_blocking`] is covered; the one that streams otherwise, the replication image,
/// is a file sent with its size.
pub async fn answering<F: Future>(version: Version, respond: F) -> F::Output {
    let whole = matches!(version, Version::HTTP_09 | Version::HTTP_10);
    WHOLE.scope(whole, respond).await
}

const CHUNK_BYTES: usize = 64 * 1024;
/// Chunks in flight between producer and client (at most ~0.5 MiB per request).
const CHANNEL_CHUNKS: usize = 8;

/// Maximum live chunk payload: the queue, a pending send, the producer's replacement
/// buffer and the frame currently held by the consumer. Independent of result size;
/// allocation metadata and other per-request state are outside this payload bound.
pub const MAX_BUFFERED_OUTPUT_BYTES: usize = (CHANNEL_CHUNKS + 3) * CHUNK_BYTES;

type Message = Frame<Bytes>;

/// Whose result stream it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    /// A reader of every graph: over HTTP/1.0 its answer is held whole, and a memory refusal
    /// of it tells which limit refused it, its own (413, the query's or the HTTP/1.0
    /// answer's) or the memory all queries share (503).
    Every,
    /// A reader of some graphs. Refused on HTTP/1.0 before any evaluation.
    Restricted,
}

impl Reader {
    /// The reader of a read in `scope`.
    pub fn of(scope: &ReadScope) -> Self {
        match scope.reads_everything() {
            true => Self::Every,
            false => Self::Restricted,
        }
    }

    /// Check the protocol using only the grant, before opening or preparing a read.
    pub fn refuse_unframed(self) -> Option<Response> {
        (self == Self::Restricted && WHOLE.try_with(|whole| *whole).unwrap_or(false)).then(|| {
            let status = StatusCode::HTTP_VERSION_NOT_SUPPORTED;
            crate::error::problem(
                status,
                &serde_json::json!({
                    "type": "https://nrese.dev/problems/http-version-not-supported",
                    "title": "HTTP Version Not Supported",
                    "status": status.as_u16(),
                    "detail": "Use HTTP/1.1 or HTTP/2 for restricted result streams."
                }),
            )
        })
    }
}

/// How the producer ended: `None` while it runs (or if it panicked), its result after.
#[derive(Default)]
struct Outcome(Mutex<Option<Result<(), ApiError>>>);

impl Outcome {
    fn set(&self, result: Result<(), ApiError>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
    }

    /// The producer's result, once its sender is gone: one that recorded none didn't finish.
    fn take(&self) -> Result<(), ApiError> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .unwrap_or_else(|| Err(ApiError::internal("the output ended without finishing")))
    }

    /// The producer's error, if it has recorded one (it may still have frames queued); a
    /// success, or nothing recorded yet, stays.
    fn failed(&self) -> Option<ApiError> {
        let mut outcome = self.0.lock().unwrap_or_else(|p| p.into_inner());
        match outcome.take() {
            Some(Err(error)) => Some(error),
            other => {
                *outcome = other;
                None
            }
        }
    }
}

/// Whether the first chunk went out in time, decided once: the producer claims the start
/// before it publishes the first chunk, the first deadline claims it late; whichever comes
/// first wins, so a chunk the client got is never followed by a cancellation for lateness.
#[derive(Default)]
struct Start(AtomicU8);

impl Start {
    const WAITING: u8 = 0;
    const STARTED: u8 = 1;
    const LATE: u8 = 2;

    fn started(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::STARTED
    }

    /// The producer's claim: whether the stream has started (now or before).
    fn begin(&self) -> bool {
        match self.0.compare_exchange(
            Self::WAITING,
            Self::STARTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(state) => state == Self::STARTED,
        }
    }

    /// The first deadline's claim: whether nothing started before it.
    fn late(&self) -> bool {
        self.0
            .compare_exchange(
                Self::WAITING,
                Self::LATE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// How long a stream may take.
#[derive(Debug, Clone, Copy)]
pub enum Pace {
    /// One deadline for all of it, read or not: a query's timeout bounds its computation.
    Total(Instant),
    /// The first chunk by `first`; after it, every chunk moves within `stall`, from the
    /// producer into the channel and from the channel to the client. A large result read
    /// at any steady speed is never cut off; a client that stops reading, or a producer
    /// that stops writing, is.
    Stall { first: Instant, stall: Duration },
}

impl Pace {
    /// The first chunk within `timeout` from now, then every chunk within it: reads of
    /// stored statements (`timeouts.graph_read`).
    pub fn stalled_after(timeout: Duration) -> Self {
        Self::Stall {
            first: Instant::now() + timeout,
            stall: timeout,
        }
    }

    /// The latest a chunk may move, waiting from now; `started`: the first one has.
    fn until(self, started: bool) -> Instant {
        match self {
            Self::Total(deadline) => deadline,
            Self::Stall { first, .. } if !started => first,
            Self::Stall { stall, .. } => Instant::now() + stall,
        }
    }
}

/// `io::Write` that forwards its output in chunks of at most [`CHUNK_BYTES`] over the
/// channel, waiting for room as long as its [`Pace`] allows.
pub struct ChannelWriter {
    sender: mpsc::Sender<Message>,
    buffer: Vec<u8>,
    trailers: Option<HeaderMap>,
    pace: Pace,
    start: Arc<Start>,
    runtime: tokio::runtime::Handle,
    /// The envelope the answer is held whole in (HTTP/1.0), opened before the producer
    /// starts; `None` where the answer streams.
    holding: Option<Envelope>,
}

impl ChannelWriter {
    /// The envelope what is written is held in, where the answer is held whole: what makes
    /// the answer draws its working memory from it too, so that the request's limit bounds
    /// both together (`nrese_store::ReadContext::within`). `None` where the answer streams.
    pub fn holding(&self) -> Option<&Envelope> {
        self.holding.as_ref()
    }

    /// The store's `error` that stopped what this writer's answer is made of, as its client
    /// is told it: as `otherwise` tells it, unless the answer is held whole and `error` is a
    /// memory refusal of its work for what the answer held (the budget's own judgement,
    /// carried with the refusal: [`nrese_exec::BudgetExceeded::for_held`]). Then it is the
    /// 413 of an answer too large for HTTP/1.0. Every producer of an answer that may be held
    /// whole maps its store errors through here, and nothing after it maps them again.
    pub fn refused(
        &self,
        error: StoreError,
        otherwise: impl FnOnce(StoreError) -> ApiError,
    ) -> ApiError {
        refusal_told(self.holding.as_ref(), error, otherwise)
    }

    /// Sends `message`, waiting for room in the channel as long as the pace allows. The
    /// first one claims the start once it has room, before it goes out.
    fn send(&self, message: Message) -> io::Result<()> {
        let started = self.start.started();
        let until = self.pace.until(started);
        let room = self
            .runtime
            .block_on(tokio::time::timeout_at(until, self.sender.reserve()));
        match room {
            Ok(Ok(permit)) => {
                if !started && !self.start.begin() {
                    return Err(io::Error::from(io::ErrorKind::TimedOut)); // the first was late
                }
                permit.send(message);
                Ok(())
            }
            Ok(Err(_)) => Err(io::Error::from(io::ErrorKind::BrokenPipe)), // client went away
            Err(_elapsed) => Err(io::Error::from(io::ErrorKind::TimedOut)), // not read in time
        }
    }

    /// Stages the terminal fields until the producer has succeeded.
    pub fn trailers(&mut self, trailers: HeaderMap) {
        self.trailers = Some(trailers);
    }

    fn finish(&mut self) -> io::Result<()> {
        io::Write::flush(self)?;
        if let Some(trailers) = self.trailers.take() {
            self.send(Frame::trailers(trailers))?;
        }
        Ok(())
    }
}

impl io::Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Split at the chunk boundary (the review of 3 October 2026, P4): a cached result
        // arrives in one write, and went out as one chunk of up to 16 MiB.
        let mut rest = bytes;
        while !rest.is_empty() {
            let taken = rest.len().min(CHUNK_BYTES - self.buffer.len());
            self.buffer.extend_from_slice(&rest[..taken]);
            rest = &rest[taken..];
            if self.buffer.len() >= CHUNK_BYTES {
                self.flush()?;
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK_BYTES));
        self.send(Frame::data(Bytes::from(chunk)))
    }
}

/// Cancels the token when dropped, i.e. when the response body is dropped: at the end of
/// the stream (harmless) or when the client disconnects; and stops the deadline's timer.
struct CancelOnDrop(CancellationToken, tokio::task::AbortHandle);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
        self.1.abort();
    }
}

/// Runs `produce` on a blocking thread and streams what it writes as the response body,
/// within `pace`; or, for a reader of every graph over HTTP/1.0, holds it whole, charged to
/// the envelope `memory` opens, which `produce` is handed as [`ChannelWriter::holding`].
/// A reader of some graphs' accepted answer streams. `produce` must honour
/// `cancellation`.
pub async fn stream_blocking(
    pace: Pace,
    cancellation: CancellationToken,
    memory: impl FnOnce() -> Envelope,
    reader: Reader,
    media_type: &'static str,
    timeout_message: &'static str,
    produce: impl FnOnce(&mut ChannelWriter) -> Result<(), ApiError> + Send + 'static,
) -> Result<Response, ApiError> {
    if let Some(response) = reader.refuse_unframed() {
        return Ok(response);
    }
    // Set for every request of the server's router (`app::screen`), which is reached only
    // through `build_app`; unset only for a handler a test calls without it, which streams.
    let whole = WHOLE.try_with(|whole| *whole).unwrap_or(false);
    // How long an answer held whole waits for its client to take each frame: a stall
    // bound's own, or the time a query was given.
    let stall = match pace {
        Pace::Stall { stall, .. } => stall,
        Pace::Total(deadline) => deadline.saturating_duration_since(Instant::now()),
    };
    let (sender, mut receiver) = mpsc::channel::<Message>(CHANNEL_CHUNKS);
    let start = Arc::new(Start::default());
    let outcome = Arc::new(Outcome::default());
    // Opened only for an answer held whole, before the producer starts: it draws on it too.
    let memory = whole.then(memory);
    let mut writer = ChannelWriter {
        sender,
        buffer: Vec::with_capacity(CHUNK_BYTES),
        trailers: None,
        pace,
        start: Arc::clone(&start),
        runtime: tokio::runtime::Handle::current(),
        holding: memory.clone(),
    };
    let ended = Arc::clone(&outcome);
    tokio::task::spawn_blocking(move || {
        let result = produce(&mut writer).and_then(|()| {
            writer
                .finish()
                .map_err(|error| ApiError::internal(error.to_string()))
        });
        // Recorded before the sender goes (with `writer`): the receiver reads it once the
        // channel closes, however full the channel was.
        ended.set(result);
    });
    // The first deadline as a computation deadline: evaluation stops at it whether or not
    // the body is being polled (the review of 3 October 2026, P5). Under a stall bound only
    // a stream that hasn't started by then; after that, its chunks' waits bound it.
    let (deadline, total) = match pace {
        Pace::Total(deadline) => (deadline, true),
        Pace::Stall { first, .. } => (first, false),
    };
    let watchdog = cancellation.clone();
    let watchdog = tokio::spawn(async move {
        tokio::time::sleep_until(deadline).await;
        if total || start.late() {
            watchdog.cancel();
        }
    })
    .abort_handle();

    let guard = CancelOnDrop(cancellation, watchdog);
    if let Some(memory) = memory {
        // The first deadline bounds all of it: no chunk goes out to start a stall bound. On
        // every exit but the answer's end, `guard` cancels the producer.
        let held = hold(receiver, &outcome, deadline, memory, timeout_message).await?;
        return Ok(respond(Body::new(Held::new(held, stall)), media_type));
    }
    let first = match tokio::time::timeout_at(deadline, receiver.recv()).await {
        Err(_elapsed) => return Err(ApiError::timeout(timeout_message)), // guard cancels
        Ok(Some(frame)) => Some(frame),
        // Ended without output: a success is an empty body, a failure keeps its status.
        Ok(None) => {
            outcome.take()?;
            outcome.set(Ok(())); // the body's end reads it again
            None
        }
    };

    struct State {
        receiver: mpsc::Receiver<Message>,
        first: Option<Frame<Bytes>>,
        outcome: Arc<Outcome>,
        guard: CancelOnDrop,
        pace: Pace,
        timeout_message: &'static str,
    }
    let state = State {
        receiver,
        first,
        outcome,
        guard,
        pace,
        timeout_message,
    };
    let frames = futures_util::stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        if let Some(frame) = state.first.take() {
            return Some((Ok(frame), Some(state)));
        }
        let until = state.pace.until(true);
        match tokio::time::timeout_at(until, state.receiver.recv()).await {
            Ok(Some(frame)) => Some((Ok(frame), Some(state))),
            // The producer is gone: a clean end only if it finished.
            Ok(None) => match state.outcome.take() {
                Ok(()) => None,
                Err(error) => Some((Err(io::Error::other(error.to_string())), None)),
            },
            Err(_elapsed) => {
                state.guard.0.cancel();
                Some((Err(io::Error::other(state.timeout_message)), None))
            }
        }
    });

    let body = Body::new(http_body_util::StreamBody::new(frames));
    Ok(respond(body, media_type))
}

fn respond(body: Body, media_type: &'static str) -> Response {
    let mut response = (StatusCode::OK, body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    response
}

/// Receives all the producer writes before any of it is sent, charging it to `memory`:
/// the whole answer, or the status of what stopped it (its deadline, the producer's error, a
/// refusal of the memory).
async fn hold(
    mut receiver: mpsc::Receiver<Message>,
    outcome: &Outcome,
    deadline: Instant,
    memory: Envelope,
    timeout_message: &'static str,
) -> Result<Holding, ApiError> {
    let bound = memory.limit();
    let mut held = Holding {
        frames: VecDeque::new(),
        left: 0,
        charged: 0,
        envelope: memory,
        moved: Instant::now(),
        cut: false,
    };
    loop {
        let received = tokio::time::timeout_at(deadline, receiver.recv()).await;
        // A wait with a deadline takes what is ready first: a hold that resumes past its
        // deadline with a frame or the producer's end ready would take it (an answer, a 404,
        // a 413). Past the deadline nothing more is taken: no answer.
        if Instant::now() >= deadline {
            return Err(ApiError::timeout(timeout_message));
        }
        // Where a test stops the hold between its clock check and its charge.
        #[cfg(test)]
        tests::step().await;
        match received {
            Err(_elapsed) => return Err(ApiError::timeout(timeout_message)),
            Ok(None) => break,
            Ok(Some(frame)) => {
                if let Some(data) = frame.data_ref() {
                    let bytes = data.len();
                    held.envelope.charge_beside(bytes).map_err(|refused| {
                        // A charge that waited on the budget past the deadline came too late
                        // to be answered for, whatever refused it. A producer that ended with
                        // its own error while this frame was queued stopped the answer first.
                        if Instant::now() >= deadline {
                            return ApiError::timeout(timeout_message);
                        }
                        outcome
                            .failed()
                            .unwrap_or_else(|| too_large(refused, bound))
                    })?;
                    held.charged += bytes;
                    held.left += bytes as u64;
                }
                held.frames.push_back(frame);
            }
        }
    }
    // The producer's error as it told it: its memory refusals it told through
    // `ChannelWriter::refused`, by the cause the budget judged at each refusal.
    outcome.take()?;
    Ok(held)
}

/// The refusal of an answer held whole past `bound`, its envelope's limit; or, where the
/// memory all queries share has no room for it, a 503 that names that memory and no amounts:
/// what it holds is other requests' work too, which its reader may not see. Where the
/// process's memory couldn't be read, a 503 that says so. No memory refusal is told when to
/// try again (no `Retry-After`).
fn too_large(refused: BudgetExceeded, bound: usize) -> ApiError {
    const SHARED: &str = "(budgets.total_query_memory, NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES)";
    if refused.unread {
        return ApiError::unavailable(format!("{refused} {SHARED}"));
    }
    if refused.shared {
        return ApiError::unavailable(format!("the server's query memory is in use {SHARED}"));
    }
    too_large_for_http10(bound)
}

/// [`ChannelWriter::refused`] of a writer that holds its answer in `holding` (`None`: it
/// streams).
fn refusal_told(
    holding: Option<&Envelope>,
    error: StoreError,
    otherwise: impl FnOnce(StoreError) -> ApiError,
) -> ApiError {
    match holding {
        Some(envelope)
            if error
                .memory_refusal()
                .is_some_and(|refused| refused.for_held) =>
        {
            too_large_for_http10(envelope.limit())
        }
        _ => otherwise(error),
    }
}

/// The 413 of an answer held whole past `bound`, its envelope's limit.
fn too_large_for_http10(bound: usize) -> ApiError {
    ApiError::payload_too_large(format!(
        "This answer is larger than the {} an HTTP/1.0 request can be answered with: HTTP/1.0 \
         has no way to mark a streamed answer as cut, so the server sends it only whole. Ask \
         over HTTP/1.1 or HTTP/2, which stream it. Behind a reverse proxy, set the proxy's \
         upstream HTTP version to 1.1 (for nginx: `proxy_http_version 1.1;`).",
        format_size(bound as u64)
    ))
}

/// An answer held whole: its frames, the bytes of them left to send (its exact length, so it
/// goes out with a `Content-Length`), and the memory they hold, returned as they go out.
struct Holding {
    frames: VecDeque<Message>,
    left: u64,
    /// The bytes of `frames` charged to `envelope` beside the producer's work
    /// ([`Envelope::charge_beside`]), released as they go out.
    charged: usize,
    envelope: Envelope,
    /// When the client last took a frame (at first: when the answer was ready).
    moved: Instant,
    /// The client took no frame for the stall bound: the frames and their memory are gone.
    cut: bool,
}

impl Holding {
    /// Lets go of what is left: the frames and the memory they hold, and the credit their
    /// envelope keeps at the memory all queries share. An answer all sent isn't cut: its end
    /// stays clean.
    fn cut(&mut self) {
        self.cut = !self.frames.is_empty();
        self.frames.clear();
        self.envelope
            .release_beside(std::mem::take(&mut self.charged));
        self.envelope.return_credit();
    }
}

/// An answer dropped before it is sent (refused, or its request gone) lets go of its memory.
impl Drop for Holding {
    fn drop(&mut self) {
        self.envelope.release_beside(self.charged);
    }
}

fn lock(holding: &Mutex<Holding>) -> std::sync::MutexGuard<'_, Holding> {
    holding.lock().unwrap_or_else(|p| p.into_inner())
}

/// The body of an answer held whole. Its client must take each frame within `stall` of the
/// one before (of the answer being ready, the first): past it, a timer of its own lets go of
/// what is left, whether or not the body is polled (a client that stops reading without
/// leaving doesn't poll it), and the body ends with an error, short of its length. On drop
/// (sent, or its client gone), it lets go of all it holds.
struct Held {
    holding: Arc<Mutex<Holding>>,
    stall: Duration,
    watch: tokio::task::AbortHandle,
}

impl Held {
    fn new(mut holding: Holding, stall: Duration) -> Self {
        // The first frame's window opens now, with the answer ready.
        holding.moved = Instant::now();
        let holding = Arc::new(Mutex::new(holding));
        // Weak: the body alone keeps what it holds, which goes with it when it is dropped,
        // not later, when the runtime drops the aborted timer.
        let watched = Arc::downgrade(&holding);
        let watch = tokio::spawn(async move {
            loop {
                let Some(until) = watched.upgrade().map(|held| lock(&held).moved + stall) else {
                    return;
                };
                tokio::time::sleep_until(until).await;
                let Some(held) = watched.upgrade() else {
                    return;
                };
                let mut holding = lock(&held);
                if holding.moved + stall <= Instant::now() {
                    holding.cut();
                    return;
                }
            }
        })
        .abort_handle();
        Self {
            holding,
            stall,
            watch,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.watch.abort();
        lock(&self.holding).cut();
    }
}

impl http_body::Body for Held {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let mut holding = lock(&self.holding);
        // Past the stall bound nothing more is taken, though the timer that lets go of what
        // is left hasn't run yet.
        if !holding.cut && holding.moved + self.stall <= Instant::now() {
            holding.cut();
        }
        if holding.cut {
            return Poll::Ready(Some(Err(io::Error::from(io::ErrorKind::TimedOut))));
        }
        let frame = holding.frames.pop_front();
        if let Some(data) = frame.as_ref().and_then(Frame::data_ref) {
            let bytes = data.len();
            holding.left -= bytes as u64;
            holding.charged -= bytes;
            holding.envelope.release_beside(bytes);
        }
        holding.moved = Instant::now();
        Poll::Ready(frame.map(Ok))
    }

    fn is_end_stream(&self) -> bool {
        let holding = lock(&self.holding);
        holding.frames.is_empty() && !holding.cut
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(lock(&self.holding).left)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio::sync::oneshot;

    use super::*;

    thread_local! {
        /// The step [`armed`] makes the next hold on this thread take, once.
        static STEP: RefCell<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>> =
            const { RefCell::new(None) };
    }

    /// The hold's step between its clock check and its charge of what it received: where a
    /// test armed one on this thread ([`armed`]), it says it got there and waits to be let on.
    pub(super) async fn step() {
        let Some((reached, go)) = STEP.with(|step| step.borrow_mut().take()) else {
            return;
        };
        let _ = reached.send(());
        let _ = go.await;
    }

    /// Arms a step for the next hold on this thread (a current-thread runtime's): the hold
    /// got there, and what lets it on.
    fn armed() -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached, at_step) = oneshot::channel();
        let (go, gone) = oneshot::channel();
        STEP.with(|step| *step.borrow_mut() = Some((reached, gone)));
        (at_step, go)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_large_write_goes_out_in_chunks_of_at_most_64_kib() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let response = stream_blocking(
            Pace::Total(deadline),
            CancellationToken::new(),
            Envelope::unlimited,
            Reader::Every,
            "text/plain",
            "timeout",
            |writer| {
                io::Write::write_all(writer, &vec![b'x'; 200 * 1024 + 5])
                    .map_err(|error| ApiError::internal(error.to_string()))
            },
        )
        .await
        .unwrap();
        let mut stream = response.into_body().into_data_stream();
        let mut sizes = Vec::new();
        while let Some(chunk) = stream.next().await {
            sizes.push(chunk.unwrap().len());
        }
        assert_eq!(
            sizes,
            vec![CHUNK_BYTES, CHUNK_BYTES, CHUNK_BYTES, 8 * 1024 + 5]
        );
    }

    /// Framing can end with a trailer only after the producer succeeds.
    #[tokio::test]
    async fn a_late_error_precedes_the_terminal_trailer() {
        use http_body_util::BodyExt;

        for version in [Version::HTTP_11, Version::HTTP_2] {
            for succeeds in [false, true] {
                let response = answering(
                    version,
                    stream_blocking(
                        Pace::Total(Instant::now() + Duration::from_secs(30)),
                        CancellationToken::new(),
                        Envelope::unlimited,
                        Reader::Every,
                        "text/plain",
                        "timeout",
                        move |writer| {
                            io::Write::write_all(writer, &vec![b'x'; CHUNK_BYTES])
                                .map_err(|error| ApiError::internal(error.to_string()))?;
                            let mut trailers = HeaderMap::new();
                            trailers.insert("x-result-checksum", HeaderValue::from_static("check"));
                            writer.trailers(trailers);
                            if succeeds {
                                Ok(())
                            } else {
                                Err(ApiError::internal("producer failed"))
                            }
                        },
                    ),
                )
                .await
                .unwrap();
                let mut body = response.into_body();
                let data = body.frame().await.unwrap().unwrap().into_data().unwrap();
                assert_eq!(data.len(), CHUNK_BYTES);
                let terminal = body.frame().await.unwrap();
                if succeeds {
                    assert_eq!(
                        terminal.unwrap().into_trailers().unwrap()["x-result-checksum"],
                        "check"
                    );
                } else {
                    assert!(
                        terminal.is_err(),
                        "{version:?}: failure was framed as success"
                    );
                }
                assert!(body.frame().await.is_none());
            }
        }
    }

    /// On tokio's paused clock: the deadline passes when the test advances the clock, and
    /// the test waits for the producer's own signal that it stopped, so no timing under
    /// load decides it (it failed once under the gate's parallel load when it slept 1.5 s
    /// of real time and then looked).
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn an_unread_body_stops_its_producer_at_the_deadline() {
        let deadline = Instant::now() + Duration::from_millis(300);
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let (stopped, on_stop) = std::sync::mpsc::channel();
        let response = stream_blocking(
            Pace::Total(deadline),
            cancellation,
            Envelope::unlimited,
            Reader::Every,
            "text/plain",
            "timeout",
            move |writer| {
                // Writes until it is stopped: by the channel refusing, or by cancellation.
                let result = loop {
                    if token.is_cancelled() {
                        break Ok(());
                    }
                    if let Err(error) = io::Write::write_all(writer, &[b'x'; 4096]) {
                        break Err(ApiError::internal(error.to_string()));
                    }
                };
                stopped.send(()).expect("the test waits");
                result
            },
        )
        .await
        .unwrap();
        // The response is held, its body never polled: the channel fills and stays full,
        // and the producer waits for room until the deadline. The clock reaches it now.
        tokio::time::advance(Duration::from_millis(301)).await;
        // The producer lets go: its wait for room ends, and the deadline's timer cancels
        // it. Received on a blocking thread, so this runtime keeps driving the timers the
        // producer waits on; the bound is a guard against a hang, not part of the check.
        let stopped = tokio::task::spawn_blocking(move || {
            on_stop.recv_timeout(std::time::Duration::from_secs(120))
        })
        .await
        .unwrap();
        assert!(stopped.is_ok(), "the producer still waits for a reader");
        drop(response);
    }

    /// Under a stall bound, on tokio's paused clock: a reader that takes a chunk every
    /// 200 ms gets all of a body that takes 4 s, far past the 300 ms first deadline; one
    /// that stops reading stops the producer once the clock passes the bound. A task that
    /// only yields keeps the runtime busy, so time moves only when the test advances it (an
    /// idle paused runtime jumps to its next timer, which would fire the bound by itself).
    /// The producer runs on a thread of its own and starts each wait when it gets there, so
    /// the clock moves only once it has sent what the channel's room allows (the slow
    /// reader), or in steps until it has stopped (the stalled one): no step of the test
    /// depends on how fast that thread runs.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_stall_bound_cuts_off_a_stalled_reader_but_not_a_slow_one() {
        use std::sync::atomic::AtomicUsize;

        let busy = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        // Real time for the producer's thread, without letting the paused clock move.
        let real =
            |ms| tokio::task::spawn_blocking(move || std::thread::sleep(Duration::from_millis(ms)));
        let stall = Duration::from_millis(300);
        let chunks = 20;
        let sent = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&sent);
        let response = stream_blocking(
            Pace::stalled_after(stall),
            CancellationToken::new(),
            Envelope::unlimited,
            Reader::Every,
            "text/plain",
            "timeout",
            move |writer| {
                for _ in 0..chunks {
                    // One chunk exactly: it goes into the channel before the write returns.
                    io::Write::write_all(writer, &[b'x'; CHUNK_BYTES])
                        .map_err(|error| ApiError::internal(error.to_string()))?;
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            },
        )
        .await
        .unwrap();
        let mut body = response.into_body().into_data_stream();
        let mut read = 0;
        while let Some(chunk) = body.next().await {
            chunk.expect("a slow reader isn't cut off");
            read += 1;
            // The producer has sent all the room allows (the chunks read, and the channel
            // full again), so its next wait starts on the clock as it is now.
            let room = chunks.min(read + CHANNEL_CHUNKS);
            for _ in 0..12_000 {
                if sent.load(Ordering::SeqCst) >= room {
                    break;
                }
                real(10).await.unwrap();
            }
            assert!(
                sent.load(Ordering::SeqCst) >= room,
                "the producer doesn't send"
            );
            tokio::time::advance(Duration::from_millis(200)).await;
        }
        assert_eq!(read, chunks);

        let (stopped, on_stop) = std::sync::mpsc::channel();
        let response = stream_blocking(
            Pace::stalled_after(stall),
            CancellationToken::new(),
            Envelope::unlimited,
            Reader::Every,
            "text/plain",
            "timeout",
            move |writer| {
                let result = loop {
                    if let Err(error) = io::Write::write_all(writer, &[b'x'; 4096]) {
                        break Err(ApiError::internal(error.to_string()));
                    }
                };
                let _ = stopped.send(());
                result
            },
        )
        .await
        .unwrap();
        let mut body = response.into_body().into_data_stream();
        body.next().await.unwrap().unwrap();
        // The reader stops: the channel fills, and the producer's wait for room ends once
        // the clock passes the bound. In steps of the bound, each with real time for the
        // producer's thread: it stops at the first step after it began to wait.
        let mut steps = 0;
        let ended = loop {
            tokio::time::advance(stall + Duration::from_millis(1)).await;
            steps += 1;
            real(50).await.unwrap();
            if on_stop.try_recv().is_ok() {
                break true;
            }
            if steps == 2_400 {
                break false;
            }
        };
        assert!(ended, "the producer still waits for a stalled reader");
        drop(body);
        busy.abort();
    }

    /// A client that stops reading until the producer has given up, then drains
    /// what was buffered, gets a failed body, never a clean end, though the failure came
    /// while the channel was full. The producer's sender is watched (weakly) until it is
    /// gone, so no timing decides the assertion.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_failure_while_the_channel_is_full_is_not_a_clean_end() {
        use http_body_util::BodyExt;

        let busy = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let stall = Duration::from_millis(300);
        let (weak_sender, weak) = tokio::sync::oneshot::channel();
        let response = stream_blocking(
            Pace::stalled_after(stall),
            CancellationToken::new(),
            Envelope::unlimited,
            Reader::Every,
            "text/plain",
            "timeout",
            move |writer| {
                weak_sender
                    .send(writer.sender.downgrade())
                    .expect("the test waits");
                for _ in 0..32 {
                    io::Write::write_all(writer, &[b'x'; CHUNK_BYTES])
                        .map_err(|error| ApiError::internal(error.to_string()))?;
                }
                Ok(())
            },
        )
        .await
        .unwrap();
        let weak = weak.await.unwrap();
        for _ in 0..120_000 {
            if weak.strong_count() == 0 {
                break;
            }
            tokio::time::advance(stall + Duration::from_millis(1)).await;
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(1)))
                .await
                .unwrap();
        }
        assert_eq!(weak.strong_count(), 0, "the producer didn't give up");
        let mut body = response.into_body();
        let (mut bytes, mut failed) = (0, false);
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => bytes += frame.into_data().map_or(0, |data| data.len()),
                Err(_) => failed = true,
            }
        }
        busy.abort();
        assert!(bytes < 32 * CHUNK_BYTES, "the producer finished after all");
        assert!(
            failed,
            "{bytes} of {} bytes, and a clean end",
            32 * CHUNK_BYTES
        );
    }

    /// A producer that finished without writing anything is a clean, empty body (the end of
    /// the body reads the outcome the first receive already read).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_producer_that_writes_nothing_gives_an_empty_body() {
        let response = stream_blocking(
            Pace::stalled_after(Duration::from_secs(30)),
            CancellationToken::new(),
            Envelope::unlimited,
            Reader::Every,
            "text/plain",
            "timeout",
            |_| Ok(()),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await;
        assert_eq!(body.expect("a clean end").len(), 0);
    }

    /// The first chunk's start is decided once. In both orders exactly one side
    /// wins: a producer that claimed the start is never declared late, and a stream declared
    /// late never starts.
    #[test]
    fn the_first_chunk_and_the_first_deadline_decide_once() {
        let start = Start::default();
        assert!(start.begin(), "the producer first: started");
        assert!(!start.late(), "then the deadline finds it started");
        assert!(start.begin(), "and later chunks go on");

        let start = Start::default();
        assert!(start.late(), "the deadline first: late");
        assert!(!start.begin(), "then the producer may not start");
        assert!(!start.started());
    }

    /// A producer that writes `bytes`, says so, waits until it is cancelled and ends with the
    /// stop's error, as a query an operator cancels does; it says when it has ended. Never
    /// cancelled, it gives up after 120 s of real time without saying so, so that a test
    /// whose cancellation fails fails, and its runtime, which waits for this blocking thread
    /// when it is dropped, still ends.
    fn written_then_stopped(
        bytes: usize,
        token: CancellationToken,
        wrote: std::sync::mpsc::Sender<()>,
        ended: std::sync::mpsc::Sender<()>,
    ) -> impl FnOnce(&mut ChannelWriter) -> Result<(), ApiError> + Send + 'static {
        move |writer| {
            let give_up = std::time::Instant::now() + Duration::from_secs(120);
            let written = io::Write::write_all(writer, &vec![b'x'; bytes])
                .and_then(|()| io::Write::flush(writer));
            let _ = wrote.send(());
            while written.is_ok() && !token.is_cancelled() {
                if std::time::Instant::now() >= give_up {
                    return Err(ApiError::timeout("never stopped"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = ended.send(());
            Err(ApiError::timeout("stopped"))
        }
    }

    /// Waits, in real time, until `memory` holds at least `bytes`.
    async fn until_held(memory: &Envelope, bytes: usize) {
        for _ in 0..60_000 {
            if memory.used() >= bytes {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("{} bytes held, not {bytes}", memory.used());
    }

    /// Over HTTP/1.0, a stop after part of the answer was written gets the stop's status, not
    /// a 200, and what was held goes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_answer_stopped_before_its_end_gets_the_stops_status_and_releases_its_memory() {
        let memory = Envelope::new(Some(1 << 30), None);
        let token = CancellationToken::new();
        let ((wrote, written), (ended, _)) =
            (std::sync::mpsc::channel(), std::sync::mpsc::channel());
        let held = memory.clone();
        let answer = tokio::spawn(WHOLE.scope(
            true,
            stream_blocking(
                Pace::Total(Instant::now() + Duration::from_secs(120)),
                token.clone(),
                move || held,
                Reader::Every,
                "text/plain",
                "timeout",
                written_then_stopped(3 * CHUNK_BYTES, token.clone(), wrote, ended),
            ),
        ));
        tokio::task::spawn_blocking(move || written.recv())
            .await
            .unwrap()
            .unwrap();
        until_held(&memory, 3 * CHUNK_BYTES).await;
        token.cancel();
        let stopped = answer
            .await
            .unwrap()
            .expect_err("a stopped answer is no answer");
        assert_eq!(
            stopped.into_response().status(),
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(memory.used(), 0);
    }

    /// Over HTTP/1.0, a client that leaves while its answer is held (the request's future is
    /// dropped) stops the producer, and what was held goes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_answer_whose_client_leaves_stops_its_producer_and_releases_its_memory() {
        let memory = Envelope::new(Some(1 << 30), None);
        let token = CancellationToken::new();
        let ((wrote, written), (ended, on_end)) =
            (std::sync::mpsc::channel(), std::sync::mpsc::channel());
        let held = memory.clone();
        let answer = tokio::spawn(WHOLE.scope(
            true,
            stream_blocking(
                Pace::Total(Instant::now() + Duration::from_secs(120)),
                token.clone(),
                move || held,
                Reader::Every,
                "text/plain",
                "timeout",
                written_then_stopped(3 * CHUNK_BYTES, token, wrote, ended),
            ),
        ));
        tokio::task::spawn_blocking(move || written.recv())
            .await
            .unwrap()
            .unwrap();
        until_held(&memory, 3 * CHUNK_BYTES).await;
        answer.abort();
        assert!(answer.await.unwrap_err().is_cancelled());
        assert_eq!(memory.used(), 0);
        let ended = tokio::task::spawn_blocking(move || {
            on_end.recv_timeout(std::time::Duration::from_secs(120))
        })
        .await
        .unwrap();
        assert!(
            ended.is_ok(),
            "the producer still runs for a client that left"
        );
    }

    /// Over HTTP/1.0, a stall bound's first deadline bounds the whole read: a producer still
    /// going past it gets 408 at that deadline, not at its end, though it wrote before it and
    /// keeps writing well within the stall bound (streamed, that would have started a 200 the
    /// stall bound then keeps going, to its end). The clock moves only as the test moves it
    /// (a blocking producer holds it still otherwise): a chunk each quarter of the bound, for
    /// three bounds, the producer open throughout; each quarter only once the response task
    /// has taken that chunk (its envelope charged for it) or has answered.
    #[tokio::test(start_paused = true)]
    async fn a_held_reads_first_deadline_bounds_all_of_it() {
        let memory = Envelope::new(Some(1 << 30), None);
        let token = CancellationToken::new();
        let stop = token.clone();
        let held = memory.clone();
        let (go, step) = std::sync::mpsc::channel::<()>();
        let (wrote, mut written) = mpsc::unbounded_channel::<()>();
        let started = Instant::now();
        let answer = tokio::spawn(WHOLE.scope(true, async move {
            let answer = stream_blocking(
                Pace::stalled_after(Duration::from_secs(1)),
                token,
                move || held,
                Reader::Every,
                "text/plain",
                "timeout",
                // A chunk at each step it is given, until it is stopped or let go.
                move |writer| {
                    while step.recv().is_ok() {
                        if stop.is_cancelled() {
                            return Err(ApiError::timeout("stopped"));
                        }
                        io::Write::write_all(writer, &[b'x'; CHUNK_BYTES])
                            .map_err(|error| ApiError::internal(error.to_string()))?;
                        let _ = wrote.send(());
                    }
                    Ok(())
                },
            )
            .await;
            let status = answer.err().map(|stopped| stopped.into_response().status());
            (status, Instant::now())
        }));
        // Real time, which the paused clock doesn't hold: a hold or a producer that stops
        // making progress fails the test past it, rather than hanging it (the clock moves
        // only as this loop moves it, and a blocking producer keeps it from moving alone).
        let give_up = std::time::Instant::now() + Duration::from_secs(120);
        let on_time = || std::time::Instant::now() < give_up;
        let mut stuck = false;
        'steps: for taken in 1..=12 {
            if go.send(()).is_err() {
                break;
            }
            loop {
                match written.try_recv() {
                    Ok(()) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => break 'steps,
                    Err(mpsc::error::TryRecvError::Empty) if on_time() => {
                        tokio::task::yield_now().await;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        stuck = true;
                        break 'steps;
                    }
                }
            }
            // The order is the test's, not the runtime's: the response task has taken this
            // chunk, or has answered, before the clock moves.
            while memory.used() < taken * CHUNK_BYTES && !answer.is_finished() {
                if !on_time() {
                    stuck = true;
                    break 'steps;
                }
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_millis(250)).await;
        }
        // Let go, the producer's step ends; stopped, the answer drops its frames' receiver.
        drop(go);
        if stuck {
            answer.abort();
            panic!("the held read made no progress in 120 s of real time");
        }
        let (status, ended) = answer.await.unwrap();
        assert_eq!(status, Some(StatusCode::REQUEST_TIMEOUT));
        let waited = ended - started;
        assert!(
            waited <= Duration::from_secs(1),
            "the read was stopped after {waited:?}, past its first deadline"
        );
        assert_eq!(memory.used(), 0);
    }

    /// Work of a query's that fits a 1 MiB limit alone, but not beside two chunks held.
    const WORK: usize = (1 << 20) - CHUNK_BYTES;

    /// Over HTTP/1.0, the answer held for a reader of every graph by a producer whose own work (a query's)
    /// draws on the same envelope as the hold: it writes `CHUNK_BYTES * 2`, waits until the
    /// hold holds them, charges `work` there, which must be refused, and ends with its
    /// refusal as its writer tells it ([`ChannelWriter::refused`]; `stopped` where the
    /// writer leaves it to the route). The response; the envelope is checked to hold nothing
    /// after it.
    async fn refused_beside_its_held_answer(
        memory: Envelope,
        work: usize,
        stopped: ApiError,
    ) -> Response {
        let charged = memory.clone();
        let held = memory.clone();
        let answer = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::Total(Instant::now() + Duration::from_secs(120)),
                    CancellationToken::new(),
                    move || held,
                    Reader::Every,
                    "text/plain",
                    "timeout",
                    move |writer| {
                        io::Write::write_all(writer, &vec![b'x'; 2 * CHUNK_BYTES])
                            .and_then(|()| io::Write::flush(writer))
                            .map_err(|error| ApiError::internal(error.to_string()))?;
                        // Its work grows once the hold holds what it wrote, and is refused.
                        for _ in 0..60_000 {
                            if charged.used() >= 2 * CHUNK_BYTES {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        let part = charged.part();
                        let refused = part.charge(work).expect_err("its work fits after all");
                        Err(writer.refused(StoreError::from(refused), |_| stopped))
                    },
                ),
            )
            .await;
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
        answer
            .expect_err("its work refused, no answer")
            .into_response()
    }

    /// Over HTTP/1.0, a query's work and its answer held whole draw on one envelope: refused
    /// there while part of the answer is held, the query was refused for holding it. It gets
    /// the 413 of an answer too large for HTTP/1.0, which tells the client to stream it, not
    /// the query's own, which tells it to raise the query's memory (over HTTP/1.1 the same
    /// query in the same envelope would be answered). Where the memory all queries share
    /// refused it, its 503 is the producer's as it is, without `Retry-After`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_producers_work_refused_beside_its_held_answer_is_the_answers_refusal() {
        let own = refused_beside_its_held_answer(
            Envelope::new(Some(1 << 20), None),
            WORK,
            ApiError::payload_too_large("the query's own (budgets.query_memory)"),
        )
        .await;
        assert_eq!(own.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = axum::body::to_bytes(own.into_body(), usize::MAX)
            .await
            .unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let detail = problem["detail"].as_str().unwrap_or_default();
        assert!(
            detail.starts_with("This answer is larger than the 1 MiB an HTTP/1.0 request"),
            "{detail}"
        );

        let shared = refused_beside_its_held_answer(
            Envelope::new(Some(1 << 30), Some(nrese_exec::SharedBudget::new(1 << 20))),
            WORK,
            ApiError::unavailable("the query's own (budgets.total_query_memory)"),
        )
        .await;
        assert_eq!(shared.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(shared.headers().get(header::RETRY_AFTER), None);
    }

    /// Over HTTP/1.0, an answer the memory all queries share has no room to hold (a frame past
    /// that memory's whole limit) gets 503 without `Retry-After`, as every memory refusal
    /// does.
    #[tokio::test]
    async fn a_held_answer_past_the_whole_shared_memory_is_not_told_to_retry() {
        let shared = nrese_exec::SharedBudget::new(CHUNK_BYTES / 2);
        let memory = Envelope::new(Some(1 << 30), Some(shared));
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        sender
            .try_send(Frame::data(Bytes::from(vec![b'x'; CHUNK_BYTES])))
            .unwrap();
        drop(sender);
        let outcome = Outcome::default();
        outcome.set(Ok(()));
        let deadline = Instant::now() + Duration::from_secs(30);
        let refused = hold(receiver, &outcome, deadline, memory.clone(), "timeout")
            .await
            .err()
            .map(IntoResponse::into_response)
            .expect("no room in the shared memory, no answer");
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refused.headers().get(header::RETRY_AFTER), None);
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
    }

    /// Over HTTP/1.0, a producer's work refused before any of its answer was held keeps its
    /// own refusal, though the hold holds the answer it wrote after it by the end: that
    /// refusal was the work's alone, the client's to change (the query's own 413).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_producers_work_refused_before_its_answer_is_held_keeps_its_own_refusal() {
        let memory = Envelope::new(Some(1 << 20), None);
        let charged = memory.clone();
        let held = memory.clone();
        let answer = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::Total(Instant::now() + Duration::from_secs(120)),
                    CancellationToken::new(),
                    move || held,
                    Reader::Every,
                    "text/plain",
                    "timeout",
                    move |writer| {
                        // Refused with nothing held, then it writes what it had.
                        let part = charged.part();
                        let refused = part.charge(2 << 20).expect_err("its work fits after all");
                        io::Write::write_all(writer, &vec![b'x'; 2 * CHUNK_BYTES])
                            .and_then(|()| io::Write::flush(writer))
                            .map_err(|error| ApiError::internal(error.to_string()))?;
                        Err(writer.refused(StoreError::from(refused), |_| own()))
                    },
                ),
            )
            .await;
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
        let own = answer
            .expect_err("its work refused, no answer")
            .into_response();
        assert_eq!(own.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = axum::body::to_bytes(own.into_body(), usize::MAX)
            .await
            .unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["detail"], "the query's own (budgets.query_memory)");
    }

    /// Over HTTP/1.0, a producer that queued a frame and then had its work refused before the
    /// hold took that frame (a CONSTRUCT refused reserving its next triple) keeps its own
    /// refusal, though the hold holds the frame by the end: what is held beside the work
    /// counts as it was at the refusal itself. Step by step: the frame queued, the refusal,
    /// the producer's end; then the hold.
    #[tokio::test]
    async fn a_producers_work_refused_while_its_queued_frame_is_unheld_keeps_its_own_refusal() {
        let memory = Envelope::new(Some(1 << 20), None);
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        sender
            .try_send(Frame::data(Bytes::from(vec![b'x'; CHUNK_BYTES])))
            .unwrap();
        let refused = memory.part().charge(2 << 20).expect_err("its work fits");
        let outcome = Outcome::default();
        outcome.set(Err(told(&memory, refused)));
        drop(sender);
        let deadline = Instant::now() + Duration::from_secs(30);
        let stopped = hold(receiver, &outcome, deadline, memory.clone(), "timeout")
            .await
            .err()
            .map(IntoResponse::into_response)
            .expect("its work refused, no answer");
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
        let body = axum::body::to_bytes(stopped.into_body(), usize::MAX)
            .await
            .unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["detail"], "the query's own (budgets.query_memory)");
    }

    /// The query's own 413, as its route tells it (`sparql::map_query_error`).
    fn own() -> ApiError {
        ApiError::payload_too_large("the query's own (budgets.query_memory)")
    }

    /// What a producer that holds its answer in `memory` for a reader of every graph ends
    /// with when its work is `refused` ([`ChannelWriter::refused`]).
    fn told(memory: &Envelope, refused: BudgetExceeded) -> ApiError {
        refusal_told(Some(memory), StoreError::from(refused), |_| own())
    }

    /// The detail of the refusal `stopped` is mapped to.
    async fn detail(stopped: ApiError) -> serde_json::Value {
        let body = axum::body::to_bytes(stopped.into_response().into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["detail"].clone()
    }

    /// Over HTTP/1.0, a producer (a CONSTRUCT) with one small frame of its answer held whose
    /// next work needs more than its whole envelope (2 MiB of 1 MiB) keeps its own refusal,
    /// the query's 413: with nothing held it would be refused too, so holding isn't its cause.
    #[tokio::test]
    async fn a_producers_work_past_its_whole_envelope_beside_a_held_frame_keeps_its_own_refusal() {
        let memory = Envelope::new(Some(1 << 20), None);
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        sender
            .try_send(Frame::data(Bytes::from(vec![b'x'; 1 << 10])))
            .unwrap();
        let outcome = Outcome::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        let held = hold(receiver, &outcome, deadline, memory.clone(), "timeout");
        let refused = async {
            // Once the hold holds the frame.
            while memory.used() < 1 << 10 {
                tokio::task::yield_now().await;
            }
            let refused = memory.part().charge(2 << 20).expect_err("its work fits");
            outcome.set(Err(told(&memory, refused)));
            drop(sender);
        };
        let (stopped, ()) = tokio::join!(held, refused);
        let stopped = stopped.err().expect("its work refused, no answer");
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
        assert_eq!(
            detail(stopped).await,
            "the query's own (budgets.query_memory)"
        );
    }

    /// Over HTTP/1.0, a producer whose work is refused after the hold took its first frame
    /// from the channel but before the hold charged it keeps its own refusal: at the refusal
    /// the hold held nothing. Step by step, on the hold's step between its clock check and its
    /// charge: the frame queued; the hold takes it and stops there; the work is refused and
    /// the producer ends; the hold goes on.
    #[tokio::test]
    async fn a_producers_work_refused_before_its_first_frame_is_charged_keeps_its_own_refusal() {
        let memory = Envelope::new(Some(1 << 20), None);
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        sender
            .try_send(Frame::data(Bytes::from(vec![b'x'; CHUNK_BYTES])))
            .unwrap();
        let outcome = Outcome::default();
        let (at_step, go) = armed();
        let deadline = Instant::now() + Duration::from_secs(30);
        let held = hold(receiver, &outcome, deadline, memory.clone(), "timeout");
        let refused = async {
            at_step.await.expect("the hold reached its step");
            assert_eq!(memory.used(), 0, "the frame was charged before the step");
            let refused = memory.part().charge(2 << 20).expect_err("its work fits");
            outcome.set(Err(told(&memory, refused)));
            drop(sender);
            go.send(()).unwrap();
        };
        let (stopped, ()) = tokio::join!(held, refused);
        let stopped = stopped.err().expect("its work refused, no answer");
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
        assert_eq!(
            detail(stopped).await,
            "the query's own (budgets.query_memory)"
        );
    }

    /// Over HTTP/1.0, a producer that ended with its own error while a frame it wrote was
    /// still unheld keeps that error, though the hold's charge of the frame is refused after
    /// it (the frame is past the envelope): the answer is what stopped it first. Step by step,
    /// on the hold's step between its clock check and its charge: the frame queued; the hold
    /// takes it and stops there; the producer records its error and ends; the hold goes on.
    #[tokio::test]
    async fn a_producers_error_recorded_before_a_queued_frames_refusal_is_the_answer() {
        let memory = Envelope::new(Some(1 << 10), None);
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        sender
            .try_send(Frame::data(Bytes::from(vec![b'x'; CHUNK_BYTES])))
            .unwrap();
        let outcome = Outcome::default();
        let (at_step, go) = armed();
        let deadline = Instant::now() + Duration::from_secs(30);
        let held = hold(receiver, &outcome, deadline, memory.clone(), "timeout");
        let ended = async {
            at_step.await.expect("the hold reached its step");
            outcome.set(Err(ApiError::internal("the export failed")));
            drop(sender);
            go.send(()).unwrap();
        };
        let (stopped, ()) = tokio::join!(held, ended);
        let stopped = stopped
            .err()
            .map(IntoResponse::into_response)
            .expect("the producer failed, no answer");
        assert_eq!(stopped.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
    }

    /// What each step of a producer's growing work charges.
    const GROWTH: usize = 64 << 10;

    /// The first refusal of work that grows by [`GROWTH`] up to `total` through `charge`.
    fn grown(charge: impl Fn(usize) -> Result<(), BudgetExceeded>, total: usize) -> BudgetExceeded {
        (0..total / GROWTH)
            .find_map(|_| charge(GROWTH).err())
            .expect("the work fits")
    }

    /// How a route tells a store error that a writer leaves to it (`sparql::map_query_error`):
    /// the memory all queries share's 503, else the query's own 413.
    fn routed(error: StoreError) -> ApiError {
        match error.is_server_memory_limit() {
            true => ApiError::unavailable("the server's query memory is in use"),
            false => own(),
        }
    }

    /// Waits on the producer's thread until `memory` holds `bytes`.
    fn held_at_least(memory: &Envelope, bytes: usize) {
        for _ in 0..60_000 {
            if memory.used() >= bytes {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A site of an answer's memory refusals: what its producer does with its writer and the
    /// envelope its answer is held in.
    type Site = fn(&mut ChannelWriter, &Envelope) -> Result<(), ApiError>;

    /// The hold's charge of a frame: the producer writes 2 MiB.
    fn frame(writer: &mut ChannelWriter, _: &Envelope) -> Result<(), ApiError> {
        io::Write::write_all(writer, &vec![b'x'; 2 << 20])
            .and_then(|()| io::Write::flush(writer))
            .map_err(|error| ApiError::internal(error.to_string()))
    }

    /// The producer's work before anything is held (a query's evaluation, a read's view): it
    /// grows to 2 MiB.
    fn evaluation(writer: &mut ChannelWriter, memory: &Envelope) -> Result<(), ApiError> {
        let part = memory.part();
        let refused = grown(|bytes| part.charge(bytes), 2 << 20);
        Err(writer.refused(StoreError::from(refused), routed))
    }

    /// A later reservation of the producer's, beside what is held: it writes two chunks,
    /// waits until they are held, then its work grows to 2 MiB.
    fn later(writer: &mut ChannelWriter, memory: &Envelope) -> Result<(), ApiError> {
        io::Write::write_all(writer, &vec![b'x'; 2 * CHUNK_BYTES])
            .and_then(|()| io::Write::flush(writer))
            .map_err(|error| ApiError::internal(error.to_string()))?;
        held_at_least(memory, 2 * CHUNK_BYTES);
        let part = memory.part();
        let refused = grown(|bytes| part.charge(bytes), 2 << 20);
        Err(writer.refused(StoreError::from(refused), routed))
    }

    /// The status and the problem's `type`, `title` and `detail` of `response`.
    async fn problem_of(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let problem = serde_json::json!({
            "type": problem["type"],
            "title": problem["title"],
            "detail": problem["detail"],
        });
        (status, problem)
    }

    /// What a reader of every graph is told of the HTTP/1.0 answer `site` makes, held whole
    /// in an envelope of 1 MiB within 8 MiB all queries share, of which other work holds all
    /// but `room` (`None`: none of it). The envelope is checked to hold nothing after it.
    async fn told_at(site: Site, room: Option<usize>) -> (StatusCode, serde_json::Value) {
        const SHARED: usize = 8 << 20;
        let shared = nrese_exec::SharedBudget::new(SHARED);
        let hidden = Envelope::new(None, Some(Arc::clone(&shared)));
        if let Some(room) = room {
            hidden.charge(SHARED - room).unwrap();
        }
        let memory = Envelope::new(Some(1 << 20), Some(shared));
        let (charged, held) = (memory.clone(), memory.clone());
        let answer = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::Total(Instant::now() + Duration::from_secs(120)),
                    CancellationToken::new(),
                    move || held,
                    Reader::Every,
                    "text/plain",
                    "timeout",
                    move |writer| site(writer, &charged),
                ),
            )
            .await;
        let refused = answer.expect_err("refused for memory, no answer");
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
        problem_of(refused.into_response()).await
    }

    /// Each site where a reader of every graph's HTTP/1.0 answer, held whole, is refused for
    /// memory, its producer's writer and hold as every route has them: the hold's charge of a
    /// frame, the producer's work before anything is held, and its later reservation beside
    /// what is held. Alone, its own envelope refuses (1 MiB): a 413. Beside other work, the
    /// memory all queries share does (all but 256 KiB held): a 503.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_readers_held_answer_is_told_which_limit_at_every_site() {
        let sites: [(&str, Site); 3] = [
            ("the hold's charge of a frame", frame),
            ("the producer's work before anything is held", evaluation),
            ("a later reservation beside what is held", later),
        ];
        for (name, site) in sites {
            let alone = told_at(site, None).await;
            assert_eq!(alone.0, StatusCode::PAYLOAD_TOO_LARGE, "{name}: {alone:?}");
            let beside = told_at(site, Some(256 << 10)).await;
            assert_eq!(
                beside.0,
                StatusCode::SERVICE_UNAVAILABLE,
                "{name}: {beside:?}"
            );
        }
    }

    /// The shared delivery path refuses a restricted HTTP/1.0 stream before it can
    /// open a memory envelope or start the producer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_restricted_http10_stream_does_not_start_its_producer() {
        let response = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::Total(Instant::now() + Duration::from_secs(120)),
                    CancellationToken::new(),
                    || -> Envelope { panic!("an envelope was opened to hold the answer") },
                    Reader::Restricted,
                    "text/plain",
                    "timeout",
                    |_| panic!("a restricted HTTP/1.0 producer started"),
                ),
            )
            .await
            .expect("the version refusal");
        assert_eq!(response.status(), StatusCode::HTTP_VERSION_NOT_SUPPORTED);
    }

    /// Over HTTP/1.0, a refusal of a producer's work carries the cause the budget judged at
    /// it, whatever is refused after it: refused while two chunks are held for work that fits
    /// alone ([`WORK`]: holding caused it), then again for work past the whole envelope (its
    /// own), the producer ends with the first, which is told as the hold's 413, the one that
    /// tells the client to stream.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refusals_cause_stays_its_own_after_a_later_refusal() {
        let memory = Envelope::new(Some(1 << 20), None);
        let (charged, held) = (memory.clone(), memory.clone());
        let answer = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::Total(Instant::now() + Duration::from_secs(120)),
                    CancellationToken::new(),
                    move || held,
                    Reader::Every,
                    "text/plain",
                    "timeout",
                    move |writer| {
                        io::Write::write_all(writer, &vec![b'x'; 2 * CHUNK_BYTES])
                            .and_then(|()| io::Write::flush(writer))
                            .map_err(|error| ApiError::internal(error.to_string()))?;
                        held_at_least(&charged, 2 * CHUNK_BYTES);
                        let part = charged.part();
                        let first = part.charge(WORK).expect_err("its work fits after all");
                        let second = part.charge(2 << 20).expect_err("its work fits after all");
                        assert!(first.for_held && !second.for_held, "{first:?}, {second:?}");
                        Err(writer.refused(StoreError::from(first), routed))
                    },
                ),
            )
            .await;
        let stopped = answer.expect_err("its work refused, no answer");
        assert_eq!(memory.used(), 0, "the refused answer still holds memory");
        let detail = detail(stopped).await;
        let detail = detail.as_str().unwrap_or_default();
        assert!(
            detail.starts_with("This answer is larger than the 1 MiB an HTTP/1.0 request"),
            "{detail}"
        );
    }

    /// Over HTTP/1.0, what a hold that resumes only past its deadline (1 s) answers, on a
    /// clock the test moves: the producer is open through expiry, and only after it are
    /// `frame` bytes queued and, with `end`, its end set (`None`: it stays open).
    async fn resumed_past_its_deadline(
        memory: Envelope,
        frame: usize,
        end: Option<Result<(), ApiError>>,
    ) -> Option<StatusCode> {
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        let outcome = Outcome::default();
        let deadline = Instant::now() + Duration::from_secs(1);
        let held = hold(receiver, &outcome, deadline, memory, "timeout");
        tokio::time::advance(Duration::from_secs(2)).await;
        if frame > 0 {
            sender
                .try_send(Frame::data(Bytes::from(vec![b'x'; frame])))
                .unwrap();
        }
        let open = match end {
            Some(end) => {
                outcome.set(end);
                drop(sender);
                None
            }
            None => Some(sender),
        };
        let status = held
            .await
            .err()
            .map(|stopped| stopped.into_response().status());
        drop(open);
        status
    }

    /// Over HTTP/1.0, past its deadline nothing more is taken, though it is ready when the
    /// hold looks: the answer's end, the producer's error (a graph read's 404, an export's
    /// 500), and a frame past the envelope (its 413) all give 408.
    #[tokio::test(start_paused = true)]
    async fn a_held_answer_ending_only_past_its_deadline_is_a_timeout() {
        let timeout = Some(StatusCode::REQUEST_TIMEOUT);
        let ended = |end| resumed_past_its_deadline(Envelope::unlimited(), 16, Some(end));
        assert_eq!(ended(Ok(())).await, timeout, "the answer");
        assert_eq!(
            ended(Err(ApiError::not_found("no such graph"))).await,
            timeout,
            "a 404"
        );
        assert_eq!(
            ended(Err(ApiError::internal("the export failed"))).await,
            timeout,
            "a 500"
        );
        let small = Envelope::new(Some(1 << 10), None);
        assert_eq!(
            resumed_past_its_deadline(small, CHUNK_BYTES, None).await,
            timeout,
            "a 413"
        );
    }

    /// Over HTTP/1.0, a frame that passed the hold's clock check but whose charge is refused
    /// only once its deadline (1 s) has passed (it waited on the budget meanwhile) is too late
    /// to be answered for: 408, not the 413 of a frame past its envelope (1 KiB). On a clock
    /// the test moves, on the hold's step between its clock check and its charge.
    #[tokio::test(start_paused = true)]
    async fn a_frame_refused_past_its_deadline_is_a_timeout() {
        let memory = Envelope::new(Some(1 << 10), None);
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        sender
            .try_send(Frame::data(Bytes::from(vec![b'x'; CHUNK_BYTES])))
            .unwrap();
        let outcome = Outcome::default();
        let (at_step, go) = armed();
        let deadline = Instant::now() + Duration::from_secs(1);
        let held = hold(receiver, &outcome, deadline, memory.clone(), "timeout");
        let late = async {
            at_step.await.expect("the hold reached its step");
            tokio::time::advance(Duration::from_secs(2)).await;
            go.send(()).unwrap();
        };
        let (stopped, ()) = tokio::join!(held, late);
        let status = stopped
            .err()
            .map(|stopped| stopped.into_response().status());
        assert_eq!(status, Some(StatusCode::REQUEST_TIMEOUT));
        assert_eq!(memory.used(), 0);
        drop(sender);
    }

    /// Over HTTP/1.0, a held answer's frame isn't taken past the stall bound, though the timer
    /// that lets go of the answer hasn't run: the body ends short of its length, its memory
    /// let go. On a clock the test moves, the timer stopped.
    #[tokio::test(start_paused = true)]
    async fn a_held_frame_is_not_taken_past_its_stall_bound() {
        let memory = Envelope::new(Some(1 << 20), None);
        let (sender, receiver) = mpsc::channel(CHANNEL_CHUNKS);
        sender
            .try_send(Frame::data(Bytes::from(vec![b'x'; CHUNK_BYTES])))
            .unwrap();
        drop(sender);
        let outcome = Outcome::default();
        outcome.set(Ok(()));
        let deadline = Instant::now() + Duration::from_secs(30);
        let holding = hold(receiver, &outcome, deadline, memory.clone(), "timeout")
            .await
            .unwrap_or_else(|stopped| panic!("{stopped}"));
        let mut body = Held::new(holding, Duration::from_secs(1));
        body.watch.abort();
        tokio::time::advance(Duration::from_secs(2)).await;
        let taken = http_body_util::BodyExt::frame(&mut body).await;
        assert!(
            matches!(taken, Some(Err(_))),
            "a frame taken past the bound"
        );
        assert_eq!(memory.used(), 0);
    }

    /// Over HTTP/1.0, the first frame's stall bound starts when the answer is ready, not when
    /// the hold began: a read that is ready just before its first deadline leaves its client
    /// the whole bound for the first frame, not what is left of the deadline.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_answers_first_send_window_starts_when_it_is_ready() {
        let bound = Duration::from_secs(2);
        let response = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::stalled_after(bound),
                    CancellationToken::new(),
                    Envelope::unlimited,
                    Reader::Every,
                    "text/plain",
                    "timeout",
                    move |writer| {
                        // Ready at four fifths of the first deadline.
                        std::thread::sleep(bound * 4 / 5);
                        io::Write::write_all(writer, &vec![b'x'; 2 * CHUNK_BYTES])
                            .map_err(|error| ApiError::internal(error.to_string()))
                    },
                ),
            )
            .await
            .expect("ready before its first deadline");
        // The client takes its first frame half the bound after the answer is ready: past
        // the bound from the hold's start, well within it from the answer's readiness.
        tokio::time::sleep(bound / 2).await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await;
        assert_eq!(
            body.expect("the first frame is taken within its window")
                .len(),
            2 * CHUNK_BYTES
        );
    }

    /// Over HTTP/1.0, a client that takes part of a held answer and then stops reading,
    /// without leaving, keeps none of it once the stall bound has passed: what was held goes
    /// back, to its envelope and to the memory all queries share (the envelope's credit there
    /// too), and the body ends with an error, short of its length, never cleanly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_answer_whose_client_stops_reading_releases_its_memory_at_the_stall_bound() {
        use http_body_util::BodyExt;

        // 1 GiB shared: the envelope keeps a credit there (16 KiB).
        let shared = nrese_exec::SharedBudget::new(1 << 30);
        let memory = Envelope::new(Some(1 << 30), Some(Arc::clone(&shared)));
        let held = memory.clone();
        let stall = Duration::from_millis(500);
        let response = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::Stall {
                        first: Instant::now() + Duration::from_secs(120),
                        stall,
                    },
                    CancellationToken::new(),
                    move || held,
                    Reader::Every,
                    "text/plain",
                    "timeout",
                    |writer| {
                        io::Write::write_all(writer, &vec![b'x'; 3 * CHUNK_BYTES])
                            .map_err(|error| ApiError::internal(error.to_string()))
                    },
                ),
            )
            .await
            .unwrap();
        let mut body = response.into_body();
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(first.len(), CHUNK_BYTES);
        assert_eq!(memory.used(), 2 * CHUNK_BYTES);
        // The client stops reading. Real time, far past the bound: a guard against a hang.
        let stopped = std::time::Instant::now();
        while memory.used() != 0 || shared.used() != 0 {
            assert!(
                stopped.elapsed() < 20 * stall,
                "{} bytes still held, {} of the shared memory, for a client that stopped reading",
                memory.used(),
                shared.used()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let next = body.frame().await;
        assert!(
            matches!(next, Some(Err(_))),
            "a cut answer ends with an error"
        );
    }
}
