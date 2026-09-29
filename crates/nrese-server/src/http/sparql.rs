use axum::http::StatusCode;
use axum::response::Response;
use nrese_store::{
    CancellationToken, GraphResultFormat, MutationCommand, PreparedQuery, SolutionsResultFormat,
    SparqlQueryRequest, SparqlUpdateRequest, StoreError,
};

use crate::error::ApiError;
use crate::http::media::media_type_matches;
use crate::http::mutation;
use crate::http::requests::QueryOperation;
use crate::http::result_stream::stream_blocking;
use crate::policy::PolicyConfig;
use crate::state::AppState;

const QUERY_TIMEOUT_MESSAGE: &str = "query execution exceeded policy timeout";

/// Evaluates a query and streams its results. The policy timeout is a real deadline: when
/// it passes, evaluation is cancelled, whether or not results have started streaming.
pub async fn execute_query(
    state: AppState,
    operation: QueryOperation,
    accept: Option<&str>,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let policy = state.policy().clone();
    policy.enforce_query_bytes(operation.query.len())?;
    let deadline = tokio::time::Instant::now() + policy.timeouts.query;

    let explain = operation.explain;
    let mut request = build_query_request(operation, accept);
    let memory = policy.limits.max_query_memory_bytes;
    request.memory_limit = (memory > 0).then_some(memory);
    let prepared =
        PreparedQuery::parse(&request).map_err(|error| map_query_error(&policy, error))?;
    let media_type = prepared.media_type();
    let store = state.store();
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    if explain {
        return stream_blocking(
            deadline,
            cancellation,
            "application/json",
            QUERY_TIMEOUT_MESSAGE,
            move |out| {
                let explanation = store
                    .explain_query(&prepared, &token)
                    .map_err(|error| map_query_error(&policy, error))?;
                serde_json::to_writer(out, &explanation_json(&explanation))
                    .map_err(|error| ApiError::internal(error.to_string()))
            },
        )
        .await;
    }
    stream_blocking(
        deadline,
        cancellation,
        media_type,
        QUERY_TIMEOUT_MESSAGE,
        move |out| {
            store
                .run_query(&prepared, &token, out)
                .map_err(|error| map_query_error(&policy, error))
        },
    )
    .await
}

/// The JSON form of an EXPLAIN: the executor, totals, and one object per operator in
/// evaluation order (`depth` gives the nesting).
fn explanation_json(explanation: &nrese_store::Explanation) -> serde_json::Value {
    let steps: Vec<serde_json::Value> = explanation
        .steps
        .iter()
        .map(|step| {
            serde_json::json!({
                "depth": step.depth,
                "operator": step.operator,
                "detail": step.detail,
                "estimated_rows": step.estimated_rows,
                "rows": step.rows,
                "micros": step.micros,
            })
        })
        .collect();
    serde_json::json!({
        "executor": explanation.executor,
        "rows": explanation.rows,
        "micros": explanation.micros,
        "steps": steps,
    })
}

/// Query errors caused by the request are 400s; a cancelled evaluation is a timeout;
/// anything else is the server's fault.
fn map_query_error(policy: &PolicyConfig, error: StoreError) -> ApiError {
    match error {
        StoreError::SparqlEvaluation(nrese_store::QueryEvaluationError::Cancelled) => {
            ApiError::timeout(QUERY_TIMEOUT_MESSAGE)
        }
        // The query needs more memory than the policy allows: the client's to change.
        error if error.is_memory_limit() => ApiError::payload_too_large(format!(
            "{error} (policy limit NRESE_MAX_QUERY_MEMORY_BYTES)"
        )),
        error if error.is_request_error() => {
            policy.bad_request_for_sparql_parse_error(error.to_string())
        }
        error => ApiError::internal(error.to_string()),
    }
}

pub async fn execute_update(state: AppState, update: String) -> Result<StatusCode, ApiError> {
    state.policy().enforce_update_bytes(update.len())?;
    mutation::run(
        &state,
        MutationCommand::Update(SparqlUpdateRequest::new(update)),
        state.policy().timeouts.update,
        "update execution exceeded policy timeout",
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn build_query_request(operation: QueryOperation, accept: Option<&str>) -> SparqlQueryRequest {
    let mut request = SparqlQueryRequest::new(operation.query);
    request.default_graphs = operation.default_graphs;
    request.named_graphs = operation.named_graphs;
    // GraphDB's `infer=false`: explicit statements only.
    if operation.infer == Some(false) {
        request.read_model = Some(nrese_store::ReadModel::Asserted);
    }

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
    use crate::http::requests::QueryOperation;

    fn operation(query: &str) -> QueryOperation {
        QueryOperation {
            query: query.to_owned(),
            ..QueryOperation::default()
        }
    }

    #[test]
    fn query_accept_csv_selects_csv_format() {
        let request = build_query_request(
            operation("SELECT * WHERE { ?s ?p ?o }"),
            Some("text/csv,application/sparql-results+json"),
        );

        assert_eq!(request.solutions_format, SolutionsResultFormat::Csv);
    }

    #[test]
    fn query_accept_default_is_json() {
        let request = build_query_request(operation("ASK WHERE { ?s ?p ?o }"), None);
        assert_eq!(request.solutions_format, SolutionsResultFormat::Json);
    }

    #[test]
    fn query_accept_prefers_rdf_xml_for_graph_results() {
        let request = build_query_request(
            operation("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"),
            Some("application/rdf+xml, application/n-triples"),
        );
        assert_eq!(request.graph_format, GraphResultFormat::RdfXml);
    }
}
