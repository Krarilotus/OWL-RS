//! The server's process-wide memory, each test in a process of its own: changing the
//! global fallback must not race other server tests (including when run by cargo test
//! rather than nextest), and a measurement counts every thread of its process
//! ([`measure::alone`]).

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use nrese_exec::measure;
use nrese_exec::memory::{process_limit, set_process_limit};
use nrese_server::ai::AiSuggestionService;
use nrese_server::auth::{AuthConfig, StaticBearerConfig};
use nrese_server::policy::PolicyConfig;
use nrese_server::{AppState, DeploymentPosture, ServerConfig, build_app};
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

#[test]
fn opening_repositories_and_system_stores_preserves_the_startup_policy() {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            set_process_limit(self.0.unwrap_or(0));
        }
    }
    let _restore = Restore(process_limit());
    let mut config = ServerConfig {
        bind_address: "127.0.0.1:0".parse().unwrap(),
        deployment_posture: DeploymentPosture::OpenWorkbench,
        store: StoreConfig::in_memory(),
        reasoner: Default::default(),
        policy: Default::default(),
        ai: Default::default(),
        replication: Default::default(),
    };
    // A nondefault ceiling, and then explicit disable. Neither may be replaced by a
    // default policy while AppState opens the system/access store.
    for limit in [u64::MAX, 0] {
        config.store.process_memory_bytes = limit;
        config.install_process_memory_policy();
        let expected = (limit != 0).then_some(limit);
        let store = StoreService::new(config.store.clone()).unwrap();
        assert_eq!(process_limit(), expected);
        let _unrelated = StoreService::new(StoreConfig::in_memory()).unwrap();
        assert_eq!(process_limit(), expected);
        let _state = AppState::try_new(
            store,
            config.reasoner.clone(),
            config.policy.clone(),
            AiSuggestionService::disabled(),
            config.deployment_posture,
        )
        .unwrap();
        assert_eq!(process_limit(), expected, "system store reset the ceiling");
    }
    // Disabling an embedded watch does not bypass the host's explicit fallback.
    config.store.process_memory_bytes = 1;
    config.install_process_memory_policy();
    let store = StoreService::new(StoreConfig {
        process_memory_bytes: 0,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    assert_eq!(process_limit(), Some(1));
    if nrese_exec::memory::process_bytes().is_some() {
        assert!(
            store
                .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
                .is_err()
        );
    }
}

/// An app over an in-memory store whose graph `urn:n8:g` holds `n` statements about `n`
/// subjects and whose graph `urn:n8:hub` holds `n` statements about one subject, loaded
/// through the Graph Store Protocol.
fn app_with(n: usize, runtime: &tokio::runtime::Runtime) -> axum::Router {
    let state = AppState::new(
        StoreService::new(StoreConfig::in_memory()).unwrap(),
        Default::default(),
        PolicyConfig {
            auth: AuthConfig::BearerStatic(StaticBearerConfig {
                read_token: None,
                admin_token: "admin".to_owned(),
            }),
            ..PolicyConfig::default()
        },
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    );
    state.mark_ready();
    let app = build_app(state);
    type Statement = fn(usize) -> String;
    let graphs: [(&str, Statement); 2] = [
        ("urn%3An8%3Ag", |i| {
            format!("<urn:n8:s{i}> <urn:n8:p> \"value {i:08}\" .\n")
        }),
        ("urn%3An8%3Ahub", |i| {
            format!("<urn:n8:hub> <urn:n8:p> <urn:n8:o{i}> .\n")
        }),
    ];
    for (graph, statement) in graphs {
        let body: String = (0..n).map(statement).collect();
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/dataset/data?graph={graph}"))
            .header("authorization", "Bearer admin")
            .header("content-type", "application/n-triples")
            .body(Body::from(body))
            .unwrap();
        let response = runtime.block_on(app.clone().oneshot(request)).unwrap();
        assert!(response.status().is_success(), "{}", response.status());
    }
    app
}

/// Sends a request and takes its body frame by frame, keeping none: its status, bytes and
/// lines.
async fn send(
    app: axum::Router,
    (method, uri, accept): (Method, &str, &str),
) -> (StatusCode, usize, usize) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer admin")
        .header("accept", accept)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let (mut bytes, mut lines) = (0, 0);
    let mut body = response.into_body();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.unwrap().into_data() {
            bytes += data.len();
            lines += data.iter().filter(|&&b| b == b'\n').count();
        }
    }
    (status, bytes, lines)
}

/// Graph Store reads, `HEAD` and the N-Quads export hold the same memory for a graph four
/// times larger: they stream what they read instead of holding the graph (decoded, then
/// serialised) as they did (SS-15). The hub row is one subject's fan-out, which the JSON-LD
/// writer once held whole.
#[test]
fn server_output_holds_the_same_memory_for_a_graph_four_times_larger() {
    if !measure::alone() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let n = 20_000;
    let apps = [app_with(n, &runtime), app_with(4 * n, &runtime)];
    let rows: [(&str, Method, &str, &str); 4] = [
        (
            "GET N-Triples",
            Method::GET,
            "/dataset/data?graph=urn%3An8%3Ag",
            "application/n-triples",
        ),
        (
            "GET JSON-LD, one subject",
            Method::GET,
            "/dataset/data?graph=urn%3An8%3Ahub",
            "application/ld+json",
        ),
        (
            "HEAD",
            Method::HEAD,
            "/dataset/data?graph=urn%3An8%3Ag",
            "application/n-triples",
        ),
        (
            "export",
            Method::GET,
            "/ops/api/admin/dataset/backup",
            "application/n-quads",
        ),
    ];
    // Unmeasured first: the blocking pool's thread, lazy statics, the views.
    for app in &apps {
        for (_, method, uri, accept) in &rows {
            runtime.block_on(send(app.clone(), (method.clone(), uri, accept)));
        }
    }
    for (name, method, uri, accept) in &rows {
        let mut peaks = Vec::new();
        for (app, size) in apps.iter().zip([n, 4 * n]) {
            let app = app.clone();
            let request = (method.clone(), *uri, *accept);
            let ((status, bytes, lines), peak) =
                measure::measure(|| runtime.block_on(send(app, request)));
            assert_eq!(status, StatusCode::OK, "{name}");
            match *name {
                "HEAD" => assert_eq!(bytes, 0, "{name}"),
                "GET N-Triples" => assert_eq!(lines, size, "{name}"),
                // Both graphs and the closing checksum comment.
                "export" => assert_eq!(lines, 2 * size + 1, "{name}"),
                _ => assert!(bytes > size * 20, "{name}: {bytes} bytes"),
            }
            peaks.push(peak);
        }
        let growth = peaks[1].saturating_sub(peaks[0]);
        assert!(
            growth <= 256 * 1024,
            "{name}: {} bytes at n, {} at 4n",
            peaks[0],
            peaks[1]
        );
    }
}
