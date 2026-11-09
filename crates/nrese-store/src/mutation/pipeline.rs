use std::sync::{Arc, RwLock};

use nrese_reasoner::ReasonerService;

use super::attribution::attribute_reject_delta;
use super::command::{MutationCommand, MutationCommitReport};
use super::error::MutationError;
use super::evidence;
use super::record::ReasoningRunRecord;
use super::ticket::MutationTicket;
use crate::config::{GateSeverity, ShaclGate};
use crate::delta::MutationDeltaPreview;
use crate::error::StoreError;
use crate::graph_store::{GraphDeleteReport, GraphTarget, GraphWriteReport, GraphWriteRequest};
use crate::service::StoreService;
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};

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
/// against the TBox).
///
/// Every public write of the store comes here: the commands of the protocols, and
/// [`MutationCommand::Delta`] for any other front-end ([`crate::DeltaPlanner`]). The
/// store's own writes outside it are crate-private (its system graphs, scratch stores);
/// bulk loads, rematerialisation and a replica's log are explicit operations of their own.
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

impl MutationPipeline {
    pub fn new(store: Arc<StoreService>, reasoner: Arc<ReasonerService>) -> Self {
        store.use_reasoning_rules(reasoner.config().materialised_program());
        store.use_dl(
            reasoner.config().mode().is_dl(),
            reasoner.config().rules.is_some(),
        );
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

    /// Applies `command` as the store's owner ([`crate::Requester::all`]: every graph)
    /// through every gate, to the end (no ticket cancels it): what an embedding
    /// application, a tool or a test writes with. A user's request goes through
    /// [`Self::apply`] with its requester and ticket.
    pub fn apply_as_owner(
        &self,
        command: MutationCommand,
    ) -> Result<MutationCommitReport, MutationError> {
        self.apply(command, &crate::Requester::all(), &MutationTicket::new())
    }

    /// A SPARQL update, as [`Self::apply_as_owner`].
    pub fn execute_update(
        &self,
        request: &SparqlUpdateRequest,
    ) -> Result<UpdateExecutionReport, MutationError> {
        match self.apply_as_owner(MutationCommand::Update(request.clone()))? {
            MutationCommitReport::Applied { revision } => Ok(UpdateExecutionReport {
                applied: true,
                revision,
            }),
            other => unreachable!("update produced {other:?}"),
        }
    }

    /// [`Self::execute_update`] of the update's text.
    pub fn execute_update_str(&self, update: &str) -> Result<UpdateExecutionReport, MutationError> {
        self.execute_update(&SparqlUpdateRequest::new(update))
    }

    /// A Graph Store write, as [`Self::apply_as_owner`].
    pub fn execute_graph_write(
        &self,
        request: &GraphWriteRequest,
    ) -> Result<GraphWriteReport, MutationError> {
        match self.apply_as_owner(MutationCommand::GraphWrite(request.clone()))? {
            MutationCommitReport::GraphWrite(report) => Ok(report),
            other => unreachable!("graph write produced {other:?}"),
        }
    }

    /// A Graph Store delete, as [`Self::apply_as_owner`].
    pub fn execute_graph_delete(
        &self,
        target: &GraphTarget,
    ) -> Result<GraphDeleteReport, MutationError> {
        match self.apply_as_owner(MutationCommand::GraphDelete(target.clone()))? {
            MutationCommitReport::GraphDelete(report) => Ok(report),
            other => unreachable!("graph delete produced {other:?}"),
        }
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
        // The request's one envelope: its WHERE clauses and its DL gate draw from it, and so
        // does what a transfer reserves of the pending changes, for as long as they live.
        let envelope = self.store.envelope(None);
        tx.charge_pending_to(Arc::new(envelope.part()))
            .map_err(|error| store_error(error.into()))?;
        let context = crate::mutation::command::UpdateContext {
            memory: envelope.clone(),
            cancellation: ticket.evaluation_token(),
            union_default_graph: self.store.config().union_default_graph,
            geosparql_stated_only: self.store.config().geosparql_stated_only,
            search_scope: self.store.search_scope(),
            services: self.store.services(),
            namespaces: self.store.namespaces().all(),
            supports: Arc::clone(self.store.supports()),
            compilation: self.store.support_compilation(),
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
            let memory = self.store.memory_stop();
            let stop = || ticket.is_cancelled() || memory.exceeded();
            let room = || memory.room();
            let operation = nrese_reasoner::eval::OperationStop::new(&stop).within(&room);
            // The violations' list draws on the request's envelope: past it, the list stops
            // and the commit is still rejected (the change is inconsistent either way).
            let report_memory = Arc::new(envelope.part());
            let (violations, materialisation, changed) =
                crate::reasoning::apply_delta(program, cached, &mut tx, &operation, &report_memory)
                    .map_err(|interrupted| {
                        memory
                            .error(interrupted)
                            .map_or(MutationError::Cancelled, &store_error)
                    })?;
            tracing::debug!(?materialisation, "commit-path materialisation");
            crate::reasoning::log_diagnostics(
                &materialisation.diagnostics,
                materialisation.diagnostics_total,
                "the commit leaves an ontology axiom unusable",
            );
            // What a committer that may read only some graphs is told, whichever gate
            // rejects ([`evidence`]): its view's reasons; the OWL 2 DL one on the DL gate's
            // budget where the gate didn't run.
            let resources = context.resources();
            let dl_own_budget = |view: &evidence::EvidenceView<'_, '_>| {
                crate::dl::gate::view_reason_on_its_budget(
                    self.store.dl().active(),
                    &self.store.config().dl,
                    view,
                    &stop,
                    &envelope,
                )
            };
            if !violations.consistent() {
                // A list that stopped at its budget before its first entry has none to
                // explain; the change is rejected all the same. The operator's record
                // holds the whole first violation, whoever committed.
                let explanation = violations.first().map(|violation| {
                    crate::reasoning::explain_violation(program, &violation, &tx, None)
                });
                let attribution = explanation.as_ref().and_then(|explanation| {
                    attribute_reject_delta(explanation, &MutationDeltaPreview::of(&tx))
                });
                self.record_run(ReasoningRunRecord::from_report(
                    &materialisation,
                    explanation.clone(),
                    attribution.clone(),
                ));
                let full = || {
                    let count = match violations.complete() {
                        true => violations.at_least().to_string(),
                        false => format!("at least {}", violations.at_least()),
                    };
                    let mut detail = match &explanation {
                        Some(explanation) => format!(
                            "mutation violates {count} consistency check(s); first: {}",
                            explanation.summary
                        ),
                        None => format!("mutation violates {count} consistency check(s)"),
                    };
                    if !violations.complete() {
                        detail.push_str(
                            " (the list of violations stopped at the request's memory budget)",
                        );
                    }
                    detail.push_str(&evidence::trigger(attribution.as_ref()));
                    evidence::Draft {
                        detail,
                        explanation,
                        attribution,
                    }
                };
                let view = evidence::EvidenceView::new(&requester.read, &tx);
                let reject = view.reject(full, |view| {
                    self.view_reason(view, &dl_own_budget, &resources)
                });
                return Err(MutationError::Rejected(Box::new(reject)));
            }
            self.record_run(ReasoningRunRecord::from_report(
                &materialisation,
                None,
                None,
            ));
            // The owl2-dl mode: the state the commit leaves, under OWL 2 DL.
            let mut dl = self.store.dl().active().then(|| {
                let mut bounds = crate::dl::bounds::prepare(&self.store, &tx, &stop);
                let gate = crate::dl::gate::check_commit(
                    &self.store,
                    &tx,
                    &stop,
                    &mut bounds,
                    &evidence::EvidenceView::new(&requester.read, &tx),
                    &|view, dl_gate_budget| self.view_reason(view, dl_gate_budget, &resources),
                    &envelope,
                );
                (gate, bounds)
            });
            if let Some((gate, _)) = &mut dl
                && let Some(reject) = gate.reject.take()
            {
                return Err(MutationError::Rejected(Box::new(reject)));
            }
            crate::shacl::check_shapes(
                &tx,
                &self.store.config().shapes_graph,
                &evidence::EvidenceView::new(&requester.read, &tx),
            )
            .map_err(store_error)?;
            // The view's OWL 2 DL reason is looked for even where the whole data is
            // consistent: the RDF mapping isn't monotone (an annotation property declared in
            // another graph changes what the view's statements say).
            self.shacl_gate(
                &tx,
                &resources,
                store_error,
                &evidence::EvidenceView::new(&requester.read, &tx),
                &|view| self.view_reason(view, &dl_own_budget, &resources),
            )?;
            if !ticket.begin_commit() {
                return Err(MutationError::Cancelled);
            }
            // What the commit touched, for the support graph sets kept for readers under
            // graph access (only while there are any).
            let supports = self.store.supports();
            let touched = supports.wants_commits().then(|| supports.touched(&tx));
            let before = tx.base().revision();
            // With the rewriting on, the QL schema notes what the commit changes of
            // individuals' memberships of class expressions, before the commit publishes.
            let summary = match self.store.query_settings().ql() {
                Some(ql) => ql.commit(tx),
                None => tx.commit(),
            }
            .map_err(|error| MutationError::Store {
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
        crate::shacl::check_shapes(
            &tx,
            &self.store.config().shapes_graph,
            &evidence::EvidenceView::new(&requester.read, &tx),
        )
        .map_err(store_error)?;
        // No OWL 2 reasoning: no OWL 2 reason to look for.
        let resources = context.resources();
        self.shacl_gate(
            &tx,
            &resources,
            store_error,
            &evidence::EvidenceView::new(&requester.read, &tx),
            &|view| self.view_reason(view, &|_| None, &resources),
        )?;
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
    /// after reasoning, and rejects the commit if the policy says so. It runs on the write's
    /// `resources` (its cancellation and envelope). A validation they stop fails the write
    /// as `stopped` maps it, under every policy: it is no verdict on the data, so neither a
    /// rejection nor a warning to commit past.
    ///
    /// A restricted committer is told what `restricted` tells of its view ([`evidence`]).
    fn shacl_gate(
        &self,
        tx: &nrese_engine::Transaction<'_>,
        resources: &nrese_sparql::Resources,
        stopped: impl FnOnce(StoreError) -> MutationError,
        view: &evidence::EvidenceView<'_, '_>,
        restricted: &dyn Fn(&evidence::EvidenceView<'_, '_>) -> evidence::Draft,
    ) -> Result<(), MutationError> {
        let gate = self.store.config().shacl_gate;
        if gate == ShaclGate::Off {
            return Ok(());
        }
        let report =
            match crate::shacl::introduced(tx, &self.store.config().shapes_graph, resources) {
                Ok(report) => report,
                Err(error @ StoreError::SparqlEvaluation(_)) => return Err(stopped(error)),
                // The stored shapes' problems, as the committer may be told them.
                Err(error) => {
                    let error = view.shapes_error(&self.store.config().shapes_graph, error);
                    return Err(MutationError::Gate(error.to_string()));
                }
            };
        let Some(report) = report else {
            return Ok(());
        };
        if report.results.is_empty() && report.failures.is_empty() {
            return Ok(());
        }
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
                let full = || evidence::Draft {
                    detail,
                    explanation: None,
                    attribution: None,
                };
                let reject = view.reject(full, restricted);
                Err(MutationError::Rejected(Box::new(reject)))
            }
            _ => {
                tracing::warn!("{detail} (SHACL gate: not rejected)");
                Ok(())
            }
        }
    }

    /// What a committer that may read only some graphs is told of a rejection, whichever
    /// gate rejected ([`evidence`]): the first reason its own view shows, in the gates'
    /// order. `dl`: the view's OWL 2 DL reason (`None` without the DL gate). Its OWL 2 RL
    /// reason isn't looked for yet: that needs a bounded, charged ground program of its view
    /// (Q07 S1b), so it is told the DL or SHACL reason its view shows, or none.
    fn view_reason(
        &self,
        view: &evidence::EvidenceView<'_, '_>,
        dl: &dyn Fn(&evidence::EvidenceView<'_, '_>) -> Option<evidence::Draft>,
        resources: &nrese_sparql::Resources,
    ) -> evidence::Draft {
        dl(view).unwrap_or_else(|| self.view_reason_after_dl(view, resources))
    }

    /// What a restricted committer is told after the OWL 2 DL reason ([`evidence`]): the
    /// SHACL reason its view shows, where the gate rejects on one, else that its view shows
    /// none.
    fn view_reason_after_dl(
        &self,
        view: &evidence::EvidenceView<'_, '_>,
        resources: &nrese_sparql::Resources,
    ) -> evidence::Draft {
        let ShaclGate::Enforce(least) = self.store.config().shacl_gate else {
            return evidence::nothing_shown();
        };
        let rejects =
            |result: &nrese_shacl::ValidationResult| severity(result.severity.as_str()) >= least;
        crate::shacl::view_reason(view, &self.store.config().shapes_graph, rejects, resources)
            .unwrap_or_else(evidence::nothing_shown)
    }

    fn record_run(&self, run: ReasoningRunRecord) {
        if let Ok(mut slot) = self.last_reasoning_run.write() {
            *slot = Some(run);
        }
    }
}

/// A SHACL result's severity, as the gate's policy ranks it (a shape's own severity IRI
/// ranks as a violation).
fn severity(iri: &str) -> GateSeverity {
    match iri {
        "http://www.w3.org/ns/shacl#Info" => GateSeverity::Info,
        "http://www.w3.org/ns/shacl#Warning" => GateSeverity::Warning,
        _ => GateSeverity::Violation,
    }
}
