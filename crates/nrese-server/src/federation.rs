//! The server's answer to `SERVICE` (SPARQL 1.1 Federated Query): a SPARQL protocol client
//! over HTTP, to the endpoints `federation.allow` names.
//!
//! Queries run on blocking threads; a request goes out on the server's runtime and the
//! thread waits for it, checking the query's cancellation. Redirects aren't followed (an
//! allowed endpoint could otherwise send the server anywhere), and an answer longer than
//! `federation.max_rows` rows is an error.
//!
//! The answer is the query's memory: its body is charged to the query's budget as it
//! arrives, before it is copied, and each row it decodes to before it is kept. An error
//! status is the endpoint's failure, read no further than [`ERROR_TEXT`] bytes of its body.
//! What the transport buffers below that (its read buffers, a stream's queue) is not
//! charged.

use std::mem::size_of;
use std::time::Duration;

use nrese_exec::{Budget, ChargedVec, Reservation};
use nrese_rdf::{NamedOrBlankNode, Term};
use nrese_sparql::{
    CancellationToken, QueryEvaluationError, ServiceCall, ServiceClient, ServiceResults,
};
use nrese_sparql_results::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};
use nrese_store::FederationConfig;
use reqwest::header::{ACCEPT, CONTENT_TYPE};

/// The most of an error status's body read, for its message.
pub const ERROR_TEXT: usize = 500;

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

/// The endpoint's failure, `what` said of it.
fn failed(endpoint: &str, what: impl std::fmt::Display) -> QueryEvaluationError {
    QueryEvaluationError::Service(format!("SERVICE <{endpoint}>: {what}").into())
}

impl ServiceClient for HttpServiceClient {
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        call: ServiceCall<'_>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        let ServiceCall {
            cancellation,
            budget,
            answer,
        } = call;
        if !self.config.allows(endpoint) {
            return Err(failed(
                endpoint,
                "the endpoint is not in federation.allow (NRESE_FEDERATION_ALLOW)",
            ));
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
            let mut response = request
                .send()
                .await
                .map_err(|error| failed(endpoint, error))?;
            let status = response.status();
            let media_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            // An error status is the failure: of its body, only what its message shows.
            let limit = if status.is_success() {
                usize::MAX
            } else {
                ERROR_TEXT
            };
            let mut body = ChargedVec::<u8, &Budget>::new(budget);
            while body.len() < limit {
                let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|error| failed(endpoint, error))?
                else {
                    break;
                };
                body.extend_from_slice(&chunk[..chunk.len().min(limit - body.len())])?;
            }
            if !status.is_success() {
                return Err(failed(
                    endpoint,
                    format_args!("{status}: {}", String::from_utf8_lossy(&body)),
                ));
            }
            Ok((media_type, body))
        };
        let token = cancellation.cloned();
        let cancelled = async move {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if token.as_ref().is_some_and(CancellationToken::is_cancelled) {
                    return;
                }
            }
        };
        let (media_type, body) = self.runtime.block_on(async {
            tokio::select! {
                result = fetch => result,
                () = cancelled => Err(QueryEvaluationError::Cancelled),
            }
        })?;
        parse(&media_type, &body, self.config.max_rows, answer).map_err(|error| match error {
            QueryEvaluationError::Service(error) => failed(endpoint, error),
            error => error,
        })
    }
}

