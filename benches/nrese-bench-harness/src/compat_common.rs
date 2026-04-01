use anyhow::{Context, Result, anyhow};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, RequestBuilder, StatusCode};
use std::time::Duration;

use crate::layout::ServiceTarget;
use crate::model::{
    CompatCase, CompatCaseReport, CompatGraphTarget, CompatHeaders, CompatKind, compat_kind_label,
    compat_operation_label,
};
use crate::normalize::{classify_http_body, normalize_content_type};

#[derive(Debug)]
pub struct HttpOutcome {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RequestExecutionOptions {
    pub timeout_ms: Option<u64>,
}

impl RequestExecutionOptions {
    pub fn from_case(case: &CompatCase) -> Self {
        Self {
            timeout_ms: case.timeout_ms,
        }
    }
}

#[derive(Debug)]
pub enum RequestOutcome {
    Http(HttpOutcome),
    Timeout { timeout_ms: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseSemantics {
    pub status: u16,
    pub content_type: Option<String>,
    pub body_class: String,
}

pub async fn execute_query_raw(
    client: &Client,
    target: &ServiceTarget,
    query: &str,
    accept: &str,
    extra_headers: &CompatHeaders,
    options: RequestExecutionOptions,
) -> Result<RequestOutcome> {
    let request = apply_case_headers(
        apply_target_auth(
            client.post(target.query_endpoint()).body(query.to_owned()),
            target,
        ),
        target,
        [
            ("content-type", "application/sparql-query"),
            ("accept", accept),
        ],
        extra_headers,
    )?;

    send_request(request, target, "query", options).await
}

pub async fn execute_update_raw(
    client: &Client,
    target: &ServiceTarget,
    update: &str,
    extra_headers: &CompatHeaders,
    options: RequestExecutionOptions,
) -> Result<RequestOutcome> {
    let request = apply_case_headers(
        apply_target_auth(
            client
                .post(target.update_endpoint())
                .body(update.to_owned()),
            target,
        ),
        target,
        [("content-type", "application/sparql-update")],
        extra_headers,
    )?;

    send_request(request, target, "update", options).await
}

pub async fn execute_graph_read_raw(
    client: &Client,
    target: &ServiceTarget,
    graph_target: &CompatGraphTarget,
    accept: &str,
    extra_headers: &CompatHeaders,
    options: RequestExecutionOptions,
) -> Result<RequestOutcome> {
    let request = graph_request(client.get(target.data_endpoint_base()), graph_target);
    let request = apply_case_headers(
        apply_target_auth(request, target),
        target,
        [("accept", accept)],
        extra_headers,
    )?;

    send_request(request, target, "graph read", options).await
}

pub async fn execute_graph_head_raw(
    client: &Client,
    target: &ServiceTarget,
    graph_target: &CompatGraphTarget,
    accept: &str,
    extra_headers: &CompatHeaders,
    options: RequestExecutionOptions,
) -> Result<RequestOutcome> {
    let request = graph_request(client.head(target.data_endpoint_base()), graph_target);
    let request = apply_case_headers(
        apply_target_auth(request, target),
        target,
        [("accept", accept)],
        extra_headers,
    )?;

    send_request(request, target, "graph head", options).await
}

pub async fn execute_graph_write_raw(
    client: &Client,
    target: &ServiceTarget,
    graph_target: &CompatGraphTarget,
    content_type: &str,
    payload: &[u8],
    replace: bool,
    extra_headers: &CompatHeaders,
    options: RequestExecutionOptions,
) -> Result<RequestOutcome> {
    let request = if replace {
        client.put(target.data_endpoint_base())
    } else {
        client.post(target.data_endpoint_base())
    };
    let request = graph_request(request, graph_target).body(payload.to_vec());
    let request = apply_case_headers(
        apply_target_auth(request, target),
        target,
        [("content-type", content_type)],
        extra_headers,
    )?;

    send_request(request, target, "graph write", options).await
}

pub async fn execute_graph_delete_raw(
    client: &Client,
    target: &ServiceTarget,
    graph_target: &CompatGraphTarget,
    extra_headers: &CompatHeaders,
    options: RequestExecutionOptions,
) -> Result<RequestOutcome> {
    let request = graph_request(client.delete(target.data_endpoint_base()), graph_target);
    let request = apply_case_headers(
        apply_target_auth(request, target),
        target,
        [],
        extra_headers,
    )?;

    send_request(request, target, "graph delete", options).await
}

pub fn require_success_http<'a>(
    target: &ServiceTarget,
    family: &str,
    outcome: &'a RequestOutcome,
) -> Result<&'a HttpOutcome> {
    match outcome {
        RequestOutcome::Http(http) if http.status.is_success() => Ok(http),
        RequestOutcome::Http(http) => Err(anyhow!(
            "{} {} failed with status {} and body {}",
            target.label,
            family,
            http.status,
            String::from_utf8_lossy(&http.body)
        )),
        RequestOutcome::Timeout { timeout_ms } => Err(anyhow!(
            "{} {} timed out{}",
            target.label,
            family,
            format_timeout_suffix(*timeout_ms)
        )),
    }
}

pub fn classify_response_semantics(outcome: &RequestOutcome) -> ResponseSemantics {
    match outcome {
        RequestOutcome::Http(http) => ResponseSemantics {
            status: http.status.as_u16(),
            content_type: normalize_content_type(http.content_type.as_deref()),
            body_class: classify_http_body(http.content_type.as_deref(), &http.body).to_owned(),
        },
        RequestOutcome::Timeout { .. } => ResponseSemantics {
            status: 0,
            content_type: None,
            body_class: "client-timeout".to_owned(),
        },
    }
}

pub fn build_response_semantics_report(
    case: &CompatCase,
    left: &RequestOutcome,
    right: &RequestOutcome,
) -> Result<CompatCaseReport> {
    match case.kind {
        CompatKind::StatusAndContentType | CompatKind::StatusContentTypeBodyClass => {
            let left_semantics = classify_response_semantics(left);
            let right_semantics = classify_response_semantics(right);
            let matched = left_semantics.status == right_semantics.status
                && left_semantics.content_type == right_semantics.content_type
                && (case.kind == CompatKind::StatusAndContentType
                    || left_semantics.body_class == right_semantics.body_class);

            Ok(CompatCaseReport {
                name: case.name.clone(),
                operation: compat_operation_label(case.operation),
                kind: compat_kind_label(case.kind),
                left_status: left_semantics.status,
                right_status: right_semantics.status,
                left_content_type: left_semantics.content_type.clone(),
                right_content_type: right_semantics.content_type.clone(),
                left_body_class: Some(left_semantics.body_class.clone()),
                right_body_class: Some(right_semantics.body_class.clone()),
                matched,
                left_summary: format_response_semantics_summary(&left_semantics, left),
                right_summary: format_response_semantics_summary(&right_semantics, right),
            })
        }
        _ => Err(anyhow!(
            "response semantics report requested for a non-semantics compat kind"
        )),
    }
}

fn graph_request(request: RequestBuilder, graph_target: &CompatGraphTarget) -> RequestBuilder {
    match graph_target {
        CompatGraphTarget::DefaultGraph => request.query(&[("default", "")]),
        CompatGraphTarget::NamedGraph { iri } => request.query(&[("graph", iri)]),
    }
}

fn apply_target_auth(request: RequestBuilder, target: &ServiceTarget) -> RequestBuilder {
    match &target.basic_auth {
        Some(auth) => request.basic_auth(&auth.username, Some(&auth.password)),
        None => request,
    }
}

fn apply_case_headers<const N: usize>(
    request: RequestBuilder,
    target: &ServiceTarget,
    default_headers: [(&str, &str); N],
    extra_headers: &CompatHeaders,
) -> Result<RequestBuilder> {
    let mut headers = HeaderMap::new();

    for (name, value) in default_headers {
        headers.insert(parse_header_name(name)?, parse_header_value(name, value)?);
    }

    for (name, value) in &target.default_headers {
        headers.insert(parse_header_name(name)?, parse_header_value(name, value)?);
    }

    for (name, value) in extra_headers {
        headers.insert(parse_header_name(name)?, parse_header_value(name, value)?);
    }

    Ok(request.headers(headers))
}

fn parse_header_name(name: &str) -> Result<HeaderName> {
    HeaderName::from_bytes(name.as_bytes())
        .with_context(|| format!("invalid header name in compat case: {name}"))
}

fn parse_header_value(name: &str, value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value)
        .with_context(|| format!("invalid header value for compat header {name}"))
}

