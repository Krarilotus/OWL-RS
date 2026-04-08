use nrese_core::{ReasonerEngine, ReasonerRunStatus};
use nrese_reasoner::{InferenceDelta, ReasonerService};
use nrese_store::{SparqlUpdateRequest, StoreService};

use crate::error::ApiError;
use crate::reasoning_runtime::LastReasoningRun;
use crate::reject_attribution::attribute_reject_delta;
use crate::state::AppState;

pub async fn execute(state: AppState, update: String) -> Result<(), ApiError> {
    if !state.is_ready() {
        return Err(ApiError::unavailable("server is not ready yet"));
    }

    let store = state.store();
    let reasoner = state.reasoner();
    let request = SparqlUpdateRequest::new(update);
    let runtime_state = state.clone();

    tokio::task::spawn_blocking(move || {
        execute_blocking(&store, &reasoner, request, &runtime_state)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))??;

    Ok(())
}

fn execute_blocking(
    store: &StoreService,
    reasoner: &ReasonerService,
    request: SparqlUpdateRequest,
    state: &AppState,
) -> Result<(), ApiError> {
    let update_lock = state.update_lock();
    let policy = state.policy().clone();
    let _guard = update_lock
        .lock()
        .map_err(|_| ApiError::internal("update lock is poisoned"))?;
    let preview = store
        .preview_update(&request)
        .map_err(|error| policy.bad_request_for_sparql_parse_error(error.to_string()))?;
    let snapshot = &preview.snapshot;
    let plan = reasoner
        .plan(snapshot)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let output = reasoner
        .run(snapshot, &plan)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let commit_attribution = output
        .inferred
        .primary_reject
        .as_ref()
        .and_then(|reject| attribute_reject_delta(reject, &preview.delta));
    state.set_last_reasoning_run(LastReasoningRun::from_report(
        &output.report,
        &output.inferred,
        commit_attribution.clone(),
    ));

    enforce_reasoner_gate(
        &output.inferred,
        output.report.status,
        commit_attribution.as_ref(),
    )?;
    let _update_report = store
        .execute_update(&request)
        .map_err(|error| policy.bad_request_for_sparql_parse_error(error.to_string()))?;

    Ok(())
}

fn enforce_reasoner_gate(
    inferred: &InferenceDelta,
    status: ReasonerRunStatus,
    commit_attribution: Option<&crate::reject_attribution::RejectAttribution>,
) -> Result<(), ApiError> {
    if matches!(status, ReasonerRunStatus::Rejected) {
        return Err(reasoner_gate_error(
            inferred,
            commit_attribution,
            "update rejected by reasoner consistency gate".to_owned(),
        ));
    }

    if inferred.consistency_violations > 0 {
        return Err(reasoner_gate_error(
            inferred,
            commit_attribution,
            format!(
                "update violates {} consistency checks",
                inferred.consistency_violations
            ),
        ));
    }

    Ok(())
}

fn reasoner_gate_error(
    inferred: &InferenceDelta,
    commit_attribution: Option<&crate::reject_attribution::RejectAttribution>,
    fallback: String,
) -> ApiError {
    let mut detail = inferred
        .primary_reject
        .as_ref()
        .map(|reject| reject.summary.clone())
        .or_else(|| inferred.diagnostics.first().cloned())
        .unwrap_or(fallback);

    if let Some((subject, predicate, object)) =
        commit_attribution.and_then(|attribution| attribution.likely_commit_trigger())
    {
        detail.push_str(&format!(
            " Likely commit-local trigger: <{subject}> <{predicate}> <{object}>."
        ));
    }

    ApiError::reasoner_reject(
        detail,
        inferred.primary_reject.clone(),
        commit_attribution.cloned(),
    )
}

#[cfg(test)]
mod tests {
    use nrese_core::ReasonerRunStatus;
    use nrese_reasoner::InferenceDelta;

    use super::enforce_reasoner_gate;

    #[test]
    fn reasoner_gate_allows_clean_reports() {
        let inferred = InferenceDelta::default();
        let result = enforce_reasoner_gate(&inferred, ReasonerRunStatus::Completed, None);
        assert!(result.is_ok());
    }

    #[test]
    fn reasoner_gate_rejects_consistency_violations() {
        let inferred = InferenceDelta {
            inferred_triples: 0,
            consistency_violations: 1,
            derived_triples: Vec::new(),
            diagnostics: vec!["first conflict".to_owned()],
            ..InferenceDelta::default()
        };
        let result = enforce_reasoner_gate(&inferred, ReasonerRunStatus::Completed, None);
        assert!(result.is_err());
    }
}
