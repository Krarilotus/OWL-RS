//! Spec: result_stream's "After the first chunk" contract requires an aborted
//! transfer to be distinguishable from a complete answer at the HTTP client.

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

use crate::support::held_service::HANG;
use crate::support::test_app_with_store;

const CHUNK: usize = 64 * 1024;
const PREFIX_FRAMES: usize = 2;
const PREFIX: usize = PREFIX_FRAMES * CHUNK;
const ROW_WIDTH: usize = 128;
const SIDE: usize = 128;
const WHOLE: usize = (1 + SIDE * SIDE) * ROW_WIDTH;
const PATH: &str = "/dataset/query";

fn query() -> String {
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

async fn http1_query(
    addr: SocketAddr,
    version: &str,
) -> Result<(BufReader<TcpStream>, String), Box<dyn Error>> {
    let mut client = TcpStream::connect(addr).await?;
    let query = query();
    let request = format!(
        "POST {PATH} {version}\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/sparql-query\r\nAccept: text/csv\r\n\
         Content-Length: {}\r\n\r\n{query}",
        query.len()
    );
    client.write_all(request.as_bytes()).await?;
    let mut client = BufReader::new(client);
    let mut head = String::new();
    loop {
        let start = head.len();
        assert_ne!(client.read_line(&mut head).await?, 0, "{head:?}");
        if &head[start..] == "\r\n" {
            break;
        }
    }
    assert!(head.starts_with(&format!("{version} 200 ")), "{head:?}");
    Ok((client, head))
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
async fn http1_chunked_cut_omits_the_final_chunk() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let server = RunningServer::start().await?;
        let (mut client, head) = http1_query(server.addr, "HTTP/1.1").await?;
        assert!(head.to_ascii_lowercase().contains("\r\ntransfer-encoding: chunked\r\n"));
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
            "{head:?}; {} data bytes; final 16 bytes {:?}; end {end:?}",
            body.len(),
            &body[body.len() - 16..]
        );
        assert!(
            matches!(end, Err(error) if error.kind() == ErrorKind::UnexpectedEof || reset(error.kind())),
            "the aborted result had a final zero-length chunk or an unexpected read error"
        );
        assert!(body.len() < WHOLE);
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn http1_close_delimited_cut_signals_a_reset() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let server = RunningServer::start().await?;
        let (mut client, head) = http1_query(server.addr, "HTTP/1.0").await?;
        let headers = head.to_ascii_lowercase();
        assert!(!headers.contains("\r\ncontent-length:"), "{head:?}");
        assert!(!headers.contains("\r\ntransfer-encoding:"), "{head:?}");
        let mut body = vec![0; PREFIX];
        client.read_exact(&mut body).await?;
        let task = server.stop();
        let end = client.read_to_end(&mut body).await;
        task.abort();
        eprintln!(
            "{head:?}; {} data bytes; final 16 bytes {:?}; end {end:?}",
            body.len(),
            &body[body.len() - 16..]
        );
        assert!(
            matches!(end, Err(error) if reset(error.kind())),
            "the close-delimited reply ended without a reset"
        );
        assert!(body.len() < WHOLE);
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn http2_cut_resets_the_stream() -> Result<(), Box<dyn Error>> {
    timeout(HANG, async {
        let server = RunningServer::start().await?;
        let (client, connection) =
            h2::client::handshake(TcpStream::connect(server.addr).await?).await?;
        let connection = tokio::spawn(connection);
        let mut client = client.ready().await?;
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