async fn into_http_outcome(response: reqwest::Response) -> Result<HttpOutcome> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let body = response
        .bytes()
        .await
        .context("failed reading response body")?
        .to_vec();

    Ok(HttpOutcome {
        status,
        content_type,
        body,
    })
}

async fn send_request(
    request: RequestBuilder,
    target: &ServiceTarget,
    family: &str,
    options: RequestExecutionOptions,
) -> Result<RequestOutcome> {
    let request = apply_timeout(request, options);
    match request.send().await {
        Ok(response) => into_http_outcome(response).await.map(RequestOutcome::Http),
        Err(error) if error.is_timeout() => Ok(RequestOutcome::Timeout {
            timeout_ms: options.timeout_ms,
        }),
        Err(error) => {
            Err(error).with_context(|| format!("{family} request failed against {}", target.label))
        }
    }
}

fn apply_timeout(request: RequestBuilder, options: RequestExecutionOptions) -> RequestBuilder {
    match options.timeout_ms {
        Some(timeout_ms) => request.timeout(Duration::from_millis(timeout_ms)),
        None => request,
    }
}

fn format_response_semantics_summary(
    semantics: &ResponseSemantics,
    outcome: &RequestOutcome,
) -> String {
    match outcome {
        RequestOutcome::Timeout { timeout_ms } => format!(
            "status=<timeout> content-type=<missing> body-class={}{}",
            semantics.body_class,
            format_timeout_suffix(*timeout_ms)
        ),
        RequestOutcome::Http(_) => format!(
            "status={} content-type={} body-class={}",
            semantics.status,
            semantics.content_type.as_deref().unwrap_or("<missing>"),
            semantics.body_class
        ),
    }
}

