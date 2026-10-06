use axum::body::Bytes;
use axum::http::{HeaderValue, header};

use crate::error::ApiError;
use crate::http::media::{header_value_str, media_type_matches};

/// A SPARQL 1.1 Protocol query operation: the query and its dataset parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryOperation {
    pub query: String,
    pub default_graphs: Vec<String>,
    pub named_graphs: Vec<String>,
    /// GraphDB's `infer` parameter: `false` reads asserted statements only.
    pub infer: Option<bool>,
    /// `explain=true`: run the query and return how it ran (JSON) instead of its results;
    /// `explain=plan`: return the plan it would run as, with estimates, without running it.
    pub explain: Explain,
    /// The graphs the requester may read (graph-level access control); `None`: every graph.
    pub access: Option<std::sync::Arc<nrese_sparql::GraphAccess>>,
    /// Who sent it (shown in the running queries).
    pub origin: Option<String>,
    /// `dl-answers=certain-where-complete|sound|exact`: under `owl2-dl`, which answers.
    pub dl_answers: Option<nrese_store::DlAnswers>,
}

impl QueryOperation {
    /// Restricts the query to what `view` lets its requester read, and names the requester.
    pub fn restrict(&mut self, view: &crate::access::AccessView) {
        self.access = view.read.clone();
        self.origin = view.origin.clone();
    }

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
                "dl-answers" => {
                    self.dl_answers =
                        Some(nrese_store::DlAnswers::from_name(&value).ok_or_else(|| {
                            ApiError::bad_request(format!(
                                "dl-answers must be 'certain-where-complete', 'sound' or \
                                 'exact', not '{value}'"
                            ))
                        })?);
                }
                "explain" => {
                    self.explain = match value.as_str() {
                        "plan" => Explain::Plan,
                        other => match boolean("explain", other)? {
                            true => Explain::Analyze,
                            false => Explain::No,
                        },
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// What a query request asks for instead of results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Explain {
    /// The results.
    #[default]
    No,
    /// Run it and report how it ran (`explain=true`).
    Analyze,
    /// The plan with estimates, without running it (`explain=plan`).
    Plan,
}

/// A SPARQL 1.1 Protocol update operation: the update and its dataset parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateOperation {
    pub update: String,
    pub using_graphs: Vec<String>,
    pub using_named_graphs: Vec<String>,
}

impl UpdateOperation {
    /// Collects `update`, `using-graph-uri` and `using-named-graph-uri` from URL-encoded
    /// pairs. The dataset parameters may repeat; other parameters are ignored.
    fn parse_pairs(&mut self, encoded: &[u8]) -> Result<(), ApiError> {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_bytes(encoded)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        for (key, value) in pairs {
            match key.as_str() {
                "update" if !self.update.is_empty() => {
                    return Err(ApiError::bad_request(
                        "exactly one update parameter is allowed",
                    ));
                }
                "update" => self.update = value,
                "using-graph-uri" => self.using_graphs.push(value),
                "using-named-graph-uri" => self.using_named_graphs.push(value),
                _ => {}
            }
        }
        Ok(())
    }
}

/// What a request to the combined SPARQL endpoint asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SparqlOperation {
    Query(QueryOperation),
    Update(UpdateOperation),
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
    if media_type_matches(header_value_str(content_type), FORM) {
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

/// `POST /update`: a URL-encoded form (all parameters in the body), or the update as the
/// body with the dataset parameters in the URL.
pub fn update_from_post(
    raw_query: Option<&str>,
    content_type: Option<&HeaderValue>,
    body: &Bytes,
) -> Result<UpdateOperation, ApiError> {
    let content_type_str = header_value_str(content_type);
    let mut operation = UpdateOperation::default();

    if media_type_matches(content_type_str, FORM) {
        operation.parse_pairs(body)?;
        operation.update = ensure_non_empty(operation.update, "update must not be empty")?;
        return Ok(operation);
    }

    if content_type_str.is_none()
        || media_type_matches(content_type_str, SPARQL_UPDATE)
        || media_type_matches(content_type_str, "text/plain")
    {
        operation.parse_pairs(raw_query.unwrap_or_default().as_bytes())?;
        if !operation.update.is_empty() {
            return Err(ApiError::bad_request(
                "the update must be in the body or the URL, not both",
            ));
        }
        let update = String::from_utf8(body.to_vec())
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        operation.update = ensure_non_empty(update, "request body must contain a SPARQL update")?;
        return Ok(operation);
    }

    Err(ApiError::unsupported_media_type(format!(
        "unsupported content type for update request: {}",
        content_type_str.unwrap_or_default()
    )))
}

const FORM: &str = "application/x-www-form-urlencoded";
const SPARQL_QUERY: &str = "application/sparql-query";
const SPARQL_UPDATE: &str = "application/sparql-update";

/// `POST` to the combined endpoint: a query or an update, told apart as the protocol
/// tells them apart: by the body's media type, or by the form's `query` / `update` field.
pub fn operation_from_post(
    raw_query: Option<&str>,
    content_type: Option<&HeaderValue>,
    body: &Bytes,
) -> Result<SparqlOperation, ApiError> {
    let content_type_str = header_value_str(content_type);
    if media_type_matches(content_type_str, FORM) {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_bytes(body)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        let has = |name: &str| pairs.iter().any(|(key, _)| key == name);
        return match (has("query"), has("update")) {
            (true, false) => {
                query_from_post(raw_query, content_type, body).map(SparqlOperation::Query)
            }
            (false, true) => {
                update_from_post(raw_query, content_type, body).map(SparqlOperation::Update)
            }
            (true, true) => Err(ApiError::bad_request(
                "a request is a query or an update, not both",
            )),
            (false, false) => Err(ApiError::bad_request(
                "the form needs a query or an update parameter",
            )),
        };
    }
    if media_type_matches(content_type_str, SPARQL_QUERY) {
        return query_from_post(raw_query, content_type, body).map(SparqlOperation::Query);
    }
    if media_type_matches(content_type_str, SPARQL_UPDATE) {
        return update_from_post(raw_query, content_type, body).map(SparqlOperation::Update);
    }
    Err(ApiError::unsupported_media_type(format!(
        "send {SPARQL_QUERY}, {SPARQL_UPDATE} or {FORM}, not '{}'",
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

    use super::{
        Explain, QueryOperation, SparqlOperation, operation_from_post, query_from_post,
        query_from_url, update_from_post,
    };

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
                explain: Explain::No,
                access: None,
                origin: None,
                dl_answers: None,
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
        assert_eq!(operation.explain, Explain::Analyze);
        let operation =
            query_from_url(Some(&format!("query={SELECT}&explain=plan"))).expect("query");
        assert_eq!(operation.explain, Explain::Plan);
        assert!(query_from_url(Some(&format!("query={SELECT}&explain=maybe"))).is_err());
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
        let operation = update_from_post(
            Some("using-graph-uri=http%3A%2F%2Fex%2Fg"),
            Some(&content_type),
            &body,
        )
        .expect("update should parse");
        assert!(operation.update.starts_with("INSERT DATA"));
        assert_eq!(operation.using_graphs, ["http://ex/g"]);
    }

    #[test]
    fn update_from_form_body_with_dataset_parameters() {
        let body = Bytes::from(
            "update=CLEAR%20ALL&using-graph-uri=http%3A%2F%2Fex%2Fa\
             &using-named-graph-uri=http%3A%2F%2Fex%2Fn&using-named-graph-uri=http%3A%2F%2Fex%2Fm",
        );
        let content_type = HeaderValue::from_static("application/x-www-form-urlencoded");
        let operation = update_from_post(None, Some(&content_type), &body).expect("update");
        assert_eq!(operation.update, "CLEAR ALL");
        assert_eq!(operation.using_graphs, ["http://ex/a"]);
        assert_eq!(operation.using_named_graphs, ["http://ex/n", "http://ex/m"]);
        let twice = Bytes::from("update=CLEAR%20ALL&update=CLEAR%20ALL");
        assert!(update_from_post(None, Some(&content_type), &twice).is_err());
        let json = HeaderValue::from_static("application/json");
        assert!(update_from_post(None, Some(&json), &body).is_err());
    }

    /// The combined endpoint tells queries from updates by media type or form field.
    #[test]
    fn the_combined_endpoint_tells_queries_from_updates() {
        let form = HeaderValue::from_static("application/x-www-form-urlencoded; charset=UTF-8");
        let kind = |content_type: &HeaderValue, body: &str| {
            operation_from_post(None, Some(content_type), &Bytes::from(body.to_owned())).map(
                |operation| match operation {
                    SparqlOperation::Query(_) => "query",
                    SparqlOperation::Update(_) => "update",
                },
            )
        };
        assert_eq!(
            kind(&form, &format!("query={SELECT}&infer=true")).ok(),
            Some("query")
        );
        assert_eq!(kind(&form, "update=CLEAR%20ALL").ok(), Some("update"));
        assert!(kind(&form, &format!("query={SELECT}&update=CLEAR%20ALL")).is_err());
        assert!(kind(&form, "timeout=5").is_err());
        let query = HeaderValue::from_static("application/sparql-query");
        assert_eq!(kind(&query, "ASK {}").ok(), Some("query"));
        let update = HeaderValue::from_static("application/sparql-update");
        assert_eq!(kind(&update, "CLEAR ALL").ok(), Some("update"));
        let other = HeaderValue::from_static("text/plain");
        assert!(
            kind(&other, "ASK {}").is_err(),
            "ambiguous without a SPARQL media type"
        );
    }

    #[test]
    fn update_from_parameterized_content_type_body() {
        let body = Bytes::from("DELETE WHERE { ?s ?p ?o }");
        let content_type = HeaderValue::from_static("application/sparql-update; charset=utf-8");
        let operation =
            update_from_post(None, Some(&content_type), &body).expect("update should parse");
        assert!(operation.update.starts_with("DELETE WHERE"));
    }
}
