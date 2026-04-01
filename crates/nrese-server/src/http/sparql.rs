use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_store::{GraphResultFormat, SolutionsResultFormat, SparqlQueryRequest};

use crate::error::ApiError;
use crate::http::media::media_type_matches;
use crate::state::AppState;
use crate::update_pipeline;

pub async fn execute_query(
    state: AppState,
    query: String,
    accept: Option<&str>,
) -> Result<Response, ApiError> {
    if !state.is_ready() {
        return Err(ApiError::unavailable("server is not ready yet"));
    }
    state.policy().enforce_query_bytes(query.len())?;

    let request = build_query_request(query, accept);
    let store = state.store();
    let result = tokio::time::timeout(
        state.policy().timeouts.query,
        tokio::task::spawn_blocking(move || store.execute_query(&request)),
    )
    .await
    .map_err(|_| ApiError::timeout("query execution exceeded policy timeout"))?
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::bad_request(error.to_string()))?;

    let mut response = (StatusCode::OK, result.payload).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(result.media_type)
            .map_err(|error| ApiError::internal(error.to_string()))?,
    );

    Ok(response)
}

pub async fn execute_update(state: AppState, update: String) -> Result<StatusCode, ApiError> {
    state.policy().enforce_update_bytes(update.len())?;
    tokio::time::timeout(
        state.policy().timeouts.update,
        update_pipeline::execute(state, update),
    )
    .await
    .map_err(|_| ApiError::timeout("update execution exceeded policy timeout"))??;
    Ok(StatusCode::NO_CONTENT)
}

fn build_query_request(query: String, accept: Option<&str>) -> SparqlQueryRequest {
    let mut request = SparqlQueryRequest::new(query);

    if media_type_matches(accept, "application/sparql-results+xml") {
        request.solutions_format = SolutionsResultFormat::Xml;
    } else if media_type_matches(accept, "text/csv") {
        request.solutions_format = SolutionsResultFormat::Csv;
    } else if media_type_matches(accept, "text/tab-separated-values") {
        request.solutions_format = SolutionsResultFormat::Tsv;
    } else {
        request.solutions_format = SolutionsResultFormat::Json;
    }

    request.graph_format = if media_type_matches(accept, "application/rdf+xml") {
        GraphResultFormat::RdfXml
    } else if media_type_matches(accept, "text/turtle")
        || media_type_matches(accept, "application/x-turtle")
    {
        GraphResultFormat::Turtle
    } else {
        GraphResultFormat::NTriples
    };

    request
}

#[cfg(test)]
mod tests {
    use nrese_store::{GraphResultFormat, SolutionsResultFormat};

    use super::build_query_request;

    #[test]
    fn query_accept_csv_selects_csv_format() {
        let request = build_query_request(
            "SELECT * WHERE { ?s ?p ?o }".to_owned(),
            Some("text/csv,application/sparql-results+json"),
        );

        assert_eq!(request.solutions_format, SolutionsResultFormat::Csv);
    }

    #[test]
    fn query_accept_default_is_json() {
        let request = build_query_request("ASK WHERE { ?s ?p ?o }".to_owned(), None);
        assert_eq!(request.solutions_format, SolutionsResultFormat::Json);
    }

    #[test]
    fn query_accept_prefers_rdf_xml_for_graph_results() {
        let request = build_query_request(
            "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }".to_owned(),
            Some("application/rdf+xml, application/n-triples"),
        );
        assert_eq!(request.graph_format, GraphResultFormat::RdfXml);
    }
}
