//! Spec: result_stream's contract: a client can always tell an answer a stop cut short from a
//! complete one. Over HTTP/1.1 and HTTP/2 the transport says so (no final chunk; a reset
//! stream). HTTP/1.0 can't, so its answer is held whole and sent with its length, its status
//! chosen at the end.
//!
//! A known length tells a cut too: the client counts the bytes. Over HTTP/1.1 and HTTP/2 the
//! server sends one only for a file whose size it knows, the replication image.
//!
//! The rows name every framing the server can send an answer with. hyper 1.9.0 (the
//! version in Cargo.lock) chooses an HTTP/1 response's framing in `src/proto/h1/role.rs`,
//! `Server::encode_headers`, from three inputs: the request's version, whether the body's
//! length is known, and whether the request is a `HEAD` (`Server::can_have_body`; also a
//! 1xx, 204 or 304 status or a `CONNECT`, which no streamed answer has). `Connection: close`
//! decides only whether the connection stays open, not the framing: the chunked row runs
//! with and without it. HTTP/2 (`src/proto/h2/server.rs`) sends every body as DATA frames,
//! ended by the stream's end or a reset. [`FRAMINGS`] holds each combination of the three
//! inputs as a row (its test) or a named exclusion, and
//! [`the_framings_are_every_combination_of_hypers_inputs`] checks that it holds all of them.

use std::error::Error;
use std::future::Future;
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::middleware::map_response;
use axum::response::Response;
use http_body::{Body as HttpBody, Frame, SizeHint};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::{StoreConfig, StoreService};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::query_protocol_tests::encode;
use crate::support::held_service::HANG;
use crate::support::{minimal_fixture_path, test_app_with_store};

const CHUNK: usize = 64 * 1024;
const PREFIX_FRAMES: usize = 2;
const PREFIX: usize = PREFIX_FRAMES * CHUNK;
const ROW_WIDTH: usize = 128;
const SIDE: usize = 128;
/// The bytes of [`query`]'s CSV answer.
pub(crate) const WHOLE: usize = (1 + SIDE * SIDE) * ROW_WIDTH;
const PATH: &str = "/dataset/query";

/// An HTTP version a request is sent with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Http {
    V10,
    V11,
    V2,
}

