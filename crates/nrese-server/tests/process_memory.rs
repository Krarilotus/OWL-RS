//! The server's process-wide memory, each test in a process of its own: changing the
//! global fallback must not race other server tests (including when run by cargo test
//! rather than nextest), and a measurement counts every thread of its process
//! ([`measure::alone`]).

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use nrese_exec::measure;
use nrese_exec::memory::{READS_MEMORY, process_limit, set_process_limit};
use nrese_reasoner::ReasonerService;
use nrese_server::ai::AiSuggestionService;
use nrese_server::auth::{AuthConfig, StaticBearerConfig};
use nrese_server::http::MAX_BUFFERED_OUTPUT_BYTES;
use nrese_server::policy::PolicyConfig;
use nrese_server::{AppState, DeploymentPosture, ServerConfig, build_app};
use nrese_store::{ProcessMemory, StoreConfig, StoreService};
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
        config.store.process_memory_bytes = ProcessMemory::Bytes(limit);
        let installed = config.install_process_memory_policy();
        if limit != 0 && !READS_MEMORY {
            // A limit this target can't read is refused at start-up, naming its setting.
            let message = format!("{:#}", installed.unwrap_err());
            assert!(message.contains("NRESE_PROCESS_MEMORY_BYTES"), "{message}");
            continue;
        }
        installed.unwrap();
        let expected = (limit != 0).then_some(limit);
        let store = StoreService::new(config.store.clone()).unwrap();
        assert_eq!(process_limit(), expected);
        let _unrelated = StoreService::new(StoreConfig::in_memory()).unwrap();
        assert_eq!(process_limit(), expected);
        let _state = AppState::try_new(
            store,
            ReasonerService::new(config.reasoner.clone()),
            config.policy.clone(),
            AiSuggestionService::disabled(),
            config.deployment_posture,
        )
        .unwrap();
        assert_eq!(process_limit(), expected, "system store reset the ceiling");
    }
    // Disabling an embedded watch does not bypass the host's explicit fallback.
    config.store.process_memory_bytes = ProcessMemory::Bytes(1);
    let installed = config.install_process_memory_policy();
    if !READS_MEMORY {
        let message = format!("{:#}", installed.unwrap_err());
        assert!(message.contains("NRESE_PROCESS_MEMORY_BYTES"), "{message}");
        return;
    }
    installed.unwrap();
    let store = StoreService::new(StoreConfig {
        process_memory_bytes: ProcessMemory::Bytes(0),
        ..StoreConfig::in_memory()
    })
    .unwrap();
    assert_eq!(process_limit(), Some(1));
    assert!(
        store
            .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
            .is_err()
    );
}

/// An app over an in-memory store whose graph `urn:n8:g` holds `n` statements about `n`
/// subjects and whose graph `urn:n8:hub` holds `n` statements about one subject, loaded
/// through the Graph Store Protocol.
fn app_with(n: usize, runtime: &tokio::runtime::Runtime) -> axum::Router {
    let state = AppState::new(
        StoreService::new(StoreConfig::in_memory()).unwrap(),
        ReasonerService::new(Default::default()),
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

async fn response(
    app: axum::Router,
    (method, uri, accept): (Method, &str, &str),
) -> axum::response::Response {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer admin")
        .header("accept", accept)
        .body(Body::empty())
        .unwrap();
    app.oneshot(request).await.unwrap()
}

/// Sends a request and takes its body frame by frame, keeping none: its status, bytes and
/// lines.
async fn send(app: axum::Router, request: (Method, &str, &str)) -> (StatusCode, usize, usize) {
    let response = response(app, request).await;
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
        assert_output_growth(name, [peaks[0], peaks[1]]);
    }
}

fn assert_output_growth(name: &str, peaks: [usize; 2]) {
    let growth = peaks[1].saturating_sub(peaks[0]);
    // A streamed response's bounded channel may be empty at n and full at 4n. Keep the
    // existing fixed-overhead allowance, adding only its size-independent payload.
    let allowance = 256 * 1024
        + if name == "HEAD" {
            0
        } else {
            MAX_BUFFERED_OUTPUT_BYTES
        };
    eprintln!("{name}: peaks={peaks:?}, growth={growth}, allowance={allowance}");
    assert!(
        growth <= allowance,
        "{name}: {} bytes at n, {} at 4n",
        peaks[0],
        peaks[1]
    );
}

#[test]
fn export_growth_accepts_the_bounded_channel_occupancy() {
    // Spec: IB0622-25e2 / SS-15: scheduling may fill the bounded export channel.
    // gate-push-a9435e267b-1794200086 measured these peaks without retaining the graph.
    assert_output_growth("export", [292_588, 598_064]);
}

#[test]
fn get_growth_accepts_a_channel_filled_by_an_unread_body() {
    // Spec: SS-15 / master's controlled witness: scheduling alone may fill GET's channel.
    if !measure::alone() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let app = app_with(80_000, &runtime);
    let request = (
        Method::GET,
        "/dataset/data?graph=urn%3An8%3Ag",
        "application/n-triples",
    );
    runtime.block_on(send(app.clone(), request.clone()));
    // The recorded red exceeded the old allowance. This drained baseline remains
    // schedule-dependent; only the unread endpoint below is synchronised.
    let ((status, _, lines), drained) =
        measure::measure(|| runtime.block_on(send(app.clone(), request.clone())));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lines, 80_000);
    let ((status, buffered, chunk, failed), unread) = measure::measure(|| {
        runtime.block_on(async {
            let response = response(app, request).await;
            let status = response.status();
            // The only blocking worker is the producer's. This marker waits until the
            // unread channel's stall deadline stops it; no sleep guesses when it fills.
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            let mut body = response.into_body();
            let (mut buffered, mut chunk, mut failed) = (0, 0, false);
            while let Some(frame) = body.frame().await {
                match frame {
                    Ok(frame) => {
                        if let Ok(data) = frame.into_data() {
                            chunk = data.len();
                            buffered += chunk;
                        }
                    }
                    Err(_) => failed = true,
                }
            }
            (status, buffered, chunk, failed)
        })
    });
    assert_eq!(status, StatusCode::OK);
    assert!(failed, "the producer must stop on backpressure");
    // The queue and first frame remain; the pending send and replacement buffer went
    // with the stopped producer. All queued frames are full, before its final flush.
    assert_eq!(buffered + 2 * chunk, MAX_BUFFERED_OUTPUT_BYTES);
    assert!(unread >= MAX_BUFFERED_OUTPUT_BYTES);
    eprintln!(
        "unread GET: buffered={buffered}, chunk={chunk}, queue_payload={}, peak={unread}",
        MAX_BUFFERED_OUTPUT_BYTES - 3 * chunk
    );
    assert_output_growth("GET N-Triples", [drained, unread]);
}
