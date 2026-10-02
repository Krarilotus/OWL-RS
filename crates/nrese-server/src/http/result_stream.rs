//! Streams output produced on a blocking thread into an HTTP response body.
//!
//! - **Backpressure.** The producer writes through a [`ChannelWriter`]: 64 KiB chunks over a
//!   bounded channel. A slow client slows the producer down instead of the server buffering
//!   the whole result.
//! - **Status.** The response status is chosen only when the first chunk (or the producer's
//!   error) arrives, so errors and timeouts before any output still get a proper status.
//! - **After the first chunk,** a failure can only abort the connection; the 200 is already
//!   sent. Clients see a truncated body, as with QLever or Fuseki.
//! - **Cancellation.** Reaching the deadline, or the client disconnecting (the body is
//!   dropped), cancels the producer's token, so evaluation stops promptly instead of running
//!   to completion for nobody.

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

/// `io::Write` that forwards its output in chunks over the channel.
pub struct ChannelWriter {
    sender: mpsc::Sender<Message>,
    buffer: Vec<u8>,
}

impl io::Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= CHUNK_BYTES {
            self.flush()?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK_BYTES));
        self.sender
            .blocking_send(Ok(Bytes::from(chunk)))
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe)) // client went away
    }
}

/// Cancels the token when dropped, i.e. when the response body is dropped: at the end of
/// the stream (harmless) or when the client disconnects.
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
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
    tokio::task::spawn_blocking(move || {
        let mut writer = ChannelWriter {
            sender: sender.clone(),
            buffer: Vec::with_capacity(CHUNK_BYTES),
        };
        let result = produce(&mut writer).and_then(|()| {
            io::Write::flush(&mut writer).map_err(|error| ApiError::internal(error.to_string()))
        });
        if let Err(error) = result {
            let _ = sender.blocking_send(Err(error)); // no receiver: the client is gone
        }
    });

    let guard = CancelOnDrop(cancellation);
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