/// The test of a framing the server sends, or why it never sends it.
#[derive(Debug)]
enum Framing {
    Row(&'static str),
    Never(&'static str),
}

const HELD: &str = "an HTTP/1.0 answer is held whole, or is a file sent with its size (the \
     replication image), so its length is known (no close-delimited body)";

/// Every combination of (version, length known, `HEAD`) for an answer: a streamed one's length
/// is unknown until it ends, known only held whole (HTTP/1.0); a file's (the replication
/// image's) is known on every version.
const FRAMINGS: &[(Http, bool, bool, Framing)] = &[
    (
        Http::V10,
        true,
        false,
        Framing::Row("http10_answer_is_whole_with_its_length"),
    ),
    (
        Http::V10,
        true,
        true,
        Framing::Row("http10_head_has_the_length_and_no_body"),
    ),
    (Http::V10, false, false, Framing::Never(HELD)),
    (Http::V10, false, true, Framing::Never(HELD)),
    (
        Http::V11,
        false,
        false,
        Framing::Row("http1_chunked_cut_omits_the_final_chunk"),
    ),
    (
        Http::V11,
        false,
        true,
        Framing::Row("http11_head_has_no_body_and_no_framing"),
    ),
    (
        Http::V11,
        true,
        false,
        Framing::Row("http11_image_has_its_length"),
    ),
    (
        Http::V11,
        true,
        true,
        Framing::Row("http11_image_head_has_its_length_and_no_body"),
    ),
    (
        Http::V2,
        false,
        false,
        Framing::Row("http2_cut_resets_the_stream"),
    ),
    (
        Http::V2,
        false,
        true,
        Framing::Row("http2_head_ends_the_stream_without_data"),
    ),
    (
        Http::V2,
        true,
        false,
        Framing::Row("http2_image_has_its_length"),
    ),
    (
        Http::V2,
        true,
        true,
        Framing::Row("http2_image_head_has_its_length_and_no_data"),
    ),
];

#[test]
fn the_framings_are_every_combination_of_hypers_inputs() {
    let mut listed: Vec<_> = FRAMINGS
        .iter()
        .map(|(http, known, head, _)| (*http, *known, *head))
        .collect();
    listed.sort();
    let mut distinct = listed.clone();
    distinct.dedup();
    assert_eq!(distinct, listed, "a combination is listed twice");
    let mut product = Vec::new();
    for http in [Http::V10, Http::V11, Http::V2] {
        for known in [false, true] {
            for head in [false, true] {
                product.push((http, known, head));
            }
        }
    }
    product.sort();
    assert_eq!(listed, product);
    // Each row is its own test; each exclusion says why.
    let mut rows = Vec::new();
    for (_, _, _, framing) in FRAMINGS {
        match framing {
            Framing::Row(test) => rows.push(*test),
            Framing::Never(why) => assert!(!why.is_empty()),
        }
    }
    let count = rows.len();
    rows.sort();
    rows.dedup();
    assert_eq!(rows.len(), count, "two rows name one test");
}

pub(crate) fn query() -> String {
    // Equal-width CSV header and rows put every 64 KiB boundary between rows.
    // Two VALUES inputs give 16,384 rows with a short request.
    let column = "v".repeat(ROW_WIDTH - 2);
    let value = "x".repeat(ROW_WIDTH - 2);
    let values = "1 ".repeat(SIDE);
    format!(
        "SELECT (\"{value}\" AS ?{column}) WHERE {{ VALUES ?a {{ {values} }} VALUES ?b {{ {values} }} }}"
    )
}

/// Limits how far the HTTP server reads ahead while the client receives two frames.
/// The gate passes all frames, size hints and errors through from the router's body.
struct PauseBody {
    body: Body,
    left: usize,
    resume: Option<oneshot::Receiver<()>>,
}

impl HttpBody for PauseBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.left == 0
            && let Some(resume) = &mut self.resume
        {
            if Pin::new(resume).poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.resume = None;
        }
        let frame = Pin::new(&mut self.body).poll_frame(cx);
        if matches!(&frame, Poll::Ready(Some(Ok(frame))) if frame.is_data()) {
            self.left = self.left.saturating_sub(1);
        }
        frame
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
}

struct RunningServer {
    addr: SocketAddr,
    store: StoreService,
    resume: oneshot::Sender<()>,
    task: JoinHandle<io::Result<()>>,
}

impl RunningServer {
    async fn start() -> Result<Self, Box<dyn Error>> {
        let store = StoreService::new(StoreConfig::in_memory())?;
        let app = test_app_with_store(
            store.clone(),
            PolicyConfig::default(),
            ReasonerConfig::default(),
        )?;
        let (resume, gate) = oneshot::channel();
        let gate = Arc::new(Mutex::new(Some(gate)));
        let app = app.layer(map_response(move |response: Response| {
            let resume = gate.lock().unwrap().take();
            async move {
                response.map(|body| {
                    Body::new(PauseBody {
                        body,
                        left: PREFIX_FRAMES,
                        resume,
                    })
                })
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let task = tokio::spawn(async move { axum::serve(listener, app).await });
        Ok(Self {
            addr,
            store,
            resume,
            task,
        })
    }

    /// The server with its gate open: every frame goes out as the client reads.
    fn ungated(self) -> (SocketAddr, JoinHandle<io::Result<()>>) {
        self.resume.send(()).expect("the server is running");
        (self.addr, self.task)
    }

    fn stop(self) -> JoinHandle<io::Result<()>> {
        // The full answer exceeds the router's output channel plus the two frames
        // we let through. The producer is still registered regardless of timing.
        let running = self.store.running_queries();
        let queries = running.list();
        assert_eq!(queries.len(), 1, "{queries:?}");
        assert!(running.cancel(queries[0].id));
        self.resume.send(()).expect("the HTTP body is still open");
        self.task
    }
}

/// Sends the query as `version`, as a `POST` or (`head`) as a `HEAD` with the query in its
/// URL, with `Connection: close` if `close`; reads the response's head, a 200.
async fn http1_query(
    addr: SocketAddr,
    version: &str,
    head: bool,
    close: bool,
) -> Result<(BufReader<TcpStream>, String), Box<dyn Error>> {
    let mut client = TcpStream::connect(addr).await?;
    let query = query();
    let connection = if close { "Connection: close\r\n" } else { "" };
    let request = if head {
        format!(
            "HEAD {PATH}?query={} {version}\r\nHost: {addr}\r\n{connection}Accept: text/csv\r\n\r\n",
            encode(&query)
        )
    } else {
        format!(
            "POST {PATH} {version}\r\nHost: {addr}\r\n{connection}\
             Content-Type: application/sparql-query\r\nAccept: text/csv\r\n\
             Content-Length: {}\r\n\r\n{query}",
            query.len()
        )
    };
    client.write_all(request.as_bytes()).await?;
    let mut client = BufReader::new(client);
    let mut response = String::new();
    loop {
        let start = response.len();
        assert_ne!(client.read_line(&mut response).await?, 0, "{response:?}");
        if &response[start..] == "\r\n" {
            break;
        }
    }
    assert!(
        response.starts_with(&format!("{version} 200 ")),
        "{response:?}"
    );
    Ok((client, response))
}

/// The value of the header `name` in a response's head.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").find_map(|line| {
        let (field, value) = line.split_once(':')?;
        field.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

async fn read_chunk(client: &mut BufReader<TcpStream>, body: &mut Vec<u8>) -> io::Result<bool> {
    let mut line = String::new();
    if client.read_line(&mut line).await? == 0 {
        return Err(ErrorKind::UnexpectedEof.into());
    }
    let size = line.trim_end().split(';').next().unwrap();
    let size = usize::from_str_radix(size, 16).expect("hexadecimal chunk size");
    if size == 0 {
        loop {
            line.clear();
            if client.read_line(&mut line).await? == 0 {
                return Err(ErrorKind::UnexpectedEof.into());
            }
            if line == "\r\n" {
                return Ok(true);
            }
        }
    }
    if (&mut *client).take(size as u64).read_to_end(body).await? != size {
        return Err(ErrorKind::UnexpectedEof.into());
    }
    let mut crlf = [0; 2];
    client.read_exact(&mut crlf).await?;
    assert_eq!(&crlf, b"\r\n");
    Ok(false)
}

fn reset(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
    )
}

#[tokio::test]
async fn http10_answer_is_whole_with_its_length() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task) = RunningServer::start().await?.ungated();
        let (mut client, head) = http1_query(addr, "HTTP/1.0", false, true).await?;
        let mut body = Vec::new();
        let end = client.read_to_end(&mut body).await;
        task.abort();
        eprintln!("{head:?}; {} data bytes; end {end:?}", body.len());
        assert_eq!(
            header(&head, "content-length"),
            Some(WHOLE.to_string().as_str()),
            "{head:?}"
        );
        assert_eq!(header(&head, "transfer-encoding"), None, "{head:?}");
        assert!(end.is_ok());
        assert_eq!(body.len(), WHOLE);
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn http10_head_has_the_length_and_no_body() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task) = RunningServer::start().await?.ungated();
        let (mut client, head) = http1_query(addr, "HTTP/1.0", true, true).await?;
        let mut body = Vec::new();
        let end = client.read_to_end(&mut body).await;
        task.abort();
        eprintln!("{head:?}; {} data bytes; end {end:?}", body.len());
        assert_eq!(
            header(&head, "content-length"),
            Some(WHOLE.to_string().as_str()),
            "{head:?}"
        );
        assert!(end.is_ok());
        assert!(body.is_empty());
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn http1_chunked_cut_omits_the_final_chunk() -> Result<(), Box<dyn Error>> {
    for close in [true, false] {
        timeout(HANG, chunked_cut(close)).await??;
    }
    Ok(())
}

async fn chunked_cut(close: bool) -> Result<(), Box<dyn Error>> {
    let server = RunningServer::start().await?;
    let (mut client, head) = http1_query(server.addr, "HTTP/1.1", false, close).await?;
    assert_eq!(header(&head, "transfer-encoding"), Some("chunked"));
    let mut body = Vec::new();
    for _ in 0..PREFIX_FRAMES {
        assert!(!read_chunk(&mut client, &mut body).await?);
    }
    assert_eq!(body.len(), PREFIX);
    let task = server.stop();
    let end = loop {
        match read_chunk(&mut client, &mut body).await {
            Ok(false) => continue,
            end => break end,
        }
    };
    task.abort();
    eprintln!(
        "close {close}: {head:?}; {} data bytes; final 16 bytes {:?}; end {end:?}",
        body.len(),
        &body[body.len() - 16..]
    );
    assert!(
        matches!(end, Err(error) if error.kind() == ErrorKind::UnexpectedEof || reset(error.kind())),
        "the aborted result had a final zero-length chunk or an unexpected read error"
    );
    assert!(body.len() < WHOLE);
    Ok(())
}

#[tokio::test]
async fn http11_head_has_no_body_and_no_framing() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task) = RunningServer::start().await?.ungated();
        let (mut client, head) = http1_query(addr, "HTTP/1.1", true, true).await?;
        let mut body = Vec::new();
        let end = client.read_to_end(&mut body).await;
        task.abort();
        eprintln!("{head:?}; {} data bytes; end {end:?}", body.len());
        assert_eq!(header(&head, "content-length"), None, "{head:?}");
        assert_eq!(header(&head, "transfer-encoding"), None, "{head:?}");
        assert!(end.is_ok());
        assert!(body.is_empty());
        Ok(())
    })
    .await?
}

