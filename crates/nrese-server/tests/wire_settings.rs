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
fn isolated_command(dir: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nrese-server"));
    command.current_dir(dir).env_clear();
    // Keep platform runtime paths, but isolate all server settings.
    for name in ["PATH", "SystemRoot", "TEMP", "TMP", "HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("NRESE_REASONING_MODE", "disabled");
    command
}
async fn start(settings: &[(&str, &str)], file: bool) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut command = isolated_command(dir.path());
    command
        .env("NRESE_BIND_ADDR", address.to_string())
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
    for (file, configured) in [
        (false, "https://public.invalid/prefix/"),
        (true, "https://public.invalid/prefix/"),
        (false, "HTTPS://PUBLIC.INVALID:443/prefix/"),
    ] {
        let server = start(&[("NRESE_PUBLIC_URL", configured)], file).await;
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

// Spec: appending the transaction path preserves significant slashes in a base path.
#[tokio::test]
async fn configured_public_url_preserves_trailing_path_segments() {
    let server = start(
        &[("NRESE_PUBLIC_URL", "https://public.invalid/prefix///")],
        false,
    )
    .await;
    let location = begin(&client(), &server).await;
    assert!(
        location.starts_with("https://public.invalid/prefix///repositories/nrese/transactions/"),
        "{location}"
    );
}

// Spec: an unset public URL keeps the existing forwarded-host behavior.
#[tokio::test]
async fn unset_public_url_uses_the_forwarded_host() {
    let server = start(&[], false).await;
    let location = begin(&client(), &server).await;
    assert!(location.starts_with("https://forwarded.invalid/repositories/nrese/transactions/"));
}

// Spec: public_url is an absolute HTTP(S) base without userinfo, controls,
// query or fragment; a refusal names the setting without echoing its value.
#[test]
fn invalid_public_urls_are_refused_without_echoing_configured_text() {
    let mut wrong = Vec::new();
    for (case, value) in [
        (
            "userinfo",
            "https://wire-marker:unused@public.invalid/prefix",
        ),
        ("newline", "https://public.invalid/wire-marker\npath"),
        ("control", "https://public.invalid/wire-marker\u{0085}"),
        ("query", "https://public.invalid/prefix?wire-marker=1"),
        ("fragment", "https://public.invalid/prefix#wire-marker"),
        ("relative", "/wire-marker/path"),
        ("scheme", "ftp://public.invalid/wire-marker"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let stdout = dir.path().join("stdout");
        let stderr = dir.path().join("stderr");
        let child = isolated_command(dir.path())
            .arg("check-config")
            .env("NRESE_PUBLIC_URL", value)
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let mut process = Server {
            child,
            base: String::new(),
            _dir: dir,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = process.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "check-config deadline");
            std::thread::sleep(Duration::from_millis(10));
        };
        let summary = std::fs::read_to_string(stdout).unwrap();
        let error = std::fs::read_to_string(stderr).unwrap();
        let rejected = !status.success();
        let named = error.contains("server.public_url");
        let disclosed = summary.contains("wire-marker") || error.contains("wire-marker");
        if !rejected || !named || disclosed {
            wrong.push(format!(
                "{case}: refused={rejected}, setting named={named}, disclosed={disclosed}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

// Spec: configuration-file refusals name the key/type or file/exact position
// with our fixed reason, never parser text or rejected values, including i128.
#[test]
fn rejected_file_values_are_never_echoed_in_configuration_errors() {
    let mut wrong = Vec::new();
    for (key, kind, contents, forbidden) in [
        (
            "server.public_url",
            "text",
            "[server]\npublic_url = [\"https://wire-marker:unused@public.invalid/prefix\"]\n",
            "wire-marker",
        ),
        (
            "server.public_url",
            "text",
            "[server]\npublic_url = { address = \"https://wire-marker:unused@public.invalid/prefix\" }\n",
            "wire-marker",
        ),
        (
            "auth.bearer_static.admin_token",
            "text",
            "[auth.bearer_static]\nadmin_token = [\"wire-marker\"]\n",
            "wire-marker",
        ),
        (
            "config.toml",
            "invalid TOML at line 2, column 15",
            "[auth.bearer_static]\nadmin_token = 18446744073709551617\n",
            "18446744073709551617",
        ),
        (
            "config.toml",
            "invalid TOML at line 2, column 14",
            "[server]\npublic_url = https://wire-marker:unused@public.invalid/prefix\n",
            "wire-marker",
        ),
    ] {
        for mode in ["check-config", "startup"] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config.toml");
            let stdout = dir.path().join("stdout");
            let stderr = dir.path().join("stderr");
            std::fs::write(&config, contents).unwrap();
            let mut command = isolated_command(dir.path());
            command
                .env("NRESE_CONFIG_PATH", &config)
                .env("NRESE_BIND_ADDR", "127.0.0.1:0");
            if mode == "check-config" {
                command.arg("check-config");
            }
            let child = command
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap();
            let mut process = Server {
                child,
                base: String::new(),
                _dir: dir,
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = process.child.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "configuration refusal deadline");
                std::thread::sleep(Duration::from_millis(10));
            };
            let summary = std::fs::read_to_string(stdout).unwrap();
            let error = std::fs::read_to_string(stderr).unwrap();
            let disclosed = summary.contains(forbidden) || error.contains(forbidden);
            let named = if key == "config.toml" {
                error.trim() == format!("Error: {}: {kind}", config.display())
            } else {
                error.contains(key) && error.contains(kind)
            };
            if disclosed || status.success() || !named {
                wrong.push(format!(
                    "{key}/{mode}: disclosed={disclosed}, refused={}, named={named}",
                    !status.success()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

// Spec: a refused URL is rejected before listening, so neither a startup log
// nor an HTTP response can expose its distinctive configured text.
#[tokio::test]
async fn a_refused_public_url_cannot_start_an_http_response() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = dir.path().join("stdout");
    let stderr = dir.path().join("stderr");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = isolated_command(dir.path())
        .env("NRESE_BIND_ADDR", address.to_string())
        .env(
            "NRESE_PUBLIC_URL",
            "https://wire-marker:unused@public.invalid/prefix",
        )
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let mut server = Server {
        child,
        base: format!("http://{address}"),
        _dir: dir,
    };
    let client = client();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (refused, served, disclosed) = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break (!status.success(), false, false);
        }
        if let Ok(response) = client
            .post(format!("{}/repositories/nrese/transactions", server.base))
            .send()
            .await
        {
            let headers = response.headers().clone();
            let body = response.text().await.unwrap();
            let disclosed = headers
                .values()
                .any(|v| v.as_bytes().windows(11).any(|b| b == b"wire-marker"))
                || body.contains("wire-marker");
            break (false, true, disclosed);
        }
        assert!(Instant::now() < deadline, "startup deadline");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let summary = std::fs::read_to_string(stdout).unwrap();
    let error = std::fs::read_to_string(stderr).unwrap();
    assert_eq!(
        (
            refused,
            served,
            disclosed,
            summary.contains("wire-marker"),
            error.contains("wire-marker")
        ),
        (true, false, false, false, false)
    );
}
