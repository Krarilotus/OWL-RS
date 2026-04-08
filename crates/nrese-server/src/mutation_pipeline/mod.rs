mod command;

use nrese_core::{ReasonerEngine, ReasonerRunStatus};
use nrese_reasoner::{InferenceDelta, ReasonerService};
use nrese_store::{
    DatasetRestoreReport, DatasetRestoreRequest, GraphDeleteReport, GraphTarget, GraphWriteReport,
    GraphWriteRequest, SparqlUpdateRequest, StoreService, TellRequest, compile_tell_update,
};

use crate::error::ApiError;
use crate::mutation_pipeline::command::{MutationCommand, MutationCommitReport};
use crate::reasoning_runtime::LastReasoningRun;
use crate::reject_attribution::{RejectAttribution, attribute_reject_delta};
use crate::state::AppState;

pub async fn execute_update(state: AppState, update: String) -> Result<(), ApiError> {
    execute(
        state,
        MutationCommand::Update(SparqlUpdateRequest::new(update)),
    )
    .await
    .map(|_| ())
}

pub async fn execute_tell(state: AppState, request: TellRequest) -> Result<(), ApiError> {
    let update = tokio::task::spawn_blocking(move || compile_tell_update(&request))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(|error| ApiError::bad_request(error.to_string()))?;

    execute(state, MutationCommand::Update(update))
        .await
        .map(|_| ())
}

pub async fn execute_graph_write(
    state: AppState,
    request: GraphWriteRequest,
) -> Result<GraphWriteReport, ApiError> {
    match execute(state, MutationCommand::GraphWrite(request)).await? {
        MutationCommitReport::GraphWrite(report) => Ok(report),
        other => Err(ApiError::internal(format!(
            "unexpected graph write mutation result: {other:?}"
        ))),
    }
}

pub async fn execute_graph_delete(
    state: AppState,
    target: GraphTarget,
) -> Result<GraphDeleteReport, ApiError> {
    match execute(state, MutationCommand::GraphDelete(target)).await? {
        MutationCommitReport::GraphDelete(report) => Ok(report),
        other => Err(ApiError::internal(format!(
            "unexpected graph delete mutation result: {other:?}"
        ))),
    }
}

pub async fn execute_restore(
    state: AppState,
    request: DatasetRestoreRequest,
) -> Result<DatasetRestoreReport, ApiError> {
    match execute(state, MutationCommand::Restore(request)).await? {
        MutationCommitReport::Restore(report) => Ok(report),
        other => Err(ApiError::internal(format!(
            "unexpected restore mutation result: {other:?}"
        ))),
    }
}

async fn execute(
    state: AppState,
    command: MutationCommand,
) -> Result<MutationCommitReport, ApiError> {
    if !state.is_ready() {
        return Err(ApiError::unavailable("server is not ready yet"));
    }

    let store = state.store();
    let reasoner = state.reasoner();
    let runtime_state = state.clone();

    tokio::task::spawn_blocking(move || {
        execute_blocking(&store, &reasoner, command, &runtime_state)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
}

fn execute_blocking(
    store: &StoreService,
    reasoner: &ReasonerService,
    command: MutationCommand,
    state: &AppState,
) -> Result<MutationCommitReport, ApiError> {
    let update_lock = state.update_lock();
    let policy = state.policy().clone();
    let _guard = update_lock
        .lock()
        .map_err(|_| ApiError::internal("update lock is poisoned"))?;
    let preview = command
        .preview(store)
        .map_err(|error| command.map_store_error(&policy, error))?;
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
        .and_then(|reject| attribute_reject_delta(reject, command.delta(&preview)));
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

    command
        .commit(store)
        .map_err(|error| command.map_store_error(&policy, error))
}

fn enforce_reasoner_gate(
    inferred: &InferenceDelta,
    status: ReasonerRunStatus,
    commit_attribution: Option<&RejectAttribution>,
) -> Result<(), ApiError> {
    if matches!(status, ReasonerRunStatus::Rejected) {
        return Err(reasoner_gate_error(
            inferred,
            commit_attribution,
            "mutation rejected by reasoner consistency gate".to_owned(),
        ));
    }

    if inferred.consistency_violations > 0 {
        return Err(reasoner_gate_error(
            inferred,
            commit_attribution,
            format!(
                "mutation violates {} consistency checks",
                inferred.consistency_violations
            ),
        ));
    }

    Ok(())
}

fn reasoner_gate_error(
    inferred: &InferenceDelta,
    commit_attribution: Option<&RejectAttribution>,
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