/// An HTTP/2 connection to `addr`, its driver running.
async fn http2(
    addr: SocketAddr,
) -> Result<
    (
        h2::client::SendRequest<Bytes>,
        JoinHandle<Result<(), h2::Error>>,
    ),
    Box<dyn Error>,
> {
    let (client, connection) = h2::client::handshake(TcpStream::connect(addr).await?).await?;
    let connection = tokio::spawn(connection);
    Ok((client.ready().await?, connection))
}

#[tokio::test]
async fn http2_cut_resets_the_stream() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let server = RunningServer::start().await?;
        let (mut client, connection) = http2(server.addr).await?;
        let query = query();
        let request = Request::post(format!("http://{}{PATH}", server.addr))
            .header("content-type", "application/sparql-query")
            .header("accept", "text/csv")
            .header("content-length", query.len())
            .body(())?;
        let (response, mut send) = client.send_request(request, false)?;
        send.send_data(Bytes::from(query), true)?;
        let response = response.await?;
        assert_eq!(response.status(), StatusCode::OK);
        let (head, mut body) = response.into_parts();
        let mut received = 0;
        while received < PREFIX {
            let data = body
                .data()
                .await
                .expect("two data frames before the stop")?;
            received += data.len();
            body.flow_control().release_capacity(data.len())?;
        }
        let task = server.stop();
        let end = loop {
            match body.data().await {
                Some(Ok(data)) => {
                    received += data.len();
                    body.flow_control().release_capacity(data.len())?;
                }
                end => break end,
            }
        };
        task.abort();
        connection.abort();
        eprintln!(
            "{:?} {:?}; {received} data bytes; end {end:?}",
            head.status, head.headers
        );
        assert!(
            matches!(end, Some(Err(error)) if error.is_reset() && error.is_remote()),
            "the cut HTTP/2 body did not end in a peer RST_STREAM"
        );
        assert!(received < WHOLE);
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn http2_head_ends_the_stream_without_data() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task) = RunningServer::start().await?.ungated();
        let (mut client, connection) = http2(addr).await?;
        let request = Request::head(format!("http://{addr}{PATH}?query={}", encode(&query())))
            .header("accept", "text/csv")
            .body(())?;
        let (response, _) = client.send_request(request, true)?;
        let response = response.await?;
        assert_eq!(response.status(), StatusCode::OK);
        let (head, mut body) = response.into_parts();
        let end = body.data().await;
        task.abort();
        connection.abort();
        eprintln!("{:?} {:?}; end {end:?}", head.status, head.headers);
        assert!(end.is_none(), "a HEAD answer's stream ends without data");
        Ok(())
    })
    .await?
}

