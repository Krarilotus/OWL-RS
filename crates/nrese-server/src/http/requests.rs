use axum::body::Bytes;
use axum::http::{HeaderValue, header};
use serde::Deserialize;

use crate::error::ApiError;
use crate::http::media::{header_value_str, media_type_matches};

#[derive(Debug, Deserialize)]
struct UpdateFormRequest {
    update: String,
}

/// A SPARQL 1.1 Protocol query operation: the query and its dataset parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryOperation {
    pub query: String,
    pub default_graphs: Vec<String>,
    pub named_graphs: Vec<String>,
    /// GraphDB's `infer` parameter: `false` reads asserted statements only.
    pub infer: Option<bool>,
    /// `explain=true`: run the query and return how it ran (JSON) instead of its results.
    pub explain: bool,
}

impl QueryOperation {
    /// Collects `query`, `default-graph-uri`, `named-graph-uri` and `infer` from
    /// URL-encoded pairs. The dataset parameters may repeat; other parameters are ignored.
    fn parse_pairs(&mut self, encoded: &[u8]) -> Result<(), ApiError> {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_bytes(encoded)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        for (key, value) in pairs {
            match key.as_str() {
                "query" if !self.query.is_empty() => {
                    return Err(ApiError::bad_request(
                        "exactly one query parameter is allowed",
                    ));
                }
                "query" => self.query = value,
                "default-graph-uri" => self.default_graphs.push(value),
                "named-graph-uri" => self.named_graphs.push(value),
                "infer" => self.infer = Some(boolean("infer", &value)?),
                "explain" => self.explain = boolean("explain", &value)?,
                _ => {}
            }
        }
        Ok(())
    }
}

fn boolean(name: &str, value: &str) -> Result<bool, ApiError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ApiError::bad_request(format!(
            "{name} must be true or false, not '{other}'"
        ))),
    }
}

/// `GET /sparql?query=…`.
pub fn query_from_url(raw_query: Option<&str>) -> Result<QueryOperation, ApiError> {
    let mut operation = QueryOperation::default();
    operation.parse_pairs(raw_query.unwrap_or_default().as_bytes())?;
    operation.query = ensure_non_empty(operation.query, "query must not be empty")?;
    Ok(operation)
}

/// `POST /sparql`: a URL-encoded form (all parameters in the body), or the query as the
/// body with the dataset parameters in the URL.
pub fn query_from_post(
    raw_query: Option<&str>,
    content_type: Option<&HeaderValue>,
    body: &Bytes,
) -> Result<QueryOperation, ApiError> {
    let mut operation = QueryOperation::default();
    if media_type_matches(
        header_value_str(content_type),
        "application/x-www-form-urlencoded",
    ) {
        operation.parse_pairs(body)?;
        operation.query = ensure_non_empty(operation.query, "query must not be empty")?;
        return Ok(operation);
    }
    operation.parse_pairs(raw_query.unwrap_or_default().as_bytes())?;
    if !operation.query.is_empty() {
        return Err(ApiError::bad_request(
            "the query must be in the body or the URL, not both",
        ));
    }
    let query = String::from_utf8(body.to_vec())
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    operation.query = ensure_non_empty(query, "request body must contain a SPARQL query")?;
    Ok(operation)
}

pub fn extract_update(
    content_type: Option<&HeaderValue>,
    body: &Bytes,
) -> Result<String, ApiError> {
    let content_type_str = header_value_str(content_type);

    if media_type_matches(content_type_str, "application/x-www-form-urlencoded") {
        let request: UpdateFormRequest = serde_urlencoded::from_bytes(body)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        return ensure_non_empty(request.update, "update must not be empty");
    }

    if content_type_str.is_none()
        || media_type_matches(content_type_str, "application/sparql-update")
        || media_type_matches(content_type_str, "text/plain")
    {
        let update = String::from_utf8(body.to_vec())
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        return ensure_non_empty(update, "request body must contain a SPARQL update");
    }

    Err(ApiError::bad_request(format!(
        "unsupported content type for update request: {}",
        content_type_str.unwrap_or_default()
    )))
}

pub fn accept_header_value(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
}

fn ensure_non_empty(value: String, error_message: &'static str) -> Result<String, ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::bad_request(error_message));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;
    use axum::http::HeaderValue;

    use super::{QueryOperation, extract_update, query_from_post, query_from_url};

    const SELECT: &str = "SELECT%20*%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D";

    #[test]
    fn query_from_form_body() {
        let body = Bytes::from(format!(
            "query={SELECT}&named-graph-uri=http%3A%2F%2Fex%2Fa&named-graph-uri=http%3A%2F%2Fex%2Fb"
        ));
        let content_type = HeaderValue::from_static("application/x-www-form-urlencoded");
        let operation = query_from_post(None, Some(&content_type), &body).expect("query");
        assert_eq!(
            operation,
            QueryOperation {
                query: "SELECT * WHERE { ?s ?p ?o }".to_owned(),
                default_graphs: Vec::new(),
                named_graphs: vec!["http://ex/a".to_owned(), "http://ex/b".to_owned()],
                infer: None,
                explain: false,
            }
        );
    }

    #[test]
    fn query_from_url_with_dataset_parameters() {
        let operation = query_from_url(Some(&format!(
            "query={SELECT}&default-graph-uri=http%3A%2F%2Fex%2Fg&timeout=5"
        )))
        .expect("query");
        assert_eq!(operation.default_graphs, ["http://ex/g"]);
        assert!(
            query_from_url(Some("default-graph-uri=x")).is_err(),
            "query missing"
        );
        assert!(query_from_url(Some(&format!("query={SELECT}&query={SELECT}"))).is_err());
        let operation =
            query_from_url(Some(&format!("query={SELECT}&infer=false"))).expect("query");
        assert_eq!(operation.infer, Some(false));
        assert!(query_from_url(Some(&format!("query={SELECT}&infer=no"))).is_err());
        let operation =
            query_from_url(Some(&format!("query={SELECT}&explain=true"))).expect("query");
        assert!(operation.explain);
    }

    #[test]
    fn direct_post_takes_dataset_parameters_from_the_url() {
        let content_type = HeaderValue::from_static("application/sparql-query");
        let body = Bytes::from("ASK {}");
        let operation = query_from_post(
            Some("named-graph-uri=http%3A%2F%2Fex%2Fn"),
            Some(&content_type),
            &body,
        )
        .expect("query");
        assert_eq!(
            (operation.query.as_str(), operation.named_graphs.len()),
            ("ASK {}", 1)
        );
        assert!(
            query_from_post(Some(&format!("query={SELECT}")), Some(&content_type), &body).is_err(),
            "query in both places"
        );
    }

    #[test]
    fn update_from_sparql_update_body() {
        let body = Bytes::from(
            "INSERT DATA { <http://example.com/s> <http://example.com/p> <http://example.com/o> }",
        );
        let content_type = HeaderValue::from_static("application/sparql-update");
        let update = extract_update(Some(&content_type), &body).expect("update should parse");
        assert!(update.starts_with("INSERT DATA"));
    }

    #[test]
    fn update_from_parameterized_content_type_body() {
        let body = Bytes::from("DELETE WHERE { ?s ?p ?o }");
        let content_type = HeaderValue::from_static("application/sparql-update; charset=utf-8");
        let update = extract_update(Some(&content_type), &body).expect("update should parse");
        assert!(update.starts_with("DELETE WHERE"));
    }
}
