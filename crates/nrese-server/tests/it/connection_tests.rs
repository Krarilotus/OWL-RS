//! HTTP over a real connection, where clients differ in how they send: what in-process
//! requests (`oneshot`) can't show.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::support::test_app;

/// One response's status line and body from `stream` (`Content-Length` framed).
fn response(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    Ok((head.lines().next().unwrap_or_default().to_owned(), body))
}

/// A client that sends the headers, then the body a moment later (as Python's
/// `http.client` does), to a handler that doesn't read the body: the answer comes after
/// the body, and the connection stays open for the next request (found by the HTTP soak:
/// the server answered and closed under the body, and the client saw an aborted
/// connection).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_sent_after_its_headers_keeps_the_connection() {
    let app = test_app().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    let answers = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let mut stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.write_all(
            b"POST /api/v1/repositories/nrese/sessions HTTP/1.1\r\nHost: test\r\n\
              Content-Type: application/json\r\nContent-Length: 2\r\n\r\n",
        )?;
        std::thread::sleep(Duration::from_millis(200));
        stream.write_all(b"{}")?;
        let first = response(&mut stream)?;
        stream.write_all(b"GET /readyz HTTP/1.1\r\nHost: test\r\n\r\n")?;
        let second = response(&mut stream)?;
        Ok((first.0, second.0))
    })
    .await
    .unwrap();
    let (first, second) = answers.expect("both answers on one connection");
    assert!(first.starts_with("HTTP/1.1 201"), "{first}");
    assert!(second.starts_with("HTTP/1.1 200"), "{second}");
}