const IMAGE: &str = "/api/v1/replication/image";

/// A primary on a real port, its store on disk with the minimal fixture: its replication
/// image ([`IMAGE`]) is a file, sent with its size on every version. The directory lives as
/// long as the server must.
pub(crate) async fn image_server()
-> Result<(SocketAddr, JoinHandle<io::Result<()>>, tempfile::TempDir), Box<dyn Error>> {
    use nrese_server::replication::{ReplicationConfig, ReplicationMode};

    let dir = tempfile::tempdir()?;
    let store =
        StoreService::new(StoreConfig::on_disk(dir.path()).with_ontology(minimal_fixture_path()))?;
    let state = nrese_server::AppState::new(
        store,
        nrese_reasoner::ReasonerService::new(ReasonerConfig::default()),
        PolicyConfig::default(),
        nrese_server::ai::AiSuggestionService::disabled(),
        nrese_server::DeploymentPosture::OpenWorkbench,
    )
    .with_replication(ReplicationConfig {
        mode: ReplicationMode::Primary,
        ..ReplicationConfig::default()
    });
    state.mark_ready();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = nrese_server::build_app(state);
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok((addr, task, dir))
}

/// The image asked with `method` as HTTP/1 `version`, the connection closed after it: the
/// response's head (a 200) and all the bytes after it.
pub(crate) async fn http1_image(
    addr: SocketAddr,
    version: &str,
    method: &str,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    let mut client = TcpStream::connect(addr).await?;
    let request =
        format!("{method} {IMAGE} {version}\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    client.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    client.read_to_end(&mut response).await?;
    let end = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("a response head");
    let head = String::from_utf8_lossy(&response[..end]).into_owned();
    assert_eq!(head.split(' ').nth(1), Some("200"), "{head}");
    Ok((head, response[end + 4..].to_vec()))
}

/// The image's length, as its head gives it: a whole number.
fn image_length(head: &str) -> usize {
    header(head, "content-length")
        .and_then(|length| length.parse().ok())
        .unwrap_or_else(|| panic!("no length: {head:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http11_image_has_its_length() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task, _dir) = image_server().await?;
        let (head, body) = http1_image(addr, "HTTP/1.1", "GET").await?;
        task.abort();
        eprintln!("{head:?}; {} data bytes", body.len());
        assert_eq!(header(&head, "transfer-encoding"), None, "{head:?}");
        assert!(!body.is_empty());
        assert_eq!(image_length(&head), body.len());
        Ok(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http11_image_head_has_its_length_and_no_body() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task, _dir) = image_server().await?;
        let (head, body) = http1_image(addr, "HTTP/1.1", "HEAD").await?;
        task.abort();
        eprintln!("{head:?}; {} data bytes", body.len());
        assert!(image_length(&head) > 0, "{head:?}");
        assert!(body.is_empty());
        Ok(())
    })
    .await?
}

/// The image asked with `method` over HTTP/2: the response's `content-length` and the data
/// bytes its stream carried to its end.
async fn http2_image(addr: SocketAddr, method: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let (mut client, connection) = http2(addr).await?;
    let request = Request::builder()
        .method(method)
        .uri(format!("http://{addr}{IMAGE}"))
        .body(())?;
    let (response, _) = client.send_request(request, true)?;
    let response = response.await?;
    assert_eq!(response.status(), StatusCode::OK);
    let (head, mut body) = response.into_parts();
    let mut received = 0;
    while let Some(data) = body.data().await {
        let data = data?;
        received += data.len();
        body.flow_control().release_capacity(data.len())?;
    }
    connection.abort();
    eprintln!("{method}: {:?}; {received} data bytes", head.headers);
    let length = head
        .headers
        .get("content-length")
        .and_then(|length| length.to_str().ok()?.parse().ok())
        .expect("a length");
    Ok((length, received))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http2_image_has_its_length() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task, _dir) = image_server().await?;
        let (length, received) = http2_image(addr, "GET").await?;
        task.abort();
        assert!(received > 0);
        assert_eq!(length, received);
        Ok(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http2_image_head_has_its_length_and_no_data() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let (addr, task, _dir) = image_server().await?;
        let (length, received) = http2_image(addr, "HEAD").await?;
        task.abort();
        assert!(length > 0);
        assert_eq!(received, 0);
        Ok(())
    })
    .await?
}
