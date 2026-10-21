//! Streams output produced on a blocking thread into an HTTP response body.
//!
//! - **Backpressure.** The producer writes through a [`ChannelWriter`]: chunks of at most
//!   64 KiB (a large write, such as a cached result, is split) over a bounded channel. A
//!   slow client slows the producer down instead of the server buffering the whole result.
//! - **Status.** The response status is chosen only when the first chunk (or the producer's
//!   error) arrives, so errors and timeouts before any output still get a proper status.
//! - **After the first chunk,** a failure can only abort the transfer: no final chunk on
//!   HTTP/1.1, a reset stream on HTTP/2, never a clean end. The 200 is already sent; clients
//!   see a truncated body they can tell from a complete one, as with QLever or Fuseki.
//! - **Trailers.** A producer may end with trailer fields ([`ChannelWriter::trailers`]),
//!   sent after the body: what is known only once all of it is written (a checksum).
//! - **Time ([`Pace`]).** A computation deadline for the whole stream (a query's timeout),
//!   or a deadline for the first chunk and then a bound on each chunk's stall (reads of
//!   stored statements, the export): a client that keeps reading is never cut off, one that
//!   stops is. Either way the producer waits for room in the channel only so long, so it
//!   lets go of its thread and snapshot when nobody reads.
//! - **Cancellation.** Reaching the deadline or a stall, or the client disconnecting (the
//!   body is dropped), cancels the producer's token, so evaluation stops promptly instead of
//!   running to completion for nobody.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body::Frame;
use nrese_store::CancellationToken;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::error::ApiError;

const CHUNK_BYTES: usize = 64 * 1024;
/// Chunks in flight between producer and client (at most ~0.5 MiB per request).
const CHANNEL_CHUNKS: usize = 8;

type Message = Result<Frame<Bytes>, ApiError>;

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
    /// A chunk went into the channel.
    started: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
}

impl ChannelWriter {
    /// Sends `message`, waiting for room in the channel as long as the pace allows.
    fn send(&self, message: Message) -> io::Result<()> {
        let until = self.pace.until(self.started.load(Ordering::Acquire));
        let sent = self
            .runtime
            .block_on(tokio::time::timeout_at(until, self.sender.send(message)));
        match sent {
            Ok(Ok(())) => {
                self.started.store(true, Ordering::Release);
                Ok(())
            }
            Ok(Err(_)) => Err(io::Error::from(io::ErrorKind::BrokenPipe)), // client went away
            Err(_elapsed) => Err(io::Error::from(io::ErrorKind::TimedOut)), // not read in time
        }
    }

    /// Ends the body with `trailers`, sent after everything written so far.
    pub fn trailers(&mut self, trailers: HeaderMap) -> io::Result<()> {
        io::Write::flush(self)?;
        self.send(Ok(Frame::trailers(trailers)))
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
        self.send(Ok(Frame::data(Bytes::from(chunk))))
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
/// within `pace`. `produce` must honour `cancellation`.
pub async fn stream_blocking(
    pace: Pace,
    cancellation: CancellationToken,
    media_type: &'static str,
    timeout_message: &'static str,
    produce: impl FnOnce(&mut ChannelWriter) -> Result<(), ApiError> + Send + 'static,
) -> Result<Response, ApiError> {
    let (sender, mut receiver) = mpsc::channel::<Message>(CHANNEL_CHUNKS);
    let started = Arc::new(AtomicBool::new(false));
    let mut writer = ChannelWriter {
        sender,
        buffer: Vec::with_capacity(CHUNK_BYTES),
        pace,
        started: Arc::clone(&started),
        runtime: tokio::runtime::Handle::current(),
    };
    tokio::task::spawn_blocking(move || {
        let result = produce(&mut writer).and_then(|()| {
            io::Write::flush(&mut writer).map_err(|error| ApiError::internal(error.to_string()))
        });
        if let Err(error) = result {
            let _ = writer.send(Err(error)); // the client is gone, or isn't reading
        }
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
        if total || !started.load(Ordering::Acquire) {
            watchdog.cancel();
        }
    })
    .abort_handle();

    let guard = CancelOnDrop(cancellation, watchdog);
    let first = match tokio::time::timeout_at(deadline, receiver.recv()).await {
        Err(_elapsed) => return Err(ApiError::timeout(timeout_message)), // guard cancels
        Ok(Some(Err(error))) => return Err(error),
        Ok(Some(Ok(frame))) => Some(frame),
        Ok(None) => None, // finished without output
    };

    struct State {
        receiver: mpsc::Receiver<Message>,
        first: Option<Frame<Bytes>>,
        guard: CancelOnDrop,
        pace: Pace,
        timeout_message: &'static str,
    }
    let state = State {
        receiver,
        first,
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
            Ok(Some(Ok(frame))) => Some((Ok(frame), Some(state))),
            Ok(Some(Err(error))) => Some((Err(io::Error::other(error.to_string())), None)),
            Ok(None) => None,
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
}
