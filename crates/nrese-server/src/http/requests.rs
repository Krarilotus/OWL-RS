use axum::body::Bytes;
use axum::http::{HeaderValue, header};
use serde::Deserialize;

use crate::error::ApiError;
use crate::http::media::{header_value_str, media_type_matches};

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub query: String,
}

#[derive(Debug, Deserialize)]
struct UpdateFormRequest {
    update: String,
}

pub fn extract_query(content_type: Option<&HeaderValue>, body: &Bytes) -> Result<String, ApiError> {
    if media_type_matches(
        header_value_str(content_type),
        "application/x-www-form-urlencoded",
    ) {
        let request: QueryRequest = serde_urlencoded::from_bytes(body)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        return ensure_non_empty(request.query, "query must not be empty");
    }

    let query = String::from_utf8(body.to_vec())
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    ensure_non_empty(query, "request body must contain a SPARQL query")
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

    use super::{extract_query, extract_update};

    #[test]
    fn query_from_form_body() {
        let body = Bytes::from("query=SELECT%20*%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D");
        let content_type = HeaderValue::from_static("application/x-www-form-urlencoded");
        let query = extract_query(Some(&content_type), &body).expect("query should parse");
        assert_eq!(query, "SELECT * WHERE { ?s ?p ?o }");
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
