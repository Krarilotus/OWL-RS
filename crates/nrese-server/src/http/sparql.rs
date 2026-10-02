use axum::http::StatusCode;
use axum::response::Response;
use nrese_store::{
    CancellationToken, GraphResultFormat, MutationCommand, QueryResultKind, SolutionsResultFormat,
    SparqlQueryRequest, SparqlUpdateRequest, StoreError,
};

use crate::error::ApiError;
use crate::http::media::{BOOLEAN, GRAPHS, SOLUTIONS, negotiated};
use crate::http::mutation;
use crate::http::requests::{QueryOperation, UpdateOperation};
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
    execute_query_in(state, operation, accept, None).await
}

/// [`execute_query`] on the data as `pending` would leave it (a read inside an RDF4J
/// transaction); with `None`, on the committed data.
pub async fn execute_query_in(
    state: AppState,
    operation: QueryOperation,
    accept: Option<&str>,
    pending: Option<nrese_store::StatementsRequest>,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let policy = state.policy().clone();
    policy.enforce_query_bytes(operation.query.len())?;
    let deadline = tokio::time::Instant::now() + policy.timeouts.query;

    let explain = operation.explain;
    let origin = operation.origin.clone();
    let mut request = build_query_request(operation);
    let memory = policy.limits.max_query_memory_bytes;
    request.memory_limit = (memory > 0).then_some(memory);
    // Prefixes it doesn't declare mean what the repository's namespaces say.
    let mut prepared = state
        .store()
        .prepare_query(&request)
        .map_err(|error| map_query_error(&policy, error))?;
    if let Some(origin) = origin {
        prepared.set_origin(origin);
    }
    // The query form decides which formats exist; an EXPLAIN is always JSON.
    if !explain {
        let (solutions, graph) = negotiate_formats(prepared.kind(), accept)?;
        prepared.set_formats(solutions, graph);
    }
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
            match &pending {
                None => store.run_query(&prepared, &token, out),
                Some(pending) => store.run_query_pending(pending, &prepared, &token, out),
            }
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
        // The policy deadline cancels a query, and so may an operator
        // (`DELETE /api/v1/repositories/{id}/queries/{query}`).
        StoreError::SparqlEvaluation(nrese_store::QueryEvaluationError::Cancelled) => {
            ApiError::timeout(
                "the query was stopped: it exceeded the policy timeout, or an operator cancelled it",
            )
        }
        // Other queries hold the memory this one asked for: it may succeed later.
        error if error.is_server_memory_limit() => ApiError::unavailable(format!(
            "{error} (budgets.total_query_memory, NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES)"
        )),
        // The query needs more memory than the policy allows: the client's to change.
        error if error.is_memory_limit() => ApiError::payload_too_large(format!(
            "{error} (budgets.query_memory, NRESE_MAX_QUERY_MEMORY_BYTES)"
        )),
        error if error.is_request_error() => {
            policy.bad_request_for_sparql_parse_error(error.to_string())
        }
        error => ApiError::internal(error.to_string()),
    }
}

pub async fn execute_update(
    state: AppState,
    operation: UpdateOperation,
    requester: nrese_store::Requester,
) -> Result<StatusCode, ApiError> {
    state
        .policy()
        .enforce_update_bytes(operation.update.len())?;
    let mut request = SparqlUpdateRequest::new(operation.update);
    request.using_graphs = operation.using_graphs;
    request.using_named_graphs = operation.using_named_graphs;
    mutation::run(
        &state,
        MutationCommand::Update(request),
        requester,
        state.policy().timeouts.update,
        "update execution exceeded policy timeout",
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The formats for a query of `kind`, as the client's `Accept` header weights them; 406 if
/// it accepts none that the form has.
fn negotiate_formats(
    kind: QueryResultKind,
    accept: Option<&str>,
) -> Result<(SolutionsResultFormat, GraphResultFormat), ApiError> {
    let (solutions, graph) = (SolutionsResultFormat::Json, GraphResultFormat::NTriples);
    Ok(match kind {
        QueryResultKind::Solutions => (negotiated(accept, SOLUTIONS)?, graph),
        QueryResultKind::Boolean => (negotiated(accept, BOOLEAN)?, graph),
        QueryResultKind::Graph => (solutions, negotiated(accept, GRAPHS)?),
    })
}

fn build_query_request(operation: QueryOperation) -> SparqlQueryRequest {
    let mut request = SparqlQueryRequest::new(
        operation.query,
        nrese_store::ReadScope::of(operation.access),
    );
    request.default_graphs = operation.default_graphs;
    request.named_graphs = operation.named_graphs;
    // GraphDB's `infer=false`: explicit statements only.
    if operation.infer == Some(false) {
        request.read_model = Some(nrese_store::ReadModel::Asserted);
    }
    request
}

#[cfg(test)]
mod tests {
    use nrese_store::{GraphResultFormat, QueryResultKind, SolutionsResultFormat};

    use super::negotiate_formats;

    #[test]
    fn the_query_form_decides_which_formats_are_negotiated() {
        let formats = |kind, accept| negotiate_formats(kind, accept).ok();
        assert_eq!(
            formats(
                QueryResultKind::Solutions,
                Some("text/csv,application/sparql-results+json;q=0.5")
            )
            .map(|(solutions, _)| solutions),
            Some(SolutionsResultFormat::Csv)
        );
        // An ASK has no CSV form: the next acceptable type is used, or none.
        assert_eq!(
            formats(
                QueryResultKind::Boolean,
                Some("text/csv,application/sparql-results+xml;q=0.5")
            )
            .map(|(solutions, _)| solutions),
            Some(SolutionsResultFormat::Xml)
        );
        assert_eq!(formats(QueryResultKind::Boolean, Some("text/csv")), None);
        assert_eq!(
            formats(
                QueryResultKind::Graph,
                Some("application/rdf+xml, application/n-triples;q=0.9")
            )
            .map(|(_, graph)| graph),
            Some(GraphResultFormat::RdfXml)
        );
        // A client that only takes result tables can't receive a graph.
        assert_eq!(
            formats(
                QueryResultKind::Graph,
                Some("application/sparql-results+json")
            ),
            None
        );
        assert_eq!(
            formats(QueryResultKind::Solutions, None),
            Some((SolutionsResultFormat::Json, GraphResultFormat::NTriples))
        );
    }
}
