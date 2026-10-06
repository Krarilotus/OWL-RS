use std::sync::{Arc, RwLock};

use nrese_reasoner::ReasonerService;

use super::attribution::attribute_reject_delta;
use super::command::{MutationCommand, MutationCommitReport};
use super::error::{MutationError, MutationReject};
use super::record::ReasoningRunRecord;
use super::ticket::MutationTicket;
use crate::config::{GateSeverity, ShaclGate};
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
    ground: std::sync::Mutex<Option<(u64, nrese_reasoner::eval::GroundProgram)>>,
    /// Set when another pipeline (other rules) took over the store: writes are refused
    /// from then on, so none applies this pipeline's rules to a stack made for others.
    retired: std::sync::atomic::AtomicBool,
}

/// The statements `tx` inserts or deletes, asserted and inferred, as reasoner facts.
fn touched(tx: &nrese_engine::Transaction<'_>) -> Vec<nrese_reasoner::ir::Triple> {
    let quads = tx.inserted().chain(tx.deleted());
    let mut facts: Vec<nrese_reasoner::ir::Triple> = quads
        .map(|q| [q.subject.raw(), q.predicate.raw(), q.object.raw()])
        .chain(
            tx.inferred_inserted()
                .chain(tx.inferred_deleted())
                .map(|t| [t.subject.raw(), t.predicate.raw(), t.object.raw()]),
        )
        .collect();
    facts.sort_unstable();
    facts.dedup();
    facts
}

