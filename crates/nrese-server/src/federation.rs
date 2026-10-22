//! The server's answer to `SERVICE` (SPARQL 1.1 Federated Query): a SPARQL protocol client
//! over HTTP, to the endpoints `federation.allow` names.
//!
//! Queries run on blocking threads; a request goes out on the server's runtime and the
//! thread waits for it, checking the query's cancellation. Redirects aren't followed (an
//! allowed endpoint could otherwise send the server anywhere), and an answer longer than
//! `federation.max_rows` rows is an error.

use std::sync::Arc;
use std::time::Duration;

use nrese_exec::{Budget, StopCheck, StopSource};
use nrese_sparql::{CancellationToken, QueryEvaluationError, ServiceClient, ServiceResults};
use nrese_sparql_results::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};
use nrese_store::FederationConfig;
use reqwest::header::{ACCEPT, CONTENT_TYPE};

mod body;

pub struct HttpServiceClient {
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    config: FederationConfig,
}

impl HttpServiceClient {
    pub fn new(config: FederationConfig, runtime: tokio::runtime::Handle) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(config.timeout_ms))
            .user_agent(concat!("nrese/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            runtime,
            config,
        })
    }
}

impl ServiceClient for HttpServiceClient {
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        budget: &Arc<Budget>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        let stop = StopSource::new(cancellation.map(CancellationToken::flag), None);
        stop.check().map_err(QueryEvaluationError::from)?;
        if !self.config.allows(endpoint) {
            return Err(QueryEvaluationError::Service(format!(
                "SERVICE <{endpoint}>: the endpoint is not in federation.allow (NRESE_FEDERATION_ALLOW)"
            )
            .into()));
        }
        let request = self
            .client
            .post(endpoint)
            .header(
                ACCEPT,
                "application/sparql-results+json, application/sparql-results+xml;q=0.9, \
                 text/tab-separated-values;q=0.5, text/csv;q=0.4",
            )
            .form(&[("query", query)]);
        let fetch = async {
            let mut response = request.send().await.map_err(endpoint_error)?;
            let status = response.status();
            // Never poll an error body, even when it advertises a small length.
            if !status.is_success() {
                return Err(QueryEvaluationError::Service(
                    format!("SERVICE <{endpoint}>: {status}").into(),
                ));
            }
            let format = format_for(
                response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default(),
            );
            let mut body = body::Body::new(budget, self.config.max_response_bytes);
            body.declared(response.content_length())?;
            // A cap-sized prefix still requires the next poll to prove EOF.
            loop {
                stop.check().map_err(QueryEvaluationError::from)?;
                let Some(chunk) = response.chunk().await.map_err(endpoint_error)? else {
                    break;
                };
                body.append(&chunk, &stop)?;
                // Drop incoming backing promptly. It was allocated below reqwest;
                // destination admission is not admission of that still-open owner.
                drop(chunk);
            }
            stop.check().map_err(QueryEvaluationError::from)?;
            Ok((format, body))
        };
        let stopped = async {
            loop {
                if let Err(reason) = stop.check() {
                    return QueryEvaluationError::from(reason);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        let result = self.runtime.block_on(async {
            tokio::select! {
                biased;
                reason = stopped => Err(reason),
                result = fetch => result,
            }
        });
        stop.check().map_err(QueryEvaluationError::from)?;
        let (format, body) = result?;
        let result = parse(format, body.bytes(), self.config.max_rows, budget, &stop);
        stop.check().map_err(QueryEvaluationError::from)?;
        result
    }
}

fn endpoint_error(error: impl std::error::Error + Send + Sync + 'static) -> QueryEvaluationError {
    QueryEvaluationError::Service(Box::new(error))
}

fn format_for(media_type: &str) -> QueryResultsFormat {
    let essence = media_type.split(';').next().unwrap_or_default().trim();
    if [
        "application/sparql-results+xml",
        "application/xml",
        "text/xml",
    ]
    .iter()
    .any(|mime| essence.eq_ignore_ascii_case(mime))
    {
        QueryResultsFormat::Xml
    } else if essence.eq_ignore_ascii_case("text/tab-separated-values") {
        QueryResultsFormat::Tsv
    } else if essence.eq_ignore_ascii_case("text/csv") {
        QueryResultsFormat::Csv
    } else {
        QueryResultsFormat::Json
    }
}

/// Partial parser port: the existing L0 parser still owns unadmitted startup,
/// lexical scratch and the source row. Only retained response copies are admitted
/// here. A new shared L0 control surface must be selected before extending it.
fn parse(
    format: QueryResultsFormat,
    body: &[u8],
    max_rows: usize,
    budget: &Arc<Budget>,
    stop: &dyn StopCheck,
) -> Result<ServiceResults, QueryEvaluationError> {
    stop.check().map_err(QueryEvaluationError::from)?;
    let parsed = QueryResultsParser::from_format(format)
        .for_slice(body)
        .map_err(endpoint_error);
    stop.check().map_err(QueryEvaluationError::from)?;
    match parsed? {
        SliceQueryResultsParserOutput::Boolean(_) => Err(QueryEvaluationError::Service(
            "the endpoint answered a boolean".into(),
        )),
        SliceQueryResultsParserOutput::Solutions(solutions) => {
            let variables = solutions.variables().to_vec();
            let mut result = ServiceResults::new(variables, budget);
            let mut solutions = solutions;
            loop {
                stop.check().map_err(QueryEvaluationError::from)?;
                let next = solutions.next();
                stop.check().map_err(QueryEvaluationError::from)?;
                let Some(solution) = next else {
                    break;
                };
                if result.rows().len() == max_rows {
                    return Err(QueryEvaluationError::Service(
                        format!("more than {max_rows} rows (federation.max_rows)").into(),
                    ));
                }
                let solution = solution.map_err(endpoint_error)?;
                result.push_row(solution.values(), stop)?;
            }
            stop.check().map_err(QueryEvaluationError::from)?;
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_xml_and_caps_the_rows() {
        let budget = Arc::new(Budget::new(1 << 20));
        let stop = nrese_exec::StopSource::none();
        let json = br#"{"head":{"vars":["x","y"]},"results":{"bindings":[
            {"x":{"type":"uri","value":"http://example.com/a"}},
            {"x":{"type":"literal","value":"b"},"y":{"type":"literal","value":"1","datatype":"http://www.w3.org/2001/XMLSchema#integer"}}]}}"#;
        let results = parse(
            format_for("application/sparql-results+json; charset=utf-8"),
            json,
            10,
            &budget,
            &stop,
        )
        .unwrap();
        assert_eq!(results.variables().len(), 2);
        assert_eq!(results.rows().len(), 2);
        assert!(results.rows()[0][1].is_none());
        assert!(parse(QueryResultsFormat::Json, json, 1, &budget, &stop).is_err());
        let xml = br#"<?xml version="1.0"?><sparql xmlns="http://www.w3.org/2005/sparql-results#"><head><variable name="x"/></head><results><result><binding name="x"><uri>http://example.com/a</uri></binding></result></results></sparql>"#;
        assert_eq!(
            parse(QueryResultsFormat::Xml, xml, 10, &budget, &stop)
                .unwrap()
                .rows()
                .len(),
            1
        );
    }

    fn wire_endpoint(wire: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/sparql", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut chunk = [0; 512];
                let count = socket.read(&mut chunk).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&chunk[..count]);
            }
            socket.write_all(&wire).unwrap();
        });
        (endpoint, task)
    }

    #[test]
    fn n7_http_exact_cap_requires_eof_and_rejects_one_more_byte() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let document = br#"{"head":{"vars":[]},"results":{"bindings":[{}]}}"#;
        for ending in [0, 1, 2] {
            let mut wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/sparql-results+json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n",
                document.len(),
            ).into_bytes();
            wire.extend_from_slice(document);
            wire.extend_from_slice(b"\r\n");
            match ending {
                0 => wire.extend_from_slice(b"0\r\n\r\n"),
                1 => wire.extend_from_slice(b"1\r\n \r\n0\r\n\r\n"),
                // A parseable cap-sized prefix without the terminal chunk is not EOF.
                2 => {}
                _ => unreachable!(),
            }
            let (endpoint, server) = wire_endpoint(wire);
            let client = HttpServiceClient::new(
                FederationConfig {
                    allow: vec![endpoint.clone()],
                    max_response_bytes: Some(document.len()),
                    ..FederationConfig::default()
                },
                runtime.handle().clone(),
            )
            .unwrap();
            let budget = Arc::new(Budget::new(1 << 20));
            let result = client.select(&endpoint, "SELECT * WHERE {}", &budget, None);
            server.join().unwrap();
            if ending == 0 {
                let result = result.unwrap();
                assert_eq!(result.rows(), &[Vec::<Option<nrese_rdf::Term>>::new()]);
                assert!(budget.used() > 0);
                drop(result);
            } else {
                let error = result.unwrap_err();
                assert!(matches!(error, QueryEvaluationError::Service(_)));
                if ending == 1 {
                    assert!(error.to_string().contains("max_response_bytes"));
                }
            }
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn n7_http_status_error_does_not_read_its_missing_body() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        // Reading this body's declared bytes would fail with an incomplete-body
        // transport error. Status handling must return the status instead.
        let (endpoint, server) = wire_endpoint(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 10000000\r\nConnection: close\r\n\r\n".to_vec(),
        );
        let client = HttpServiceClient::new(
            FederationConfig {
                allow: vec![endpoint.clone()],
                ..FederationConfig::default()
            },
            runtime.handle().clone(),
        )
        .unwrap();
        let budget = Arc::new(Budget::new(1 << 20));
        let error = client
            .select(&endpoint, "SELECT * WHERE {}", &budget, None)
            .unwrap_err();
        server.join().unwrap();
        assert!(matches!(error, QueryEvaluationError::Service(_)));
        assert!(error.to_string().contains("503"), "{error}");
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn n7_http_wait_polls_the_original_cancellation_token() {
        use std::io::Read;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let budget = Arc::new(Budget::new(1 << 20));
        let token = CancellationToken::new();
        let actuator = token.clone();
        assert!(Arc::ptr_eq(&token.flag(), &actuator.flag()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/sparql", listener.local_addr().unwrap());
        let (finished, wait) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 1024];
            assert_ne!(socket.read(&mut request).unwrap(), 0);
            actuator.cancel();
            // Keep headers pending until the caller drops the request.
            let _ = wait.recv_timeout(Duration::from_secs(5));
        });
        let client = HttpServiceClient::new(
            FederationConfig {
                allow: vec![endpoint.clone()],
                timeout_ms: 1000,
                ..FederationConfig::default()
            },
            runtime.handle().clone(),
        )
        .unwrap();
        let error = client
            .select(&endpoint, "SELECT * WHERE {}", &budget, Some(&token))
            .unwrap_err();
        finished.send(()).unwrap();
        server.join().unwrap();
        assert!(token.is_cancelled());
        assert!(matches!(error, QueryEvaluationError::Cancelled));
        assert_eq!(budget.used(), 0);
    }
}
