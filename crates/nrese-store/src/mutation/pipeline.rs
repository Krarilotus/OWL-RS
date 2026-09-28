use std::sync::{Arc, RwLock};

use nrese_core::{ReasonerEngine, ReasonerRunStatus};
use nrese_reasoner::{InferenceDelta, ReasonerService, ReasoningMode};

use super::attribution::{RejectAttribution, attribute_reject_delta};
use super::command::{MutationCommand, MutationCommitReport};
use super::error::{MutationError, MutationReject};
use super::record::ReasoningRunRecord;
use super::ticket::MutationTicket;
use crate::delta::{MutationDeltaPreview, gate_snapshot};
use crate::error::StoreError;
use crate::service::StoreService;

/// The single owner of write semantics: plan → validate → claim → commit.
///
/// 1. Open an engine transaction. Its writer slot serialises writers; readers never wait.
/// 2. Apply the command to the transaction. Nothing is visible yet, and the transaction's
///    exact delta *is* the preview: no copy of the dataset is made.
/// 3. Run the validation gates on the transaction's state (currently the v1 reasoner gate).
/// 4. Claim the commit through the [`MutationTicket`]; abort if the caller cancelled.
/// 5. Commit: WAL append (durable mode), then publish.
///
/// Cost: O(delta) with reasoning disabled. With reasoner v2 (`rdfs`, `owl2-rl`) the delta
/// executor maintains the inferred stack at a cost that follows the change (plus grounding
/// against the TBox). The v1 reasoner reads the whole dataset as strings, O(dataset).
#[derive(Debug)]
pub struct MutationPipeline {
    store: Arc<StoreService>,
    reasoner: Arc<ReasonerService>,
    last_reasoning_run: RwLock<Option<ReasoningRunRecord>>,
    /// Reasoner v2's ruleset compiled against the dictionary, on first use.
    program: std::sync::OnceLock<crate::reasoning::Program>,
    /// Reasoner v2's ground program and the committed revision it describes. Writers
    /// outside the pipeline (bulk loads, rematerialisation) change the revision, which
    /// invalidates it.
    ground: std::sync::Mutex<Option<(u64, nrese_reasoner::v2::eval::GroundProgram)>>,
}

impl MutationPipeline {
    pub fn new(store: Arc<StoreService>, reasoner: Arc<ReasonerService>) -> Self {
        Self {
            store,
            reasoner,
            last_reasoning_run: RwLock::new(None),
            program: std::sync::OnceLock::new(),
            ground: std::sync::Mutex::new(None),
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
        let store_error = |source| {
            if ticket.is_cancelled() {
                MutationError::Cancelled
            } else {
                MutationError::Store { kind, source }
            }
        };
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }

        let mut tx = self.store.engine().transaction();
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }
        let report = command
            .apply(&mut tx, ticket.evaluation_token())
            .map_err(store_error)?;

        if let Some(ruleset) = self.reasoner.config().materialised_ruleset() {
            // Reasoner v2: the closure goes into the inferred stack, in this transaction.
            let program = self.program.get_or_init(|| {
                let tx = &tx;
                crate::reasoning::Program::new(ruleset, &|term| tx.intern(term))
            });
            let revision = tx.base().revision();
            // Writers are serialised by the transaction, so the cache can't change meanwhile.
            let mut ground = self.ground.lock().unwrap_or_else(|p| p.into_inner());
            let cached = ground
                .as_ref()
                .filter(|(at, _)| *at == revision)
                .map(|(_, program)| program);
            let (violations, materialisation, changed) =
                crate::reasoning::apply_delta(program, cached, &mut tx);
            tracing::debug!(?materialisation, "commit-path materialisation");
            if let Some(violation) = violations.first() {
                let decode = |id: u64| {
                    tx.decode(nrese_engine::TermId::from_raw(id))
                        .map_or_else(|| format!("#{id}"), |term| term.to_string())
                };
                let bindings: Vec<String> =
                    violation.bindings.iter().map(|&id| decode(id)).collect();
                return Err(MutationError::Rejected(Box::new(MutationReject {
                    detail: format!(
                        "mutation violates {} consistency check(s); first: {} with {}",
                        violations.len(),
                        violation.rule,
                        bindings.join(", ")
                    ),
                    explanation: None,
                    attribution: None,
                })));
            }
            if !ticket.begin_commit() {
                return Err(MutationError::Cancelled);
            }
            let summary = tx.commit().map_err(|error| MutationError::Store {
                kind,
                source: StoreError::Engine(error),
            })?;
            // The program now describes the committed state.
            match changed {
                Some(program) => *ground = Some((summary.revision, program)),
                None => {
                    if let Some((at, _)) = ground.as_mut() {
                        *at = summary.revision;
                    }
                }
            }
            return Ok(report.committed(summary.revision));
        }

        let reads_triples = self.reasoner.config().mode() != ReasoningMode::Disabled;
        let snapshot =
            gate_snapshot(&tx, tx.base().revision() + 1, reads_triples).map_err(store_error)?;
        let plan = self
            .reasoner
            .plan(&snapshot)
            .map_err(|error| MutationError::Gate(error.to_string()))?;
        let output = self
            .reasoner
            .run(&snapshot, &plan)
            .map_err(|error| MutationError::Gate(error.to_string()))?;
        let attribution = output
            .inferred
            .primary_reject
            .as_ref()
            .and_then(|reject| attribute_reject_delta(reject, &MutationDeltaPreview::of(&tx)));
        self.record_run(ReasoningRunRecord::from_report(
            &output.report,
            &output.inferred,
            attribution.clone(),
        ));
        enforce_reasoner_gate(&output.inferred, output.report.status, attribution)?;

        if !ticket.begin_commit() {
            return Err(MutationError::Cancelled);
        }
        let summary = tx.commit().map_err(|error| MutationError::Store {
            kind,
            source: StoreError::Engine(error),
        })?;
        Ok(report.committed(summary.revision))
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
