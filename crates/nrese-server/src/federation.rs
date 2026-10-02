//! The server's answer to `SERVICE` (SPARQL 1.1 Federated Query): a SPARQL protocol client
//! over HTTP, to the endpoints `federation.allow` names.
//!
//! Queries run on blocking threads; a request goes out on the server's runtime and the
//! thread waits for it, checking the query's cancellation. Redirects aren't followed (an
//! allowed endpoint could otherwise send the server anywhere), and an answer longer than
//! `federation.max_rows` rows is an error.

use std::error::Error;
use std::time::Duration;

use nrese_sparql::{CancellationToken, ServiceClient, ServiceResults};
use nrese_sparql_results::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};
use nrese_store::FederationConfig;
use reqwest::header::{ACCEPT, CONTENT_TYPE};

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

type BoxError = Box<dyn Error + Send + Sync>;

impl ServiceClient for HttpServiceClient {
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, BoxError> {
        if !self.config.allows(endpoint) {
            return Err(format!(
                "SERVICE <{endpoint}>: the endpoint is not in federation.allow (NRESE_FEDERATION_ALLOW)"
            )
            .into());
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
        let fetch = async move {
            let response = request.send().await?;
            let status = response.status();
            let media_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let body = response.bytes().await?;
            if !status.is_success() {
                let text = String::from_utf8_lossy(&body[..body.len().min(500)]).into_owned();
                return Err::<_, BoxError>(format!("{status}: {text}").into());
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
                () = cancelled => Err("SERVICE request cancelled".into()),
            }
        })?;
        parse(&media_type, &body, self.config.max_rows)
            .map_err(|error| format!("SERVICE <{endpoint}>: {error}").into())
    }
}

/// The solutions of a SPARQL results document of the media type `media_type`.
fn parse(media_type: &str, body: &[u8], max_rows: usize) -> Result<ServiceResults, BoxError> {
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
    match QueryResultsParser::from_format(format).for_slice(body)? {
        SliceQueryResultsParserOutput::Boolean(_) => Err("the endpoint answered a boolean".into()),
        SliceQueryResultsParserOutput::Solutions(solutions) => {
            let variables = solutions.variables().to_vec();
            let mut rows = Vec::new();
            for solution in solutions {
                if rows.len() == max_rows {
                    return Err(format!("more than {max_rows} rows (federation.max_rows)").into());
                }
                let solution = solution?;
                rows.push(variables.iter().map(|v| solution.get(v).cloned()).collect());
            }
            Ok(ServiceResults { variables, rows })
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
        let results = parse("application/sparql-results+json; charset=utf-8", json, 10).unwrap();
        assert_eq!(results.variables.len(), 2);
        assert_eq!(results.rows.len(), 2);
        assert!(results.rows[0][1].is_none());
        assert!(parse("application/sparql-results+json", json, 1).is_err());
        let xml = br#"<?xml version="1.0"?><sparql xmlns="http://www.w3.org/2005/sparql-results#"><head><variable name="x"/></head><results><result><binding name="x"><uri>http://example.com/a</uri></binding></result></results></sparql>"#;
        assert_eq!(
            parse("application/sparql-results+xml", xml, 10)
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}
