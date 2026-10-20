//! HTTP lab: requests through the server's whole router in one process (no sockets, no
//! client): what a request pays besides the store's work, middleware and routing included.
//!
//! ```text
//! cargo run --release -p nrese-server --example http_lab -- [--quads 100000] [--batch 2000] \
//!     [--runs 5] [--json OUT]
//! ```
//!
//! An in-memory store of `--quads` generated statements (8 a subject), without reasoning;
//! then `--runs` samples of each workload, each the mean of `--batch` requests back to back,
//! in microseconds, every response checked:
//! - `point_query`: `GET /dataset/query` of one subject's object for one predicate (one row);
//! - `small_update`: `POST /dataset/update` inserting one new statement (a commit each).
//!
//! The JSON report has, per workload, `p50_us` and `samples_us`, and `checked`: whether
//! every response was what it must be (else the times don't count).

use std::time::Instant;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{StoreConfig, StoreService};
use tower::util::ServiceExt;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct Args {
    quads: u64,
    batch: u64,
    runs: usize,
    json: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        quads: 100_000,
        batch: 2_000,
        runs: 5,
        json: None,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--quads" => args.quads = value()?.parse().map_err(|e| format!("--quads: {e}"))?,
            "--batch" => args.batch = value()?.parse().map_err(|e| format!("--batch: {e}"))?,
            "--runs" => args.runs = value()?.parse().map_err(|e| format!("--runs: {e}"))?,
            "--json" => args.json = Some(value()?),
            other => {
                return Err(format!(
                    "unknown argument {other}; see the top of http_lab.rs"
                ));
            }
        }
    }
    Ok(args)
}

/// `text` as a URL query value.
fn encoded(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// One request through the router: its status and body.
async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("served");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, body.to_vec())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = args()?;
    let store = StoreService::new(StoreConfig::in_memory())?;
    let subjects = args.quads.div_ceil(8);
    let data: String = (0..args.quads)
        .map(|n| {
            format!(
                "<http://example.com/s{}> <http://example.com/p{}> \"value {n}\" .\n",
                n / 8,
                n % 8
            )
        })
        .collect();
    let file = std::env::temp_dir().join(format!("http-lab-{}.nt", std::process::id()));
    std::fs::write(&file, data)?;
    let loaded = store.bulk_load(&nrese_store::BulkLoadRequest {
        files: vec![file.clone()],
        replace: false,
        graph: nrese_store::GraphTarget::DefaultGraph,
        invalid: None,
    });
    std::fs::remove_file(&file)?;
    loaded?;
    let state = AppState::new(
        store,
        ReasonerService::new(ReasonerConfig::for_mode(
            nrese_reasoner::ReasoningMode::Disabled,
        )),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    );
    state.mark_ready();
    let app = build_app(state);

    let point = |i: u64| {
        let s = (i * 7919) % subjects;
        let query =
            format!("SELECT ?o WHERE {{ <http://example.com/s{s}> <http://example.com/p0> ?o }}");
        Request::builder()
            .method(Method::GET)
            .uri(format!("/dataset/query?query={}", encoded(&query)))
            .header(header::ACCEPT, "text/tab-separated-values")
            .body(Body::empty())
            .expect("request")
    };
    let update = |i: u64| {
        Request::builder()
            .method(Method::POST)
            .uri("/dataset/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(format!(
                "INSERT DATA {{ <http://example.com/u{i}> <http://example.com/q> \"new\" }}"
            )))
            .expect("request")
    };
    // The header and one row.
    let one_row = |status: StatusCode, body: &[u8]| {
        status == StatusCode::OK && body.iter().filter(|&&b| b == b'\n').count() == 2
    };
    let done = |status: StatusCode, _: &[u8]| status.is_success();
    let (point_us, point_ok) = measure(&app, &args, point, one_row).await;
    let (update_us, update_ok) = measure(&app, &args, update, done).await;
    let checked = point_ok && update_ok;
    let p50 = |values: &[f64]| {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[sorted.len() / 2]
    };
    let list = |values: &[f64]| {
        values
            .iter()
            .map(|v| format!("{v:.3}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    eprintln!(
        "http_lab: point query p50 {:.2} µs, small update p50 {:.2} µs; checked: {checked}",
        p50(&point_us),
        p50(&update_us)
    );
    let report = format!(
        "{{\"quads\": {}, \"batch\": {}, \"runs\": {}, \"checked\": {checked}, \
         \"point_query\": {{\"p50_us\": {:.3}, \"samples_us\": [{}]}}, \
         \"small_update\": {{\"p50_us\": {:.3}, \"samples_us\": [{}]}}}}\n",
        args.quads,
        args.batch,
        args.runs,
        p50(&point_us),
        list(&point_us),
        p50(&update_us),
        list(&update_us)
    );
    match &args.json {
        Some(path) => std::fs::write(path, report)?,
        None => print!("{report}"),
    }
    Ok(())
}

/// `args.runs` samples (after one unmeasured batch), each the mean time of `args.batch`
/// requests `make` makes, in microseconds; and whether `ok` held for every response.
async fn measure(
    app: &axum::Router,
    args: &Args,
    make: impl Fn(u64) -> Request<Body>,
    ok: impl Fn(StatusCode, &[u8]) -> bool,
) -> (Vec<f64>, bool) {
    let (mut samples, mut checked, mut next) = (Vec::new(), true, 0u64);
    for sample in 0..=args.runs {
        let started = Instant::now();
        for _ in 0..args.batch {
            let (status, body) = send(app, make(next)).await;
            next += 1;
            checked &= ok(status, &body);
        }
        if sample > 0 {
            samples.push(started.elapsed().as_secs_f64() * 1e6 / args.batch as f64);
        }
    }
    (samples, checked)
}
