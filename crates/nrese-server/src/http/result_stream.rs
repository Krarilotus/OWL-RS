//! Streams output produced on a blocking thread into an HTTP response body.
//!
//! - **Backpressure.** The producer writes through a [`ChannelWriter`]: chunks of at most
//!   64 KiB (a large write, such as a cached result, is split) over a bounded channel. A
//!   slow client slows the producer down instead of the server buffering the whole result.
//! - **Status.** The response status is chosen only when the first chunk (or the producer's
//!   error) arrives, so errors and timeouts before any output still get a proper status.
//! - **After the first chunk,** a failure can only abort the connection; the 200 is already
//!   sent. Clients see a truncated body, as with QLever or Fuseki.
//! - **Cancellation.** Reaching the deadline, or the client disconnecting (the body is
//!   dropped), cancels the producer's token, so evaluation stops promptly instead of running
//!   to completion for nobody. The deadline holds even if the body isn't polled (a client
//!   that stops reading): a timer cancels the token, and the producer waits for room in
//!   the channel only until the deadline, so it lets go of its thread and snapshot.

use std::io;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_store::CancellationToken;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::error::ApiError;

const CHUNK_BYTES: usize = 64 * 1024;
/// Chunks in flight between producer and client (at most ~0.5 MiB per request).
const CHANNEL_CHUNKS: usize = 8;

type Message = Result<Bytes, ApiError>;

/// `io::Write` that forwards its output in chunks of at most [`CHUNK_BYTES`] over the
/// channel, waiting for room until the deadline.
pub struct ChannelWriter {
    sender: mpsc::Sender<Message>,
    buffer: Vec<u8>,
    deadline: Instant,
    runtime: tokio::runtime::Handle,
}

impl ChannelWriter {
    /// Sends `message`, waiting for room in the channel until the deadline.
    fn send(&self, message: Message) -> io::Result<()> {
        let sent = self.runtime.block_on(tokio::time::timeout_at(
            self.deadline,
            self.sender.send(message),
        ));
        match sent {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(io::Error::from(io::ErrorKind::BrokenPipe)), // client went away
            Err(_elapsed) => Err(io::Error::from(io::ErrorKind::TimedOut)), // not read in time
        }
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
        self.send(Ok(Bytes::from(chunk)))
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

/// Runs `produce` on a blocking thread and streams what it writes as the response body.
/// `produce` must honour `cancellation`.
pub async fn stream_blocking(
    deadline: Instant,
    cancellation: CancellationToken,
    media_type: &'static str,
    timeout_message: &'static str,
    produce: impl FnOnce(&mut ChannelWriter) -> Result<(), ApiError> + Send + 'static,
) -> Result<Response, ApiError> {
    let (sender, mut receiver) = mpsc::channel::<Message>(CHANNEL_CHUNKS);
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut writer = ChannelWriter {
            sender,
            buffer: Vec::with_capacity(CHUNK_BYTES),
            deadline,
            runtime,
        };
        let result = produce(&mut writer).and_then(|()| {
            io::Write::flush(&mut writer).map_err(|error| ApiError::internal(error.to_string()))
        });
        if let Err(error) = result {
            let _ = writer.send(Err(error)); // the client is gone, or isn't reading
        }
    });
    // The deadline as a computation deadline: evaluation stops at it whether or not the
    // body is being polled (the review of 3 October 2026, P5).
    let watchdog = cancellation.clone();
    let watchdog = tokio::spawn(async move {
        tokio::time::sleep_until(deadline).await;
        watchdog.cancel();
    })
    .abort_handle();

    let guard = CancelOnDrop(cancellation, watchdog);
    let first = match tokio::time::timeout_at(deadline, receiver.recv()).await {
        Err(_elapsed) => return Err(ApiError::timeout(timeout_message)), // guard cancels
        Ok(Some(Err(error))) => return Err(error),
        Ok(Some(Ok(chunk))) => Some(chunk),
        Ok(None) => None, // finished without output
    };

    struct State {
        receiver: mpsc::Receiver<Message>,
        first: Option<Bytes>,
        guard: CancelOnDrop,
        deadline: Instant,
        timeout_message: &'static str,
    }
    let state = State {
        receiver,
        first,
        guard,
        deadline,
        timeout_message,
    };
    let body = futures_util::stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        if let Some(chunk) = state.first.take() {
            return Some((Ok(chunk), Some(state)));
        }
        match tokio::time::timeout_at(state.deadline, state.receiver.recv()).await {
            Ok(Some(Ok(chunk))) => Some((Ok(chunk), Some(state))),
            Ok(Some(Err(error))) => Some((Err(io::Error::other(error.to_string())), None)),
            Ok(None) => None,
            Err(_elapsed) => {
                state.guard.0.cancel();
                Some((Err(io::Error::other(state.timeout_message)), None))
            }
        }
    });

    let mut response = (StatusCode::OK, Body::from_stream(body)).into_response();
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
            deadline,
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
            deadline,
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
}