fn format_timeout_suffix(timeout_ms: Option<u64>) -> String {
    match timeout_ms {
        Some(timeout_ms) => format!(" after {}ms", timeout_ms),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use crate::layout::ServiceTarget;
    use crate::model::{CompatCase, CompatHeaders, CompatKind, CompatOperation};
    use reqwest::{Client, StatusCode};

    use super::{
        HttpOutcome, RequestExecutionOptions, RequestOutcome, apply_case_headers,
        build_response_semantics_report, classify_response_semantics, execute_query_raw,
    };

    #[test]
    fn response_semantics_normalize_problem_json() {
        let outcome = HttpOutcome {
            status: StatusCode::BAD_REQUEST,
            content_type: Some("application/problem+json; charset=utf-8".to_owned()),
            body: br#"{"title":"bad"}"#.to_vec(),
        };

        let semantics = classify_response_semantics(&RequestOutcome::Http(outcome));
        assert_eq!(semantics.status, 400);
        assert_eq!(
            semantics.content_type.as_deref(),
            Some("application/problem+json")
        );
        assert_eq!(semantics.body_class, "problem-json");
    }

    #[test]
    fn response_semantics_classify_timeout_as_client_timeout() {
        let semantics = classify_response_semantics(&RequestOutcome::Timeout {
            timeout_ms: Some(15),
        });
        assert_eq!(semantics.status, 0);
        assert!(semantics.content_type.is_none());
        assert_eq!(semantics.body_class, "client-timeout");
    }

    #[test]
    fn response_semantics_report_includes_body_class() {
        let case = CompatCase {
            name: "invalid-graph-write".to_owned(),
            operation: CompatOperation::GraphPutEffect,
            query: None,
            accept: "application/n-triples".to_owned(),
            update: None,
            verify_query: None,
            graph_target: None,
            graph_payload: None,
            graph_content_type: None,
            graph_replace: true,
            generated_payload: None,
            timeout_ms: None,
            request_headers: CompatHeaders::new(),
            kind: CompatKind::StatusContentTypeBodyClass,
        };
        let outcome = HttpOutcome {
            status: StatusCode::BAD_REQUEST,
            content_type: Some("application/problem+json".to_owned()),
            body: br#"{"title":"Bad Request"}"#.to_vec(),
        };

        let report = build_response_semantics_report(
            &case,
            &RequestOutcome::Http(outcome),
            &RequestOutcome::Http(HttpOutcome {
                status: StatusCode::BAD_REQUEST,
                content_type: Some("application/problem+json".to_owned()),
                body: br#"{"title":"Bad Request"}"#.to_vec(),
            }),
        )
        .expect("report");
        assert!(report.matched);
        assert_eq!(report.left_body_class.as_deref(), Some("problem-json"));
    }

    #[test]
    fn timeout_response_semantics_report_uses_timeout_summary() {
        let case = CompatCase {
            name: "slow-query".to_owned(),
            operation: CompatOperation::Query,
            query: Some("SELECT * WHERE { ?s ?p ?o }".to_owned()),
            accept: "application/sparql-results+json".to_owned(),
            update: None,
            verify_query: None,
            graph_target: None,
            graph_payload: None,
            graph_content_type: None,
            graph_replace: true,
            generated_payload: None,
            timeout_ms: Some(25),
            request_headers: CompatHeaders::new(),
            kind: CompatKind::StatusContentTypeBodyClass,
        };

        let report = build_response_semantics_report(
            &case,
            &RequestOutcome::Timeout {
                timeout_ms: Some(25),
            },
            &RequestOutcome::Timeout {
                timeout_ms: Some(25),
            },
        )
        .expect("report");

        assert!(report.matched);
        assert_eq!(report.left_status, 0);
        assert_eq!(report.left_body_class.as_deref(), Some("client-timeout"));
        assert!(report.left_summary.contains("after 25ms"));
    }

    #[test]
    fn custom_case_headers_override_default_request_headers() {
        let client = Client::new();
        let request = client.post("http://example.invalid");
        let request = apply_case_headers(
            request,
            &ServiceTarget::nrese_with_headers(
                "http://example.invalid".to_owned(),
                CompatHeaders::new(),
            ),
            [("accept", "application/sparql-results+json")],
            &CompatHeaders::from([("accept".to_owned(), "application/n-triples".to_owned())]),
        )
        .expect("headers")
        .build()
        .expect("request");

        assert_eq!(
            request
                .headers()
                .get("accept")
                .and_then(|value| value.to_str().ok()),
            Some("application/n-triples")
        );
    }

    #[test]
    fn target_headers_are_applied_before_case_headers() {
        let client = Client::new();
        let request = client.post("http://example.invalid");
        let target = ServiceTarget::nrese_with_headers(
            "http://example.invalid".to_owned(),
            CompatHeaders::from([("x-env".to_owned(), "pack".to_owned())]),
        );

        let request = apply_case_headers(
            request,
            &target,
            [("accept", "application/sparql-results+json")],
            &CompatHeaders::new(),
        )
        .expect("headers")
        .build()
        .expect("request");

        assert_eq!(
            request
                .headers()
                .get("x-env")
                .and_then(|value| value.to_str().ok()),
            Some("pack")
        );
    }

    #[tokio::test]
    async fn execute_query_raw_returns_timeout_outcome_when_request_exceeds_case_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("listener addr");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer);
            thread::sleep(Duration::from_millis(150));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/sparql-results+json\r\nContent-Length: 16\r\n\r\n{\"boolean\":true}",
            );
        });

        let client = Client::builder().build().expect("client");
        let target =
            ServiceTarget::nrese_with_headers(format!("http://{addr}"), CompatHeaders::new());
        let outcome = execute_query_raw(
            &client,
            &target,
            "ASK WHERE { ?s ?p ?o }",
            "application/sparql-results+json",
            &CompatHeaders::new(),
            RequestExecutionOptions {
                timeout_ms: Some(25),
            },
        )
        .await
        .expect("timeout outcome");

        match outcome {
            RequestOutcome::Timeout {
                timeout_ms: Some(25),
            } => {}
            other => panic!("expected timeout outcome, got {other:?}"),
        }

        server.join().expect("server join");
    }
}
