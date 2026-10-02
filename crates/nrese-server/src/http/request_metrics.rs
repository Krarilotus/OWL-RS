//! Request outcomes and latencies for `/metrics` (O3): per kind of request, how many ended
//! in each status class, and a histogram of the time to the response's start (the head;
//! a streamed body may still be on its way).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::state::AppState;

/// The kinds of request the metrics tell apart, by path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Query,
    Update,
    /// The single endpoint for queries and updates (`/sparql`, `/dataset`).
    Sparql,
    GraphStore,
    Shacl,
    Other,
}

impl Kind {
    const ALL: [Self; 6] = [
        Self::Query,
        Self::Update,
        Self::Sparql,
        Self::GraphStore,
        Self::Shacl,
        Self::Other,
    ];

    fn of(path: &str) -> Self {
        match path {
            "/dataset/query" => Self::Query,
            "/dataset/update" => Self::Update,
            "/dataset" => Self::Sparql,
            "/dataset/data" => Self::GraphStore,
            _ if path == crate::runtime_posture::SPARQL_ENDPOINT => Self::Sparql,
            _ if path == crate::runtime_posture::SHACL_ENDPOINT => Self::Shacl,
            _ => Self::Other,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Update => "update",
            Self::Sparql => "sparql",
            Self::GraphStore => "graph_store",
            Self::Shacl => "shacl",
            Self::Other => "other",
        }
    }
}

/// Upper bounds of the latency buckets, in seconds (Prometheus' `le`).
const BUCKETS: [f64; 14] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0, 30.0,
];

#[derive(Default)]
struct PerKind {
    /// Responses by status class: 1xx to 5xx.
    status: [AtomicU64; 5],
    /// Requests per bucket (not cumulative; rendering adds them up).
    buckets: [AtomicU64; BUCKETS.len()],
    count: AtomicU64,
    sum_micros: AtomicU64,
}

/// Counters shared by every request; lock-free.
#[derive(Default)]
pub struct RequestMetrics {
    kinds: [PerKind; Kind::ALL.len()],
}

impl RequestMetrics {
    pub fn record(&self, kind: Kind, status: u16, elapsed: Duration) {
        let metrics = &self.kinds[kind as usize];
        let class = usize::from(status / 100).clamp(1, 5) - 1;
        metrics.status[class].fetch_add(1, Ordering::Relaxed);
        let seconds = elapsed.as_secs_f64();
        if let Some(bucket) = BUCKETS.iter().position(|&bound| seconds <= bound) {
            metrics.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        }
        metrics.count.fetch_add(1, Ordering::Relaxed);
        metrics
            .sum_micros
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
    }

    /// The metrics in Prometheus' text format.
    pub fn render(&self, out: &mut String) {
        use std::fmt::Write as _;
        out.push_str(
            "# HELP nrese_http_responses_total Responses by kind of request and status class.\n\
             # TYPE nrese_http_responses_total counter\n",
        );
        for kind in Kind::ALL {
            let metrics = &self.kinds[kind as usize];
            for (class, count) in metrics.status.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "nrese_http_responses_total{{kind=\"{}\",status=\"{}xx\"}} {}",
                    kind.label(),
                    class + 1,
                    count.load(Ordering::Relaxed)
                );
            }
        }
        out.push_str(
            "# HELP nrese_http_request_duration_seconds Time to the response's start, by kind of request.\n\
             # TYPE nrese_http_request_duration_seconds histogram\n",
        );
        for kind in Kind::ALL {
            let metrics = &self.kinds[kind as usize];
            let label = kind.label();
            let mut cumulative = 0;
            for (bound, count) in BUCKETS.iter().zip(&metrics.buckets) {
                cumulative += count.load(Ordering::Relaxed);
                let _ = writeln!(
                    out,
                    "nrese_http_request_duration_seconds_bucket{{kind=\"{label}\",le=\"{bound}\"}} {cumulative}"
                );
            }
            let count = metrics.count.load(Ordering::Relaxed);
            let sum = metrics.sum_micros.load(Ordering::Relaxed) as f64 / 1e6;
            let _ = writeln!(
                out,
                "nrese_http_request_duration_seconds_bucket{{kind=\"{label}\",le=\"+Inf\"}} {count}\n\
                 nrese_http_request_duration_seconds_sum{{kind=\"{label}\"}} {sum}\n\
                 nrese_http_request_duration_seconds_count{{kind=\"{label}\"}} {count}"
            );
        }
    }
}

/// The middleware that records every request.
pub async fn track(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let kind = Kind::of(request.uri().path());
    let start = Instant::now();
    let response = next.run(request).await;
    state
        .request_metrics()
        .record(kind, response.status().as_u16(), start.elapsed());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_classes_and_buckets() {
        let metrics = RequestMetrics::default();
        metrics.record(Kind::Query, 200, Duration::from_micros(300));
        metrics.record(Kind::Query, 200, Duration::from_millis(20));
        metrics.record(Kind::Query, 413, Duration::from_millis(2));
        metrics.record(Kind::Update, 503, Duration::from_secs(60));
        let mut text = String::new();
        metrics.render(&mut text);
        assert!(text.contains("nrese_http_responses_total{kind=\"query\",status=\"2xx\"} 2"));
        assert!(text.contains("nrese_http_responses_total{kind=\"query\",status=\"4xx\"} 1"));
        assert!(text.contains("nrese_http_responses_total{kind=\"update\",status=\"5xx\"} 1"));
        assert!(text.contains(
            "nrese_http_request_duration_seconds_bucket{kind=\"query\",le=\"0.0005\"} 1"
        ));
        assert!(
            text.contains(
                "nrese_http_request_duration_seconds_bucket{kind=\"query\",le=\"0.025\"} 3"
            )
        );
        assert!(text.contains("nrese_http_request_duration_seconds_count{kind=\"query\"} 3"));
        // Slower than the last bucket: only in +Inf.
        assert!(
            text.contains(
                "nrese_http_request_duration_seconds_bucket{kind=\"update\",le=\"30\"} 0"
            )
        );
        assert!(
            text.contains(
                "nrese_http_request_duration_seconds_bucket{kind=\"update\",le=\"+Inf\"} 1"
            )
        );
        assert_eq!(Kind::of("/dataset/query"), Kind::Query);
        assert_eq!(Kind::of("/healthz"), Kind::Other);
    }
}
