//! Spec: the approved protocol settings preserve defaults, permit a proxy path
//! prefix, and expire idle transactions atomically at the configured time.
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
        std::fs::write(dir.path().join("config.toml"),
            "[server]\npublic_url = 'https://public.invalid/prefix/'\n[store]\ntransaction_idle_seconds = 1\n").unwrap();
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

// Spec: expiry refuses QUERY, UPDATE and COMMIT with 404 and publishes no
// queued change; the unset timeout remains 600 and keeps a transaction alive.
#[tokio::test]
async fn configured_idle_expiry_refuses_all_actions_without_partial_writes() {
    for (settings, expires) in [
        (vec![("NRESE_TRANSACTION_IDLE_SECONDS", "1")], true),
        (vec![], false),
    ] {
        let server = start(&settings, false).await;
        let client = client();
        let location = begin(&client, &server).await;
        let path = reqwest::Url::parse(&location).unwrap().path().to_owned();
        let url = format!("{}{path}", server.base);
        let ping = client
            .put(format!("{url}?action=PING"))
            .send()
            .await
            .unwrap();
        let idle = ping.text().await.unwrap();
        assert_eq!(
            client
                .put(format!("{url}?action=UPDATE"))
                .header("content-type", "application/sparql-update")
                .body("INSERT DATA { <urn:pending> <urn:p> <urn:o> }")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        tokio::time::sleep(Duration::from_millis(1200)).await;
        for (action, content, body) in [
            ("QUERY", "application/sparql-query", "ASK {}"),
            (
                "UPDATE",
                "application/sparql-update",
                "INSERT DATA { <urn:late> <urn:p> <urn:o> }",
            ),
            ("COMMIT", "", ""),
        ] {
            let response = client
                .put(format!("{url}?action={action}"))
                .header("content-type", content)
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if expires { 404 } else { 200 },
                "{action}"
            );
        }
        assert_eq!(idle.trim(), if expires { "1000" } else { "600000" });
        let response: serde_json::Value = client
            .post(format!("{}/repositories/nrese", server.base))
            .header("content-type", "application/sparql-query")
            .header("accept", "application/sparql-results+json")
            .body("ASK { ?s <urn:p> <urn:o> }")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["boolean"], !expires);
        if !expires {
            assert!(location.starts_with("https://forwarded.invalid/"));
        }
    }
}
