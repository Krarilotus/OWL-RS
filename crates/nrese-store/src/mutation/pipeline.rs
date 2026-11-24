use std::sync::{Arc, RwLock};

use nrese_engine::{Snapshot, TermId};
use nrese_reasoner::ReasonerService;
use nrese_reasoner::eval::GroundProgram;

use super::attribution::attribute_reject_delta;
use super::command::{MutationCommand, MutationCommitReport};
use super::error::{MutationError, RejectKind};
use super::evidence;
use super::record::ReasoningRunRecord;
use super::ticket::{MutationTicket, publishing};
use crate::config::{GateSeverity, ShaclGate};
use crate::delta::MutationDeltaPreview;
use crate::error::StoreError;
use crate::graph_store::{GraphDeleteReport, GraphTarget, GraphWriteReport, GraphWriteRequest};
use crate::service::StoreService;
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};

type GroundKey = (u64, Option<TermId>);

fn ground_key(snapshot: &Snapshot) -> GroundKey {
    let identity = snapshot.identity();
    (identity.revision, identity.equality)
}

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
    /// Reasoner v2's ground program, keyed by the revision and how it was read.
    ground: std::sync::Mutex<Option<(GroundKey, Arc<GroundProgram>)>>,
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
        self.apply_noted(command, requester, ticket, None)
    }

    /// [`Self::apply`], its commit noted in a journal beside the store's log by `note`
    /// ([`nrese_engine::Transaction::note_commit`]): a quarantine readmission's resolution.
    pub(crate) fn apply_noted<'p>(
        &'p self,
        command: MutationCommand,
        requester: &crate::Requester,
        ticket: &MutationTicket,
        note: Option<nrese_engine::CommitNote<'p>>,
    ) -> Result<MutationCommitReport, MutationError> {
        let kind = command.kind();
        let known_error = |error| ticket.end_error(error);
        let store_error = |source| known_error(MutationError::Store { kind, source });
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }
        if self.is_retired() {
            return Err(known_error(MutationError::Retired));
        }

        // Reasoner v2 maintains the inferred stack from a correct one: materialise it first
        // if nothing records that it is current (a fresh or preloaded store). A retired
        // pipeline must not: its rules are no longer the store's, and the store's closure
        // belongs to the pipeline that took over. Retirement is part of the stop condition,
        // which the rematerialisation checks a last time under the writer slot before it
        // commits, so retiring during this preflight changes nothing either.
        //
        // The check that decides is the one under the transaction's writer slot:
        // a write that doesn't maintain the closure (a bulk load, `clear_inferred`) may clear
        // the certificate between the preflight and the slot. Then the slot goes back, the
        // closure is rematerialised, and the transaction is taken again.
        let rules = self.reasoner.config().materialised_program();
        let mut tx = loop {
            if let Some(rules) = &rules
                && !self.store.reasoning_is_current(rules)
            {
                let stop = || ticket.stop_requested() || self.is_retired();
                match self.store.rematerialise_until(rules, &stop) {
                    Ok(_) => {}
                    Err(StoreError::MaterialisationCancelled) if self.is_retired() => {
                        return Err(known_error(MutationError::Retired));
                    }
                    Err(StoreError::MaterialisationCancelled) => {
                        return Err(known_error(MutationError::Cancelled));
                    }
                    Err(error) => return Err(store_error(error)),
                }
            }
            #[cfg(test)]
            crate::service::hooks::run(&crate::service::hooks::AFTER_PREFLIGHT);
            let tx = self.store.engine().transaction();
            let writer = tx.writer().expect("a transaction holds the writer slot");
            match &rules {
                Some(rules) if !self.store.reasoning_is_current_held(writer, rules) => drop(tx),
                _ => break tx,
            }
        };
        if let Some(note) = note {
            tx.note_commit(note);
        }
        // Checked with the writer slot held: a pipeline that took over the store
        // rematerialises after retiring this one, so a write that got the slot before
        // commits first and is recomputed, and one after is refused here.
        if self.is_retired() {
            return Err(known_error(MutationError::Retired));
        }
        if ticket.is_cancelled() {
            return Err(MutationError::Cancelled);
        }
        // The request's one envelope: its WHERE clauses and its DL gate draw from it, and so
        // does what a transfer reserves of the pending changes, for as long as they live.
        let envelope = self.store.envelope(None);
        tx.charge_pending_to(Arc::new(envelope.part()))
            .map_err(|error| store_error(error.into()))?;
        let context = self
            .store
            .update_context(envelope.clone(), ticket.evaluation_token());
        let report = command
            .apply(&mut tx, requester, &context)
            .map_err(store_error)?;

        if let Some(rules) = self.reasoner.config().materialised_program() {
            // Reasoner v2: the closure goes into the inferred stack, in this transaction.
            // A compilation the process's memory interrupts refuses the commit and caches
            // nothing: the next commit compiles again. Writers are serialised by the
            // transaction, and the program is a function of the rules, the configuration and
            // the dictionary's ids for the rules' constants, which never change once interned;
            // so a second compilation is the first one's value, and whichever is set stands.
            let program = match self.program.get() {
                Some(program) => program,
                None => {
                    let config = self.store.config();
                    let compiled = crate::reasoning::Program::compile(&rules, &|t| tx.intern(t))
                        .hiding_unnamed_classes(config.hide_unnamed_classes)
                        .map_err(|interrupted| {
                            store_error(self.store.interruption_refusal(interrupted))
                        })?
                        .by_representatives(config.equality_by_representatives)
                        .storing_representatives(config.equality_compact);
                    self.program.get_or_init(|| compiled)
                }
            };
            // The commit's stack is read as the program keeps it, installed with it: expanded
            // over representatives from the commit that brings the store's first `owl:sameAs`.
            let reading = program.stored_equality();
            tx.read_equality_as(reading);
            let base_key = ground_key(tx.base());
            // Writers are serialised by the transaction, so the cache can't change meanwhile.
            let mut ground = self.ground.lock().unwrap_or_else(|p| p.into_inner());
            let cached = ground
                .as_ref()
                .filter(|(at, _)| *at == base_key)
                .map(|(_, program)| program.as_ref());
            // A cancelled request stops the reasoning; dropping `tx` then discards both
            // the asserted and the inferred changes, and releases the writer.
            // So does the process's memory limit: the commit is refused, not the machine.
            let memory = self.store.memory_stop();
            let stop = || ticket.stop_requested() || memory.exceeded();
            let room = || memory.room();
            let operation = nrese_reasoner::eval::OperationStop::new(&stop).within(&room);
            // The violations' list draws on the request's envelope: past it, the list stops
            // and the commit is still rejected (the change is inconsistent either way).
            let report_memory = Arc::new(envelope.part());
            let working_memory = || self.store.operation_memory();
            // A rebuild's own report judges only over a state assessed consistent.
            let base_consistent = matches!(
                self.store.consistency(),
                crate::ConsistencyStatus::Consistent
            );
            let (violations, materialisation, changed, literal_equality) =
                crate::reasoning::apply_delta(
                    program,
                    cached,
                    &mut tx,
                    &operation,
                    &report_memory,
                    &working_memory,
                    base_consistent,
                )
                .map_err(|interrupted| {
                    memory
                        .error(interrupted)
                        .map_or_else(|| known_error(MutationError::Cancelled), &store_error)
                })?;
            let inconsistent = !violations.consistent();
            if inconsistent {
                ticket.claim_error()?;
            }
            tracing::debug!(?materialisation, "commit-path materialisation");
            // Whether the closure the commit leaves leaves out a list axiom: its program's
            // whole list diagnostics (the report's are only the new ones). With no program at
            // hand (an update with no reasoning delta, on a pipeline that caches none: a
            // fresh one, a reopened store's), the store's record of the pending version.
            let lists_left_out = match changed.as_ref().or(cached) {
                Some(p) => !p.list_diagnostics.is_empty(),
                None => self.store.lists_left_out(&tx.pending_snapshot()),
            };
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
            if inconsistent {
                // A list that stopped at its budget before its first entry has none to
                // explain; the change is rejected all the same. The operator's record
                // holds the whole first violation, whoever committed.
                let explanation = if operation.fired() {
                    None
                } else {
                    violations.first().map(|violation| {
                        nrese_reasoner::engine::explain_violation_until(
                            program,
                            &violation,
                            &tx,
                            None,
                            changed.as_ref().or(cached),
                            &|| operation.fired(),
                        )
                    })
                };
                let attribution = if operation.fired() {
                    None
                } else {
                    explanation.as_ref().and_then(|explanation| {
                        attribute_reject_delta(explanation, &MutationDeltaPreview::of(&tx))
                    })
                };
                self.record_run(ReasoningRunRecord::from_report(
                    &materialisation,
                    explanation.clone(),
                    attribution.clone(),
                ));
                // Under `owl2-dl`, where OWL 2 DL may read the data otherwise, the rules'
                // clash isn't stated as an inconsistency (`WhyUndecided`).
                let dl = self.store.dl();
                let treatments = &self.store.config().dl.treatments;
                let undecided = dl
                    .active()
                    .then(|| dl.undecided_after(&tx, lists_left_out, treatments))
                    .flatten();
                let full = || {
                    let count = match violations.complete() {
                        true => violations.at_least().to_string(),
                        false => format!("at least {}", violations.at_least()),
                    };
                    let mut detail = match (undecided, &explanation) {
                        (Some(why), _) => {
                            format!("{}; the write is refused.", why.text(violations.at_least()))
                        }
                        (None, Some(explanation)) => format!(
                            "mutation violates {count} consistency check(s); first: {}",
                            explanation.summary
                        ),
                        (None, None) => format!("mutation violates {count} consistency check(s)"),
                    };
                    if !violations.complete() {
                        detail.push_str(
                            " (the list of violations stopped at the request's memory budget)",
                        );
                    }
                    detail.push_str(&evidence::trigger(attribution.as_ref()));
                    evidence::Draft {
                        kind: match undecided {
                            Some(_) => RejectKind::Undecided,
                            None => RejectKind::Inconsistent,
                        },
                        detail,
                        explanation,
                        attribution,
                    }
                };
                let view = evidence::EvidenceView::new(&requester.read, &tx);
                let reject = view.reject(full, |view| {
                    self.view_reason(view, None, &dl_own_budget, &resources)
                });
                return Err(MutationError::Rejected(Box::new(reject)));
            }
            self.record_run(ReasoningRunRecord::from_report(
                &materialisation,
                None,
                None,
            ));
            // The owl2-dl mode: the state the commit leaves, under OWL 2 DL.
            let dl = if self.store.dl().active() {
                let mut bounds =
                    crate::dl::bounds::prepare(&self.store, &tx, &stop, lists_left_out);
                let mut gate = crate::dl::gate::check_commit(
                    &self.store,
                    &tx,
                    &stop,
                    &mut bounds,
                    &evidence::EvidenceView::new(&requester.read, &tx),
                    &|view, dl_gate_budget| {
                        self.view_reason(view, None, dl_gate_budget, &resources)
                    },
                    &envelope,
                );
                if let Some(reject) = gate.reject.take() {
                    return Err(known_error(MutationError::Rejected(Box::new(reject))));
                }
                // What the premise of the rules' closure tells at the revision, named from the
                // transaction once, here: of the ontology the gate or the bounds read; on the
                // RL route nothing (the route holds only where the premise does); after a
                // delta, its base's record (the delta keeps what the reader reads); else no
                // record, never "nothing".
                let name = |t| crate::dl::consistency::named(&tx, t);
                let read = bounds
                    .ontology
                    .take()
                    .map(|o| crate::dl::query::left_out(&o, &name));
                let base = tx.base().revision();
                let record = read.or_else(|| match &bounds.prepared {
                    crate::dl::bounds::Prepared::Rules(_) => Some(None),
                    crate::dl::bounds::Prepared::Delta(_) => (self.store.dl().status())
                        .filter(|s| s.revision == base)
                        .and_then(|s| s.left_out),
                    crate::dl::bounds::Prepared::Rebuilt(_)
                    | crate::dl::bounds::Prepared::Unavailable => None,
                });
                Some((gate, bounds, record))
            } else {
                None
            };
            let shapes = crate::shacl::check_shapes(
                &tx,
                &self.store.config().shapes_graph,
                ticket,
                &evidence::EvidenceView::new(&requester.read, &tx),
            )
            .map_err(store_error)?;
            // The view's OWL 2 DL reason is looked for even where the whole data is
            // consistent: the RDF mapping isn't monotone (an annotation property declared in
            // another graph changes what the view's statements say).
            self.shacl_gate(
                &tx,
                ticket,
                &resources,
                store_error,
                &evidence::EvidenceView::new(&requester.read, &tx),
                |view, after| self.view_reason(view, Some(after), &dl_own_budget, &resources),
            )?;
            if !ticket.begin_commit() {
                return Err(MutationError::Cancelled);
            }
            // What the commit touched, for the support graph sets kept for readers under
            // graph access (only while there are any).
            let supports = self.store.supports();
            let touched = supports.wants_commits().then(|| supports.touched(&tx));
            let before = tx.base().revision();
            // The commit maintained the closure: its version carries its base's certificate,
            // the same object. With the rewriting on, that certificate's QL schema notes what
            // the commit changes of individuals' memberships of class expressions, before the
            // commit publishes.
            let ql = crate::service::Certificate::of(tx.base()).and_then(|c| c.ql.clone());
            // Its rights hold from here through the publication (contracts.md, lock order).
            let rights = ticket.hold_rights()?;
            // In quarantine the commit checks only its own facts: its version keeps no
            // assessment of its consistency. The mark rises first (odd: unassessed), then the
            // old assessment and its marker go, before the commit publishes: a removal that
            // fails fails the write and leaves the store in quarantine.
            let quarantined = self.store.is_quarantined();
            if quarantined {
                (self.store.unassessed).fetch_or(1, std::sync::atomic::Ordering::Release);
                let writer = tx.writer().expect("a transaction holds the writer slot");
                self.store.invalidate_held(writer).map_err(store_error)?;
            }
            // A commit that equated a literal with another term: its version and every later
            // one withhold the word from reads of literals, until a rematerialisation finds no
            // such equality (`ReasoningState::literal_equality`). The mark goes on the state
            // kept in memory, certificate or not, and on the version's certificate where it
            // has one.
            if literal_equality {
                let writer = tx.writer().expect("a transaction holds the writer slot");
                self.store.mark_literal_equality(writer);
            }
            if let Some(mut certificate) = tx.base().certificate().cloned() {
                if literal_equality {
                    certificate = StoreService::with_literal_equality(certificate);
                }
                tx.vouch(certificate);
            }
            if let Some(shapes) = shapes {
                tx.keep_watched(Arc::new(crate::shacl::ShapesCounted(Ok(shapes))));
            }
            let committed = match ql {
                Some(ql) => publishing(ticket, report, tx, |tx| ql.commit(tx)),
                None => publishing(ticket, report, tx, |tx| tx.commit()),
            };
            #[cfg(test)]
            if committed.is_err() {
                crate::service::hooks::run(&crate::service::hooks::AFTER_FAILED_COMMIT);
            }
            let (summary, report) = committed.map_err(|error| MutationError::Store {
                kind,
                source: StoreError::Engine(error),
            })?;
            drop(rights);
            #[cfg(test)]
            crate::service::hooks::run(&crate::service::hooks::AFTER_WRITE_PUBLICATION);
            if let Some(touched) = touched {
                supports.note_commit(before, summary.revision, touched);
            }
            (self.store.lists).commit(before, summary.revision, lists_left_out);
            if let Some((gate, bounds, left_out)) = dl {
                // On the RL route unproved (in quarantine) nothing decided the revision's
                // consistency: no verdict is recorded for it, and its reads check it
                // (`dl::query::consistency_at`).
                let rules = matches!(bounds.prepared, crate::dl::bounds::Prepared::Rules(_));
                let unproved = rules && !bounds.proves_consistency;
                crate::dl::bounds::install(&self.store, bounds, summary.revision);
                if !unproved {
                    self.store.dl().record(crate::dl::DlStatus {
                        revision: summary.revision,
                        consistency: gate.checked,
                        left_out,
                    });
                }
            }
            // A no-op keeps its base. A commit that changed the reading cannot keep a program
            // grounded under the previous reading.
            let next_key = if summary.revision == base_key.0 {
                base_key
            } else {
                (summary.revision, reading)
            };
            if next_key.1 != base_key.1 {
                *ground = None;
            } else if let Some(program) = changed {
                *ground = Some((next_key, Arc::new(program)));
            } else if let Some((at, _)) = ground.as_mut().filter(|(at, _)| *at == base_key) {
                *at = next_key;
            }
            drop(ground);
            #[cfg(test)]
            crate::service::hooks::run(&crate::service::hooks::BEFORE_REVALIDATION);
            // In quarantine the commit checked only its own facts: revalidate everything, so
            // the store leaves quarantine once the data is repaired. The commit itself stands.
            if quarantined && let Err(error) = self.store.rematerialise(&rules) {
                tracing::error!(%error, "revalidation after a commit in quarantine failed");
            }
            return Ok(report);
        }

        // Without reasoning, commits don't maintain the inferred stack.
        self.store
            .invalidate_held(tx.writer().expect("a transaction holds the writer slot"))
            .map_err(store_error)?;
        let shapes = crate::shacl::check_shapes(
            &tx,
            &self.store.config().shapes_graph,
            ticket,
            &evidence::EvidenceView::new(&requester.read, &tx),
        )
        .map_err(store_error)?;
        // No OWL 2 reasoning: no OWL 2 reason to look for.
        let resources = context.resources();
        self.shacl_gate(
            &tx,
            ticket,
            &resources,
            store_error,
            &evidence::EvidenceView::new(&requester.read, &tx),
            |view, after| self.view_reason(view, Some(after), &|_| None, &resources),
        )?;
        if !ticket.begin_commit() {
            return Err(MutationError::Cancelled);
        }
        let rights = ticket.hold_rights()?;
        if let Some(shapes) = shapes {
            tx.keep_watched(Arc::new(crate::shacl::ShapesCounted(Ok(shapes))));
        }
        let (_, report) = publishing(ticket, report, tx, |tx| tx.commit()).map_err(|error| {
            MutationError::Store {
                kind,
                source: StoreError::Engine(error),
            }
        })?;
        drop(rights);
        #[cfg(test)]
        crate::service::hooks::run(&crate::service::hooks::AFTER_WRITE_PUBLICATION);
        Ok(report)
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
        ticket: &MutationTicket,
        resources: &nrese_sparql::Resources,
        stopped: impl FnOnce(StoreError) -> MutationError,
        view: &evidence::EvidenceView<'_, '_>,
        restricted: impl FnOnce(
            &evidence::EvidenceView<'_, '_>,
            &nrese_engine::Snapshot,
        ) -> evidence::Draft,
    ) -> Result<(), MutationError> {
        let gate = self.store.config().shacl_gate;
        if gate == ShaclGate::Off {
            return Ok(());
        }
        let shapes_graph = &self.store.config().shapes_graph;
        let report = match crate::shacl::introduced(tx, shapes_graph, resources, ticket) {
            Ok(report) => report,
            Err(error @ StoreError::SparqlEvaluation(_)) => return Err(stopped(error)),
            // The stored shapes' problems, as the committer may be told them.
            Err(error) => {
                ticket.claim_error()?;
                let error = view.shapes_error(shapes_graph, error);
                return Err(MutationError::Gate(error.to_string()));
            }
        };
        let Some((report, pending)) = report else {
            return Ok(());
        };
        if report.results.is_empty() && report.failures.is_empty() {
            return Ok(());
        }
        let rejects = matches!(
            gate,
            ShaclGate::Enforce(least)
                if !report.failures.is_empty()
                    || report.results.iter().any(|result| severity(result.severity.as_str()) >= least)
        );
        if rejects {
            ticket.claim_error()?;
        }
        // Formatted only where it is told: a restricted committer's view is told its own.
        let detail = || {
            let first = report.results.first().map_or_else(String::new, |result| {
                format!("; first: {}", crate::shacl::described(result))
            });
            format!(
                "the commit introduces {} SHACL result(s) and {} failure(s){first}",
                report.results.len(),
                report.failures.len()
            )
        };
        if rejects {
            let full = || evidence::Draft {
                kind: RejectKind::ShapesViolated,
                detail: detail(),
                explanation: None,
                attribution: None,
            };
            let reject = view.reject(full, |view| restricted(view, &pending));
            Err(MutationError::Rejected(Box::new(reject)))
        } else {
            tracing::warn!("{} (SHACL gate: not rejected)", detail());
            Ok(())
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
        pending: Option<&nrese_engine::Snapshot>,
        dl: &dyn Fn(&evidence::EvidenceView<'_, '_>) -> Option<evidence::Draft>,
        resources: &nrese_sparql::Resources,
    ) -> evidence::Draft {
        if resources.check().is_err() {
            return evidence::nothing_shown();
        }
        dl(view).unwrap_or_else(|| self.view_reason_after_dl(view, pending, resources))
    }

    /// What a restricted committer is told after the OWL 2 DL reason ([`evidence`]): the
    /// SHACL reason its view shows, where the gate rejects on one, else that its view shows
    /// none.
    fn view_reason_after_dl(
        &self,
        view: &evidence::EvidenceView<'_, '_>,
        pending: Option<&nrese_engine::Snapshot>,
        resources: &nrese_sparql::Resources,
    ) -> evidence::Draft {
        if resources.check().is_err() {
            return evidence::nothing_shown();
        }
        let ShaclGate::Enforce(least) = self.store.config().shacl_gate else {
            return evidence::nothing_shown();
        };
        let rejects =
            |result: &nrese_shacl::ValidationResult| severity(result.severity.as_str()) >= least;
        crate::shacl::view_reason(
            view,
            pending,
            &self.store.config().shapes_graph,
            rejects,
            resources,
        )
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