/// The endpoint's failure to give an answer that parses.
fn unparsed(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> QueryEvaluationError {
    QueryEvaluationError::Service(error.into())
}

/// The solutions of a SPARQL results document of the media type `media_type`, each row
/// charged to `answer` before it is kept.
fn parse(
    media_type: &str,
    body: &[u8],
    max_rows: usize,
    answer: &mut Reservation,
) -> Result<ServiceResults, QueryEvaluationError> {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let format = match essence.as_str() {
        "application/sparql-results+xml" | "application/xml" | "text/xml" => {
            QueryResultsFormat::Xml
        }
        "text/tab-separated-values" => QueryResultsFormat::Tsv,
        "text/csv" => QueryResultsFormat::Csv,
        _ => QueryResultsFormat::Json,
    };
    match QueryResultsParser::from_format(format)
        .for_slice(body)
        .map_err(unparsed)?
    {
        SliceQueryResultsParserOutput::Boolean(_) => {
            Err(unparsed("the endpoint answered a boolean"))
        }
        SliceQueryResultsParserOutput::Solutions(solutions) => {
            let variables = solutions.variables().to_vec();
            let mut rows = Vec::new();
            for solution in solutions {
                if rows.len() == max_rows {
                    return Err(unparsed(format!(
                        "more than {max_rows} rows (federation.max_rows)"
                    )));
                }
                let solution = solution.map_err(unparsed)?;
                answer.charge(row_bytes(variables.iter().map(|v| solution.get(v))))?;
                rows.push(variables.iter().map(|v| solution.get(v).cloned()).collect());
            }
            Ok(ServiceResults { variables, rows })
        }
    }
}

/// At most what a row of `values` holds once kept: its place in the rows (twice, as they
/// double when they grow), a slot per value and the values' text.
fn row_bytes<'t>(values: impl Iterator<Item = Option<&'t Term>>) -> usize {
    2 * size_of::<Vec<Option<Term>>>()
        + values
            .map(|value| size_of::<Option<Term>>() + value.map_or(0, text_bytes))
            .sum::<usize>()
}

/// At most the heap a term holds: its IRI, label or literal parts, its triple's parts.
fn text_bytes(term: &Term) -> usize {
    match term {
        Term::NamedNode(n) => n.as_str().len(),
        Term::BlankNode(b) => b.as_str().len(),
        Term::Literal(l) => {
            l.value().len() + l.language().map_or(0, str::len) + l.datatype().as_str().len()
        }
        Term::Triple(t) => {
            let subject = match &t.subject {
                NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
                NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
            };
            size_of::<nrese_rdf::Triple>()
                + subject
                + t.predicate.as_str().len()
                + text_bytes(&t.object)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_xml_and_caps_the_rows() {
        let json = br#"{"head":{"vars":["x","y"]},"results":{"bindings":[
            {"x":{"type":"uri","value":"http://example.com/a"}},
            {"x":{"type":"literal","value":"b"},"y":{"type":"literal","value":"1","datatype":"http://www.w3.org/2001/XMLSchema#integer"}}]}}"#;
        let mut answer = Reservation::new(std::sync::Arc::new(Budget::unlimited()));
        let results = parse(
            "application/sparql-results+json; charset=utf-8",
            json,
            10,
            &mut answer,
        )
        .unwrap();
        assert_eq!(results.variables.len(), 2);
        assert_eq!(results.rows.len(), 2);
        assert!(results.rows[0][1].is_none());
        assert!(parse("application/sparql-results+json", json, 1, &mut answer).is_err());
        let xml = br#"<?xml version="1.0"?><sparql xmlns="http://www.w3.org/2005/sparql-results#"><head><variable name="x"/></head><results><result><binding name="x"><uri>http://example.com/a</uri></binding></result></results></sparql>"#;
        assert_eq!(
            parse("application/sparql-results+xml", xml, 10, &mut answer)
                .unwrap()
                .rows
                .len(),
            1
        );
    }

    /// The decoded rows are the query's memory, whatever the body's size: rows holding more
    /// than the budget are refused, and the refusal holds nothing of it.
    #[test]
    fn decoded_rows_are_charged_to_the_query() {
        let rows = 1_000;
        let bindings: Vec<String> = (0..rows)
            .map(|i| format!(r#"{{"o":{{"type":"uri","value":"http://e/{i}"}}}}"#))
            .collect();
        let json = format!(
            r#"{{"head":{{"vars":["o"]}},"results":{{"bindings":[{}]}}}}"#,
            bindings.join(",")
        );
        // Room for the rows' value slots, not for the rows themselves.
        let budget = std::sync::Arc::new(Budget::new(rows * size_of::<Option<Term>>()));
        let mut answer = Reservation::new(std::sync::Arc::clone(&budget));
        let refused = parse(
            "application/sparql-results+json",
            json.as_bytes(),
            rows,
            &mut answer,
        );
        assert!(
            matches!(refused, Err(QueryEvaluationError::MemoryLimit(_))),
            "{refused:?}"
        );
        drop(answer);
        assert_eq!(budget.used(), 0);
    }
}
