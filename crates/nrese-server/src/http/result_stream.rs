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
//! - **Trailers.** A producer may end with trailer fields ([`ChannelWriter::trailers`]),
//!   sent after the body: what is known only once all of it is written (a checksum).
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
//!   no answer charged.

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
use nrese_exec::{BudgetExceeded, Envelope, Reservation};
use nrese_store::CancellationToken;
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
/// It holds the answer only: what the read needs besides is its own (a transaction's
/// pending changes, applied for its read in an envelope of their own).
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
    pace: Pace,
    start: Arc<Start>,
    runtime: tokio::runtime::Handle,
}

impl ChannelWriter {
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

    /// Ends the body with `trailers`, sent after everything written so far.
    pub fn trailers(&mut self, trailers: HeaderMap) -> io::Result<()> {
        io::Write::flush(self)?;
        self.send(Frame::trailers(trailers))
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
/// within `pace`; or holds it whole (HTTP/1.0), charged to the envelope `memory` opens.
/// `produce` must honour `cancellation`.
pub async fn stream_blocking(
    pace: Pace,
    cancellation: CancellationToken,
    memory: impl FnOnce() -> Envelope,
    media_type: &'static str,
    timeout_message: &'static str,
    produce: impl FnOnce(&mut ChannelWriter) -> Result<(), ApiError> + Send + 'static,
) -> Result<Response, ApiError> {
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
    let mut writer = ChannelWriter {
        sender,
        buffer: Vec::with_capacity(CHUNK_BYTES),
        pace,
        start: Arc::clone(&start),
        runtime: tokio::runtime::Handle::current(),
    };
    let ended = Arc::clone(&outcome);
    tokio::task::spawn_blocking(move || {
        let result = produce(&mut writer).and_then(|()| {
            io::Write::flush(&mut writer).map_err(|error| ApiError::internal(error.to_string()))
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
    if whole {
        // The first deadline bounds all of it: no chunk goes out to start a stall bound. On
        // every exit but the answer's end, `guard` cancels the producer.
        let held = hold(receiver, &outcome, deadline, memory(), timeout_message).await?;
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
    let envelope = memory.clone();
    let mut held = Holding {
        frames: VecDeque::new(),
        left: 0,
        memory: Reservation::new(memory),
        envelope: envelope.clone(),
        moved: Instant::now(),
        cut: false,
    };
    loop {
        match tokio::time::timeout_at(deadline, receiver.recv()).await {
            Err(_elapsed) => return Err(ApiError::timeout(timeout_message)),
            Ok(None) => break,
            Ok(Some(frame)) => {
                if let Some(data) = frame.data_ref() {
                    let bytes = data.len();
                    held.memory
                        .charge(bytes)
                        .map_err(|refused| too_large(refused, bound))?;
                    held.left += bytes as u64;
                }
                held.frames.push_back(frame);
            }
        }
    }
    if let Err(stopped) = outcome.take() {
        // The producer's own work (a query's) draws on the same envelope: refused while the
        // hold held part of the answer, it was refused for holding it, as a refusal of the
        // hold's own charge would be.
        let beside = held.memory.bytes() > 0 && envelope.refused();
        return Err(match stopped {
            ApiError::PayloadTooLarge(_) if beside => too_large_for_http10(bound),
            ApiError::ServiceUnavailable(detail) if beside => ApiError::busy(detail),
            stopped => stopped,
        });
    }
    Ok(held)
}

/// The refusal of an answer held whole past `bound`, its envelope's limit; or, where the
/// memory all queries share is taken, a 503 that may be retried; or, where the process's
/// memory couldn't be read, a 503 without a hint.
fn too_large(refused: BudgetExceeded, bound: usize) -> ApiError {
    let shared =
        || format!("{refused} (budgets.total_query_memory, NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES)");
    if refused.unread {
        return ApiError::unavailable(shared());
    }
    if refused.shared {
        return ApiError::busy(shared());
    }
    too_large_for_http10(bound)
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
    memory: Reservation<Envelope>,
    /// The envelope `memory` is charged to.
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
        self.memory.release(self.memory.bytes());
        self.envelope.return_credit();
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
        Self { holding, watch }
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
        if holding.cut {
            return Poll::Ready(Some(Err(io::Error::from(io::ErrorKind::TimedOut))));
        }
        let frame = holding.frames.pop_front();
        if let Some(data) = frame.as_ref().and_then(Frame::data_ref) {
            let bytes = data.len();
            holding.left -= bytes as u64;
            holding.memory.release(bytes);
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
    use std::time::Duration;

    use futures_util::StreamExt;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_large_write_goes_out_in_chunks_of_at_most_64_kib() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let response = stream_blocking(
            Pace::Total(deadline),
            CancellationToken::new(),
            Envelope::unlimited,
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
    /// stop's error, as a query an operator cancels does; it says when it has ended.
    fn written_then_stopped(
        bytes: usize,
        token: CancellationToken,
        wrote: std::sync::mpsc::Sender<()>,
        ended: std::sync::mpsc::Sender<()>,
    ) -> impl FnOnce(&mut ChannelWriter) -> Result<(), ApiError> + Send + 'static {
        move |writer| {
            let written = io::Write::write_all(writer, &vec![b'x'; bytes])
                .and_then(|()| io::Write::flush(writer));
            let _ = wrote.send(());
            while written.is_ok() && !token.is_cancelled() {
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
    /// going past it gets 408, though it wrote before it and keeps writing well within the
    /// stall bound (streamed, that would have started a 200 the stall bound then keeps
    /// going, to its end).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_reads_first_deadline_bounds_all_of_it() {
        let memory = Envelope::new(Some(1 << 30), None);
        let token = CancellationToken::new();
        let stop = token.clone();
        let held = memory.clone();
        let stopped = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::stalled_after(Duration::from_secs(1)),
                    token,
                    move || held,
                    "text/plain",
                    "timeout",
                    // A chunk every 50 ms for 4 s: far past the first deadline, never near
                    // the stall bound.
                    move |writer| {
                        for _ in 0..80 {
                            if stop.is_cancelled() {
                                return Err(ApiError::timeout("stopped"));
                            }
                            io::Write::write_all(writer, &[b'x'; CHUNK_BYTES])
                                .map_err(|error| ApiError::internal(error.to_string()))?;
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Ok(())
                    },
                ),
            )
            .await
            .expect_err("past its deadline, no answer");
        assert_eq!(
            stopped.into_response().status(),
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(memory.used(), 0);
    }

    /// Over HTTP/1.0, the answer held by a producer whose own work (a query's) draws on the
    /// same envelope as the hold, and which is refused there after writing `CHUNK_BYTES * 2`
    /// and ending with what its refusal is mapped to (`stopped`): the response; the envelope
    /// is checked to hold nothing after it.
    async fn refused_beside_its_held_answer(memory: Envelope, stopped: ApiError) -> Response {
        let work = memory.clone();
        let held = memory.clone();
        let answer = WHOLE
            .scope(
                true,
                stream_blocking(
                    Pace::Total(Instant::now() + Duration::from_secs(120)),
                    CancellationToken::new(),
                    move || held,
                    "text/plain",
                    "timeout",
                    move |writer| {
                        io::Write::write_all(writer, &vec![b'x'; 2 * CHUNK_BYTES])
                            .and_then(|()| io::Write::flush(writer))
                            .map_err(|error| ApiError::internal(error.to_string()))?;
                        // Its work grows once the hold holds what it wrote, and is refused.
                        for _ in 0..60_000 {
                            if work.used() >= 2 * CHUNK_BYTES {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        let part = work.part();
                        assert!(part.charge(2 << 20).is_err(), "its work fits after all");
                        Err(stopped)
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
    /// query in the same envelope would be answered); or, where the memory all queries share
    /// refused it, the 503 that may be retried.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_producers_work_refused_beside_its_held_answer_is_the_answers_refusal() {
        let own = refused_beside_its_held_answer(
            Envelope::new(Some(1 << 20), None),
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
            ApiError::unavailable("the query's own (budgets.total_query_memory)"),
        )
        .await;
        assert_eq!(shared.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            shared.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("1"))
        );
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
