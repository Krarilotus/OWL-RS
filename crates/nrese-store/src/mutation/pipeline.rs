use std::sync::{Arc, RwLock};

use nrese_reasoner::ReasonerService;

use super::attribution::attribute_reject_delta;
use super::command::{MutationCommand, MutationCommitReport};
use super::error::{MutationError, MutationReject};
use super::record::ReasoningRunRecord;
use super::ticket::MutationTicket;
use crate::delta::MutationDeltaPreview;
use crate::error::StoreError;
use crate::service::StoreService;

/// The single owner of write semantics: plan → validate → claim → commit.
///
/// 1. Open an engine transaction. Its writer slot serialises writers; readers never wait.
/// 2. Apply the command to the transaction. Nothing is visible yet, and the transaction's
///    exact delta *is* the preview: no copy of the dataset is made.
/// 3. Reason: with reasoner v2, the delta executor updates the inferred stack in the same
///    transaction; a consistency violation rejects the mutation.
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

        // Reasoner v2 maintains the inferred stack from a correct one: materialise it first
        // if nothing records that it is current (a fresh or preloaded store).
        if let Some(ruleset) = self.reasoner.config().materialised_ruleset()
            && !self
                .store
                .reasoning_state()
                .is_some_and(|state| state.is_current_for(ruleset))
        {
            self.store.rematerialise(ruleset).map_err(store_error)?;
        }
        let mut tx = self.store.engine().transaction();
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }
        let report = command
            .apply(
                &mut tx,
                ticket.evaluation_token(),
                self.store.config().union_default_graph,
            )
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
            // A cancelled request stops the reasoning; dropping `tx` then discards both
            // the asserted and the inferred changes, and releases the writer.
            let stop = || ticket.is_cancelled();
            let (violations, materialisation, changed) =
                crate::reasoning::apply_delta(program, cached, &mut tx, &stop)
                    .map_err(|_| MutationError::Cancelled)?;
            tracing::debug!(?materialisation, "commit-path materialisation");
            crate::reasoning::log_diagnostics(
                &materialisation.diagnostics,
                materialisation.diagnostics_total,
                "the commit leaves an ontology axiom unusable",
            );
            if let Some(violation) = violations.first() {
                let explanation = crate::reasoning::explain(program, violation, &tx);
                let attribution =
                    attribute_reject_delta(&explanation, &MutationDeltaPreview::of(&tx));
                let mut detail = format!(
                    "mutation violates {} consistency check(s); first: {}",
                    violations.len(),
                    explanation.summary
                );
                if let Some((s, p, o)) = attribution
                    .as_ref()
                    .and_then(super::attribution::RejectAttribution::likely_commit_trigger)
                {
                    detail.push_str(&format!(" Likely commit-local trigger: <{s}> <{p}> <{o}>."));
                }
                self.record_run(ReasoningRunRecord::from_report(
                    &materialisation,
                    Some(explanation.clone()),
                    attribution.clone(),
                ));
                return Err(MutationError::Rejected(Box::new(MutationReject {
                    detail,
                    explanation: Some(explanation),
                    attribution,
                })));
            }
            self.record_run(ReasoningRunRecord::from_report(
                &materialisation,
                None,
                None,
            ));
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
            drop(ground);
            // In quarantine the commit checked only its own facts: revalidate everything, so
            // the store leaves quarantine once the data is repaired. The commit itself stands.
            if matches!(
                self.store.consistency(),
                crate::ConsistencyStatus::Inconsistent { .. }
            ) && let Err(error) = self.store.rematerialise(ruleset)
            {
                tracing::error!(%error, "revalidation after a commit in quarantine failed");
            }
            return Ok(report.committed(summary.revision));
        }

        // Without reasoning, commits don't maintain the inferred stack.
        self.store.invalidate_reasoning().map_err(store_error)?;
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