impl MutationPipeline {
    pub fn new(store: Arc<StoreService>, reasoner: Arc<ReasonerService>) -> Self {
        store.use_reasoning_rules(reasoner.config().materialised_program());
        store.use_dl(reasoner.config().mode().is_dl());
        Self {
            store,
            reasoner,
            last_reasoning_run: RwLock::new(None),
            program: std::sync::OnceLock::new(),
            ground: std::sync::Mutex::new(None),
            retired: std::sync::atomic::AtomicBool::new(false),
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

    /// Retires this pipeline: another one with other rules takes over its store, and this
    /// one's writes are refused ([`MutationError::Retired`]) from now on.
    pub fn retire(&self) {
        self.retired
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether [`Self::retire`] was called.
    pub fn is_retired(&self) -> bool {
        self.retired.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Applies `command` for `requester` unless a gate rejects it or `ticket` is cancelled
    /// before the commit starts. Blocking; call it from a blocking context.
    pub fn apply(
        &self,
        command: MutationCommand,
        requester: &crate::Requester,
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
        if self.is_retired() {
            return Err(MutationError::Retired);
        }

        // Reasoner v2 maintains the inferred stack from a correct one: materialise it first
        // if nothing records that it is current (a fresh or preloaded store). A retired
        // pipeline must not: its rules are no longer the store's, and the store's closure
        // belongs to the pipeline that took over. Retirement is part of the stop condition,
        // which the rematerialisation checks a last time under the writer slot before it
        // commits, so retiring during this preflight changes nothing either.
        if let Some(rules) = self.reasoner.config().materialised_program()
            && !self.store.reasoning_is_current(&rules)
        {
            let stop = || ticket.is_cancelled() || self.is_retired();
            match self.store.rematerialise_until(&rules, &stop) {
                Ok(_) => {}
                Err(StoreError::MaterialisationCancelled) if self.is_retired() => {
                    return Err(MutationError::Retired);
                }
                Err(StoreError::MaterialisationCancelled) => return Err(MutationError::Cancelled),
                Err(error) => return Err(store_error(error)),
            }
        }
        let mut tx = self.store.engine().transaction();
        // Checked with the writer slot held: a pipeline that took over the store
        // rematerialises after retiring this one, so a write that got the slot before
        // commits first and is recomputed, and one after is refused here.
        if self.is_retired() {
            return Err(MutationError::Retired);
        }
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }
        let context = crate::mutation::command::UpdateContext {
            cancellation: ticket.evaluation_token(),
            union_default_graph: self.store.config().union_default_graph,
            geosparql_stated_only: self.store.config().geosparql_stated_only,
            services: self.store.services(),
            namespaces: self.store.namespaces().all(),
        };
        let report = command
            .apply(&mut tx, requester, &context)
            .map_err(store_error)?;

        if let Some(rules) = self.reasoner.config().materialised_program() {
            // Reasoner v2: the closure goes into the inferred stack, in this transaction.
            let program = self.program.get_or_init(|| {
                let tx = &tx;
                crate::reasoning::Program::compile(&rules, &|term| tx.intern(term))
                    .hiding_unnamed_classes(self.store.config().hide_unnamed_classes)
                    .by_representatives(self.store.config().equality_by_representatives)
                    .storing_representatives(self.store.config().equality_compact)
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
            // So does the process's memory limit: the commit is refused, not the machine.
            let watch = self.store.memory_watch();
            let over = || watch.as_ref().is_some_and(|watch| watch.exceeded());
            let stop = || ticket.is_cancelled() || over();
            let (violations, materialisation, changed) =
                crate::reasoning::apply_delta(program, cached, &mut tx, &stop).map_err(|_| {
                    match &watch {
                        Some(watch) if watch.exceeded() => {
                            store_error(StoreError::ProcessMemoryLimit {
                                limit: watch.limit(),
                            })
                        }
                        _ => MutationError::Cancelled,
                    }
                })?;
            tracing::debug!(?materialisation, "commit-path materialisation");
            crate::reasoning::log_diagnostics(
                &materialisation.diagnostics,
                materialisation.diagnostics_total,
                "the commit leaves an ontology axiom unusable",
            );
            if let Some(violation) = violations.first() {
                let explanation = crate::reasoning::explain_violation(program, violation, &tx);
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
            // The owl2-dl mode: the state the commit leaves, under OWL 2 DL.
            let dl = self.store.dl().active().then(|| {
                let mut bounds = crate::dl::bounds::prepare(&self.store, &tx, &stop);
                let gate = crate::dl::gate::check_commit(&self.store, &tx, &stop, &mut bounds);
                (gate, bounds)
            });
            if let Some((gate, _)) = &dl
                && let Some(detail) = &gate.reject
            {
                return Err(MutationError::Rejected(Box::new(MutationReject {
                    detail: detail.clone(),
                    explanation: Some(nrese_reasoner::RejectExplanation {
                        summary: detail.clone(),
                        violated_constraint: "owl2-dl-consistency".to_owned(),
                        focus_resource: String::new(),
                        evidence: Vec::new(),
                    }),
                    attribution: None,
                })));
            }
            crate::shacl::check_shapes(&tx, &self.store.config().shapes_graph)
                .map_err(store_error)?;
            self.shacl_gate(&tx)?;
            if !ticket.begin_commit() {
                return Err(MutationError::Cancelled);
            }
            // What the commit touched, for the support graph sets kept for readers under
            // graph access (only while there are any).
            let supports = self.store.supports();
            let touched = supports.wants_commits().then(|| touched(&tx));
            let before = tx.base().revision();
            let summary = tx.commit().map_err(|error| MutationError::Store {
                kind,
                source: StoreError::Engine(error),
            })?;
            if let Some(touched) = touched {
                supports.note_commit(before, summary.revision, touched);
            }
            if let Some((gate, bounds)) = dl {
                crate::dl::bounds::install(&self.store, bounds, summary.revision);
                self.store.dl().record(crate::dl::DlStatus {
                    revision: summary.revision,
                    consistency: gate.checked,
                });
            }
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
            // A class left out became consumable: its memberships are computed in full.
            if materialisation.needs_rematerialisation
                && let Err(error) = self.store.rematerialise(&rules)
            {
                tracing::error!(%error, "rematerialisation for an unnamed class that became used failed");
            }
            // In quarantine the commit checked only its own facts: revalidate everything, so
            // the store leaves quarantine once the data is repaired. The commit itself stands.
            if matches!(
                self.store.consistency(),
                crate::ConsistencyStatus::Inconsistent { .. }
            ) && let Err(error) = self.store.rematerialise(&rules)
            {
                tracing::error!(%error, "revalidation after a commit in quarantine failed");
            }
            return Ok(report.committed(summary.revision));
        }

        // Without reasoning, commits don't maintain the inferred stack.
        self.store.invalidate_reasoning().map_err(store_error)?;
        crate::shacl::check_shapes(&tx, &self.store.config().shapes_graph).map_err(store_error)?;
        self.shacl_gate(&tx)?;
        if !ticket.begin_commit() {
            return Err(MutationError::Cancelled);
        }
        let summary = tx.commit().map_err(|error| MutationError::Store {
            kind,
            source: StoreError::Engine(error),
        })?;
        Ok(report.committed(summary.revision))
    }

    /// The SHACL commit gate (C2): validates what the transaction's changes can affect,
    /// after reasoning, and rejects the commit if the policy says so.
    fn shacl_gate(&self, tx: &nrese_engine::Transaction<'_>) -> Result<(), MutationError> {
        let gate = self.store.config().shacl_gate;
        if gate == ShaclGate::Off {
            return Ok(());
        }
        let report = crate::shacl::introduced(tx, &self.store.config().shapes_graph)
            .map_err(|error| MutationError::Gate(error.to_string()))?;
        let Some(report) = report else {
            return Ok(());
        };
        if report.results.is_empty() && report.failures.is_empty() {
            return Ok(());
        }
        let severity = |iri: &str| match iri {
            "http://www.w3.org/ns/shacl#Info" => GateSeverity::Info,
            "http://www.w3.org/ns/shacl#Warning" => GateSeverity::Warning,
            _ => GateSeverity::Violation,
        };
        let first = report.results.first().map_or_else(String::new, |result| {
            format!(
                "; first: focus node {}, {}",
                result.focus_node, result.component
            )
        });
        let detail = format!(
            "the commit introduces {} SHACL result(s) and {} failure(s){first}",
            report.results.len(),
            report.failures.len()
        );
        match gate {
            ShaclGate::Enforce(least)
                if !report.failures.is_empty()
                    || report
                        .results
                        .iter()
                        .any(|result| severity(result.severity.as_str()) >= least) =>
            {
                Err(MutationError::Rejected(Box::new(MutationReject {
                    detail,
                    explanation: None,
                    attribution: None,
                })))
            }
            _ => {
                tracing::warn!("{detail} (SHACL gate: not rejected)");
                Ok(())
            }
        }
    }

    fn record_run(&self, run: ReasoningRunRecord) {
        if let Ok(mut slot) = self.last_reasoning_run.write() {
            *slot = Some(run);
        }
    }
}
