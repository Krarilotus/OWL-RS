//! Spec: the public URL preserves defaults and permits a proxy path prefix.
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    base: String,
    _dir: tempfile::TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
async fn start(settings: &[(&str, &str)], file: bool) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut command = Command::new(env!("CARGO_BIN_EXE_nrese-server"));
    command.current_dir(dir.path()).env_clear();
    // Keep platform runtime paths, but isolate all server settings.
    for name in ["PATH", "SystemRoot", "TEMP", "TMP", "HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("NRESE_BIND_ADDR", address.to_string())
        .env("NRESE_REASONING_MODE", "disabled")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if file {
        std::fs::write(
            dir.path().join("config.toml"),
            "[server]\npublic_url = 'https://public.invalid/prefix/'\n",
        )
        .unwrap();
        command.env("NRESE_CONFIG_PATH", dir.path().join("config.toml"));
    } else {
        command.envs(settings.iter().copied());
    }
    let mut server = Server {
        child: command.spawn().unwrap(),
        base: format!("http://{address}"),
        _dir: dir,
    };
    let client = client();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if client
            .get(format!("{}/readyz", server.base))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return server;
        }
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "server exited before ready"
        );
        assert!(Instant::now() < deadline, "server readiness deadline");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
async fn begin(client: &reqwest::Client, server: &Server) -> String {
    let response = client
        .post(format!("{}/repositories/nrese/transactions", server.base))
        .header("x-forwarded-host", "forwarded.invalid")
        .header("x-forwarded-proto", "https")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.headers()["location"].to_str().unwrap().to_owned()
}

// Spec: public URL overrides forwarded headers; a proxy strips only its prefix
// while the client follows the returned Location through QUERY/UPDATE/COMMIT.
#[tokio::test]
async fn configured_public_url_is_followed_through_a_stripping_proxy() {
    for file in [false, true] {
        let server = start(
            &[("NRESE_PUBLIC_URL", "https://public.invalid/prefix/")],
            file,
        )
        .await;
        let upstream = client();
        let base = server.base.clone();
        let proxy = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let upstream = upstream.clone();
            let base = base.clone();
            async move {
                let (parts, body) = request.into_parts();
                let path = parts
                    .uri
                    .path_and_query()
                    .unwrap()
                    .as_str()
                    .strip_prefix("/prefix")
                    .unwrap();
                let bytes = axum::body::to_bytes(body, 65536).await.unwrap();
                let response = upstream
                    .request(parts.method, format!("{base}{path}"))
                    .headers(parts.headers)
                    .body(bytes)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                (status, response.bytes().await.unwrap())
            }
        });
        let location = begin(&client(), &server).await;
        assert!(
            location.starts_with("https://public.invalid/prefix/repositories/nrese/transactions/"),
            "{location}"
        );
        use tower::ServiceExt;
        for (action, method, content, body) in [
            ("QUERY", "POST", "application/sparql-query", "ASK {}"),
            (
                "UPDATE",
                "PUT",
                "application/sparql-update",
                "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
            ),
            ("COMMIT", "PUT", "", ""),
        ] {
            let request = axum::http::Request::builder()
                .method(method)
                .uri(format!("{location}?action={action}"))
                .header("content-type", content)
                .body(axum::body::Body::from(body))
                .unwrap();
            assert_eq!(proxy.clone().oneshot(request).await.unwrap().status(), 200);
        }
    }
}

// Spec: an unset public URL keeps the existing forwarded-host behavior.
#[tokio::test]
async fn unset_public_url_uses_the_forwarded_host() {
    let server = start(&[], false).await;
    let location = begin(&client(), &server).await;
    assert!(location.starts_with("https://forwarded.invalid/repositories/nrese/transactions/"));
}
