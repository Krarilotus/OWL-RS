use std::sync::{Arc, Mutex, RwLock};

use nrese_core::{ReasonerEngine, ReasonerRunStatus};
use nrese_reasoner::{InferenceDelta, ReasonerService};

use super::attribution::{RejectAttribution, attribute_reject_delta};
use super::command::{MutationCommand, MutationCommitReport};
use super::error::{MutationError, MutationReject};
use super::record::ReasoningRunRecord;
use super::ticket::MutationTicket;
use crate::service::StoreService;

/// The single owner of write semantics: one writer at a time, plan → validate → commit.
///
/// Steps:
/// 1. take the write slot (serialises writers; readers are never blocked by it)
/// 2. normalise the command and preview its effect without publishing it
/// 3. run the validation gates (currently the reasoner consistency gate)
/// 4. claim the commit through the [`MutationTicket`]; abort if the caller cancelled
/// 5. commit
///
/// Engine v1 note: the preview in step 2 still copies the dataset (audit finding F1). The
/// engine v2 work package P1 replaces it with delta-proportional planning behind this API.
#[derive(Debug)]
pub struct MutationPipeline {
    store: Arc<StoreService>,
    reasoner: Arc<ReasonerService>,
    write_slot: Mutex<()>,
    last_reasoning_run: RwLock<Option<ReasoningRunRecord>>,
}

impl MutationPipeline {
    pub fn new(store: Arc<StoreService>, reasoner: Arc<ReasonerService>) -> Self {
        Self {
            store,
            reasoner,
            write_slot: Mutex::new(()),
            last_reasoning_run: RwLock::new(None),
        }
    }

    pub fn store(&self) -> &Arc<StoreService> {
        &self.store
    }

    pub fn reasoner(&self) -> &Arc<ReasonerService> {
        &self.reasoner
    }

    /// The most recent reasoning gate run, rejected or not.
    pub fn last_reasoning_run(&self) -> Option<ReasoningRunRecord> {
        self.last_reasoning_run
            .read()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Applies `command` unless a gate rejects it or `ticket` is cancelled before the commit
    /// starts. Blocking; call it from a blocking context.
    pub fn apply(
        &self,
        command: MutationCommand,
        ticket: &MutationTicket,
    ) -> Result<MutationCommitReport, MutationError> {
        let kind = command.kind();
        let store_error = |source| MutationError::Store { kind, source };

        let _slot = self
            .write_slot
            .lock()
            .map_err(|_| MutationError::Poisoned)?;
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }

        let command = command.normalize().map_err(store_error)?;
        let preview = command.preview(&self.store).map_err(store_error)?;
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }

        let snapshot = &preview.snapshot;
        let plan = self
            .reasoner
            .plan(snapshot)
            .map_err(|error| MutationError::Gate(error.to_string()))?;
        let output = self
            .reasoner
            .run(snapshot, &plan)
            .map_err(|error| MutationError::Gate(error.to_string()))?;
        let attribution = output
            .inferred
            .primary_reject
            .as_ref()
            .and_then(|reject| attribute_reject_delta(reject, &preview.delta));
        self.record_run(ReasoningRunRecord::from_report(
            &output.report,
            &output.inferred,
            attribution.clone(),
        ));
        enforce_reasoner_gate(&output.inferred, output.report.status, attribution)?;

        if !ticket.begin_commit() {
            return Err(MutationError::Cancelled);
        }
        command.commit(&self.store).map_err(store_error)
    }

    fn record_run(&self, run: ReasoningRunRecord) {
        if let Ok(mut slot) = self.last_reasoning_run.write() {
            *slot = Some(run);
        }
    }
}

fn enforce_reasoner_gate(
    inferred: &InferenceDelta,
    status: ReasonerRunStatus,
    attribution: Option<RejectAttribution>,
) -> Result<(), MutationError> {
    let fallback = if matches!(status, ReasonerRunStatus::Rejected) {
        "mutation rejected by reasoner consistency gate".to_owned()
    } else if inferred.consistency_violations > 0 {
        format!(
            "mutation violates {} consistency checks",
            inferred.consistency_violations
        )
    } else {
        return Ok(());
    };

    let mut detail = inferred
        .primary_reject
        .as_ref()
        .map(|reject| reject.summary.clone())
        .or_else(|| inferred.diagnostics.first().cloned())
        .unwrap_or(fallback);
    if let Some((subject, predicate, object)) = attribution
        .as_ref()
        .and_then(RejectAttribution::likely_commit_trigger)
    {
        detail.push_str(&format!(
            " Likely commit-local trigger: <{subject}> <{predicate}> <{object}>."
        ));
    }

    Err(MutationError::Rejected(Box::new(MutationReject {
        detail,
        explanation: inferred.primary_reject.clone(),
        attribution,
    })))
}

#[cfg(test)]
mod tests {
    use nrese_core::ReasonerRunStatus;
    use nrese_reasoner::InferenceDelta;

    use super::enforce_reasoner_gate;

    #[test]
    fn reasoner_gate_allows_clean_reports() {
        let inferred = InferenceDelta::default();
        assert!(enforce_reasoner_gate(&inferred, ReasonerRunStatus::Completed, None).is_ok());
    }

    #[test]
    fn reasoner_gate_rejects_consistency_violations() {
        let inferred = InferenceDelta {
            consistency_violations: 1,
            diagnostics: vec!["first conflict".to_owned()],
            ..InferenceDelta::default()
        };
        let error = enforce_reasoner_gate(&inferred, ReasonerRunStatus::Completed, None)
            .expect_err("violations must reject");
        assert_eq!(error.to_string(), "first conflict");
    }
}
