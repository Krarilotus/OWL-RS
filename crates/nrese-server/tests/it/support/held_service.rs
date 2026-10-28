//! A SPARQL endpoint that accepts a `SERVICE` call and never answers. A query that calls it
//! runs, held on I/O rather than by the speed of the host, until its deadline or a
//! cancellation stops it; the call's connection closing shows that the query let go of it.

use std::error::Error;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;

use nrese_server::federation::HttpServiceClient;
use nrese_store::FederationConfig;
use tokio::net::{TcpListener, TcpStream};

/// How long a test waits for anything before it fails instead of hanging.
pub const HANG: Duration = Duration::from_secs(60);

pub struct HeldEndpoint {
    listener: TcpListener,
    url: String,
}

impl HeldEndpoint {
    pub async fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/sparql", listener.local_addr()?);
        Ok(Self { listener, url })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The server's `SERVICE` client, allowed to call this endpoint, with a request timeout
    /// (an hour) no test reaches: only the query's deadline or its cancellation ends a call.
    pub fn client(&self) -> Result<Arc<HttpServiceClient>, Box<dyn Error>> {
        let federation = FederationConfig {
            allow: vec![self.url.clone()],
            timeout_ms: 3_600_000,
            ..FederationConfig::default()
        };
        Ok(Arc::new(HttpServiceClient::new(
            federation,
            tokio::runtime::Handle::current(),
        )?))
    }

    /// The `SERVICE` call of the request `running`, once it has connected; an error if the
    /// request ends first.
    pub async fn call<T: std::fmt::Debug>(
        &self,
        running: &mut tokio::task::JoinHandle<T>,
    ) -> Result<HeldCall, Box<dyn Error>> {
        // Real time, not tokio's: with the test's clock paused, tokio's timers don't run while
        // the query's blocking evaluator does.
        let (fired, watchdog) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(HANG);
            let _ = fired.send(());
        });
        let stream = tokio::select! {
            accepted = self.listener.accept() => accepted
                .map(|(stream, _)| stream)
                .map_err(|error| error.to_string()),
            ended = running => Err(format!(
                "the request ended before its SERVICE call connected: {ended:?}"
            )),
            _ = watchdog => Err(format!("no SERVICE call reached the endpoint in {HANG:?}")),
        }?;
        Ok(HeldCall(stream))
    }
}

/// A call the endpoint holds: it reads the request and never answers.
pub struct HeldCall(TcpStream);

impl HeldCall {
    /// Waits until the caller closes the connection.
    pub async fn closed(self) -> Result<(), Box<dyn Error>> {
        let mut buffer = [0u8; 4096];
        let closed = async {
            loop {
                self.0.readable().await?;
                match self.0.try_read(&mut buffer) {
                    Ok(0) => return Ok(()),
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                        ) =>
                    {
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        tokio::time::timeout(HANG, closed).await.map_err(|_| {
            format!("the SERVICE call was still open after {HANG:?}: the query didn't let go of it")
        })??;
        Ok(())
    }
}
