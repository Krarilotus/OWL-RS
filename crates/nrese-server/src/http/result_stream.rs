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

use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body::Frame;
use nrese_store::CancellationToken;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::cancellable::CancelOnDrop;
use crate::error::ApiError;

const CHUNK_BYTES: usize = 64 * 1024;
/// Chunks in flight between producer and client (at most ~0.5 MiB per request).
const CHANNEL_CHUNKS: usize = 8;

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

/// Runs `produce` on a blocking thread and streams what it writes as the response body,
/// within `pace`. `produce` must honour `cancellation`.
pub async fn stream_blocking(
    pace: Pace,
    cancellation: CancellationToken,
    media_type: &'static str,
    timeout_message: &'static str,
    produce: impl FnOnce(&mut ChannelWriter) -> Result<(), ApiError> + Send + 'static,
) -> Result<Response, ApiError> {
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

    let guard = CancelOnDrop(cancellation, Some(watchdog));
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
    let mut response = (StatusCode::OK, body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    Ok(response)
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

    /// Sol r1 F1: a client that stops reading until the producer has given up, then drains
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
            "text/plain",
            "timeout",
            |_| Ok(()),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await;
        assert_eq!(body.expect("a clean end").len(), 0);
    }

    /// Sol r1 F2: the first chunk's start is decided once. In both orders exactly one side
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
}
