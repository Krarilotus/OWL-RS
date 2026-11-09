//! The DL consistency gate on commit (docs/design/owl2-dl.md §8, package 4.3): after the
//! RL gate passed, the ontology the commit leaves is checked under OWL 2 DL, and a commit
//! that makes consistent data inconsistent is rejected as the RL gate rejects.
//!
//! - **Quarantine.** Data that was inconsistent before the commit (loaded or restored
//!   past the gate) doesn't block commits: one that leaves it inconsistent is accepted, so
//!   repairs can take several commits, and the status stays `inconsistent` until one
//!   makes it consistent (as RL's quarantine).
//! - **Undecided.** A check that runs out of its budget (or meets what the engines lack)
//!   accepts the commit and records the status `unknown` with the reason: answers are
//!   then never reported complete. Rejecting would make hard ontologies unwritable.
//!
//! - **Incremental** (docs/design/owl2-dl.md §9). Where the revision before was
//!   consistent and the commit changes assertions only (no statement that may change the
//!   schema: `bounds::changes_schema`), a commit that only removes them leaves it
//!   consistent, unchecked (a subset of a consistent ontology is consistent; engine
//!   `monotone`), and one that adds some is decided on the ABox islands with an individual
//!   it mentions (`touched-islands`); the whole ontology where the ABox doesn't split.
//!
//! The pending-state check, prior-state check and rejection evidence share one deadline
//! and cancellation token. Evidence cannot restart a spent gate's reasoning budget.

use nrese_dl::limits::Deadline;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nrese_dl::tableau::Cancel;
use nrese_engine::Transaction;

use super::consistency::{self, Budget, Checked, Verdict};
use super::{DlConsistency, DlStatus, source};
use crate::StoreService;
use crate::mutation::evidence::{Draft, EvidenceView};

/// What the gate decided: the commit's status, and why it is rejected if it is, for the
/// committer ([`rejection`]).
pub(crate) struct Gate {
    pub checked: Checked,
    pub reject: Option<crate::MutationReject>,
}

/// Runs `check` with a [`Cancel`] that fires when `cancelled` does (a cancelled commit).
pub(super) fn cancellable<T>(
    cancelled: &(dyn Fn() -> bool + Sync),
    check: impl FnOnce(Cancel) -> T,
) -> T {
    let cancel = Cancel::default();
    // A request already cancelled must not race the watcher's first poll.
    if cancelled() {
        cancel.cancel();
    }
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let watcher = {
            let cancel = cancel.clone();
            let done = &done;
            scope.spawn(move || {
                while !done.load(Ordering::Acquire) {
                    if cancelled() {
                        cancel.cancel();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };
        // Also release the watcher if the operation unwinds.
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let finished = Done(&done);
        let checked = check(cancel);
        drop(finished);
        let _ = watcher.join();
        checked
    })
}

/// The budget of a check under `store`'s settings, drawing from the request's envelope
/// `memory`.
pub(crate) fn budget(
    store: &StoreService,
    cancel: Option<Cancel>,
    memory: &nrese_exec::Envelope,
) -> Budget {
    budget_of(&store.config().dl, cancel, memory)
}

/// [`budget`] under the settings `dl`.
fn budget_of(
    dl: &crate::DlConfig,
    cancel: Option<Cancel>,
    memory: &nrese_exec::Envelope,
) -> Budget {
    Budget {
        timeout: dl.timeout,
        memory_bytes: dl.memory_per_worker(1),
        memory: Some(memory.clone()),
        workers: nrese_exec::workers::Workers::current().limited(dl.threads),
        cancel,
        max_nodes: dl.max_nodes,
        max_branch_points: dl.max_branch_points,
        max_merges: dl.max_merges,
        max_join: dl.max_join,
        explanation_tests: dl.explanation_tests,
    }
}

/// Checks the state `tx` would commit. Where U1 after the commit proves it consistent
/// ([`super::bounds::Preparation`]), nothing more runs: the cost of the U1's change.
/// Otherwise O(asserted statements) for reading the ontology (unless the preparation
/// read it), plus the engine's work.
pub(crate) fn check_commit(
    store: &StoreService,
    tx: &Transaction<'_>,
    cancelled: &(dyn Fn() -> bool + Sync),
    preparation: &mut super::bounds::Preparation,
    view: &crate::mutation::evidence::EvidenceView<'_, '_>,
    then: &Then<'_>,
    memory: &nrese_exec::Envelope,
) -> Gate {
    let deadline = Deadline::after(store.config().dl.timeout);
    if store.config().dl.consistency == DlConsistency::Off {
        return Gate {
            checked: Checked {
                verdict: Verdict::Unknown("not checked (dl.consistency = off)".to_owned()),
                engine: "none",
                elapsed: Duration::ZERO,
            },
            reject: None,
        };
    }
    if preparation.proves_consistency {
        return Gate {
            checked: Checked {
                verdict: Verdict::Consistent,
                engine: preparation.decided_by,
                elapsed: Duration::ZERO,
            },
            reject: None,
        };
    }
    cancellable(&|| cancelled() || deadline.passed(), |cancel| {
        check_with_budget(
            store,
            tx,
            preparation,
            view,
            then,
            &budget(store, Some(cancel), memory),
            deadline,
        )
    })
}

/// What the commit changes, where the revision before was consistent and it changes
/// assertions only: the individuals its added statements mention (empty: it only
/// removes). `None` otherwise.
fn assertion_change(store: &StoreService, tx: &Transaction<'_>) -> Option<HashSet<u64>> {
    let before = tx.base().revision();
    match store.dl().status() {
        Some(DlStatus {
            revision,
            consistency,
        }) if revision == before && consistency.verdict == Verdict::Consistent => {}
        _ => return None,
    }
    let mut touched = HashSet::new();
    for quad in tx.deleted() {
        if super::bounds::changes_schema(tx, super::bounds::quad_triple(quad)) {
            return None;
        }
    }
    for quad in tx.inserted() {
        let triple = super::bounds::quad_triple(quad);
        if super::bounds::changes_schema(tx, triple) {
            return None;
        }
        let [s, _, o] = triple;
        touched.insert(s);
        if nrese_engine::TermId::from_raw(o).kind() == nrese_engine::TermKind::Iri {
            touched.insert(o);
        }
    }
    Some(touched)
}

fn check_with_budget(
    store: &StoreService,
    tx: &Transaction<'_>,
    preparation: &mut super::bounds::Preparation,
    view: &crate::mutation::evidence::EvidenceView<'_, '_>,
    then: &Then<'_>,
    operation: &Budget,
    deadline: Deadline,
) -> Gate {
    let started = Instant::now();
    let change = assertion_change(store, tx);
    if change.as_ref().is_some_and(HashSet::is_empty) {
        return Gate {
            checked: Checked {
                verdict: Verdict::Consistent,
                engine: "monotone",
                elapsed: started.elapsed(),
            },
            reject: None,
        };
    }
    // The check's memory: the request's envelope, its reading of the ontology included.
    let memory = operation.memory.clone().unwrap_or_default();
    let ontology = match preparation.ontology.take() {
        Some(ontology) => ontology,
        None => {
            source::intern_vocabulary(tx);
            match source::read_pending(tx, memory.clone()) {
                Ok(ontology) => ontology,
                // Unknown, never consistent: the check didn't run.
                Err(_) => {
                    return Gate {
                        checked: Checked {
                            verdict: Verdict::Unknown(source::refused("the commit")),
                            engine: "none",
                            elapsed: started.elapsed(),
                        },
                        reject: None,
                    };
                }
            }
        }
    };
    let budget = operation.remaining(deadline);
    let checked = change
        .and_then(|touched| consistency::check_after_assertions(&ontology, &touched, &budget))
        .unwrap_or_else(|| consistency::check(&ontology, &budget));
    if checked.verdict != Verdict::Inconsistent {
        return Gate {
            checked,
            reject: None,
        };
    }
    // Inconsistent after the commit: rejected unless it was so before (quarantine).
    let before = tx.base().revision();
    let was_inconsistent = match store.dl().status() {
        Some(DlStatus {
            revision,
            consistency,
        }) if revision == before => consistency.verdict == Verdict::Inconsistent,
        // Unread, as undecided: not known to have been inconsistent, so rejected.
        _ => source::read_snapshot(tx.base(), memory).is_ok_and(|ontology| {
            let checked = consistency::check(&ontology, &operation.remaining(deadline));
            checked.verdict == Verdict::Inconsistent
        }),
    };
    if was_inconsistent {
        return Gate {
            checked,
            reject: None,
        };
    }
    rejection(&ontology, view, then, checked, operation, deadline)
}

/// What a restricted committer is told of a rejection ([`crate::mutation::evidence`]),
/// given the view's OWL 2 DL reason found on the gate's own budget.
pub(crate) type Then<'a> =
    dyn Fn(&EvidenceView<'_, '_>, &dyn Fn(&EvidenceView<'_, '_>) -> Option<Draft>) -> Draft + 'a;

/// The rejection, for the committer. One that may read every graph is told a minimal set of
/// axioms with no model, each by its source triples, and which engine found the data
/// inconsistent in what time. One that may read only some graphs is told what `then` tells
/// of its view, given its view's own OWL 2 DL reason ([`view_reason`]): a set of the axioms
/// of its own graphs that have no model by themselves. The set found over every graph, its
/// size, the engine and the time all
/// depend on statements it may not read.
fn rejection(
    ontology: &nrese_owl::Ontology,
    view: &EvidenceView<'_, '_>,
    then: &Then<'_>,
    checked: Checked,
    operation: &Budget,
    deadline: Deadline,
) -> Gate {
    let full = || {
        // Why: a minimal set of axioms that has no model, each by its source triples. One
        // not decided within the budget leaves the rejection without axioms.
        let justification = super::explain::justify(
            ontology,
            &super::explain::Goal::Inconsistent,
            &[],
            &operation.remaining(deadline),
            deadline,
        )
        .ok()
        .flatten();
        let explanation = match &justification {
            Some(justification) => {
                format!(
                    "{} axiom(s) have no model together",
                    justification.axioms.len()
                )
            }
            None => "rejection evidence was not obtained within the operation's budget".to_owned(),
        };
        let detail = format!(
            "the mutation makes the data inconsistent under OWL 2 DL (found by the {} in {} ms); \
             {explanation}",
            checked.engine,
            checked.elapsed.as_millis()
        );
        let evidence = justification
            .map(|justification| axiom_evidence(view.tx(), ontology, &justification))
            .unwrap_or_default();
        Draft {
            explanation: Some(nrese_reasoner::RejectExplanation {
                summary: detail.clone(),
                violated_constraint: CONSTRAINT.to_owned(),
                focus_resource: String::new(),
                evidence,
            }),
            detail,
            attribution: None,
        }
    };
    let reject = view.reject(full, |view| {
        then(view, &|view| view_reason(view, operation, deadline))
    });
    Gate {
        checked,
        reject: Some(reject),
    }
}

const CONSTRAINT: &str = "owl2-dl-consistency";

/// A justification's axioms, each by its source triples.
fn axiom_evidence(
    tx: &Transaction<'_>,
    ontology: &nrese_owl::Ontology,
    justification: &super::explain::Justification,
) -> Vec<nrese_reasoner::RejectEvidence> {
    let decode = |t: u64| {
        tx.decode(nrese_engine::TermId::from_raw(t))
            .map_or_else(|| format!("#{t}"), |term| term.to_string())
    };
    super::explain::explained(ontology, justification, &decode)
        .into_iter()
        .flat_map(|axiom| axiom.sources)
        .flat_map(|(_, triples)| triples)
        .map(|[s, p, o]| nrese_reasoner::RejectEvidence {
            role: "axiom",
            subject: s,
            predicate: p,
            object: o,
            origin: "asserted".to_owned(),
        })
        .collect()
}

/// The OWL 2 DL reason a restricted committer's view shows: the ontology of the statements
/// asserted in graphs it may read has no model by itself; a minimal set of its axioms, each
/// by its source triples (every one of them from those graphs, whatever graph an axiom's
/// `Source` names). `None` if it has a model, or that isn't known within `operation`.
/// Reading the ontology is charged to the operation's memory, as the gate's own is.
fn view_reason(
    view: &EvidenceView<'_, '_>,
    operation: &Budget,
    deadline: Deadline,
) -> Option<Draft> {
    let memory = operation.memory.clone().unwrap_or_default();
    let ontology = source::read_pending_where(view.tx(), memory, |graph| view.graph(graph)).ok()?;
    let justification = super::explain::justify(
        &ontology,
        &super::explain::Goal::Inconsistent,
        &[],
        &operation.remaining(deadline),
        deadline,
    )
    .ok()??;
    let reason = format!(
        "the data is inconsistent under OWL 2 DL: these {} axiom(s) have no model together",
        justification.axioms.len()
    );
    let evidence = axiom_evidence(view.tx(), &ontology, &justification);
    Some(crate::mutation::evidence::shown(
        reason,
        CONSTRAINT,
        String::new(),
        evidence,
    ))
}

/// [`view_reason`] where the DL gate didn't run (an earlier gate rejected): on a budget of
/// its own, the gate's (a commit that passes the earlier gates spends as much). `None`
/// without the DL gate (`active`, the store's DL mode, and `dl`, its settings).
///
/// It reads and can't write: it is given no store, and the view's transaction only by
/// shared reference ([`EvidenceView::tx`]), which can't commit it (`Transaction::commit`
/// takes it by value). Its one lasting effect is the fixed vocabulary every gate run
/// interns.
pub(crate) fn view_reason_on_its_budget(
    active: bool,
    dl: &crate::DlConfig,
    view: &EvidenceView<'_, '_>,
    cancelled: &(dyn Fn() -> bool + Sync),
    memory: &nrese_exec::Envelope,
) -> Option<Draft> {
    if !active || dl.consistency == DlConsistency::Off {
        return None;
    }
    // The fixed vocabulary every gate run interns (`source::intern_vocabulary`).
    source::intern_vocabulary(view.tx());
    let deadline = Deadline::after(dl.timeout);
    cancellable(&|| cancelled() || deadline.passed(), |cancel| {
        view_reason(view, &budget_of(dl, Some(cancel), memory), deadline)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_checks_share_remaining_time_and_cancellation() {
        let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
        let cancel = Cancel::default();
        let mut base = budget(
            &store,
            Some(cancel.clone()),
            &nrese_exec::Envelope::unlimited(),
        );
        base.timeout = Some(Duration::from_secs(30));
        let now = Instant::now();
        let deadline = Deadline::at(now + Duration::from_secs(10));
        let pending = base.remaining(deadline);
        let prior = pending.remaining(deadline);
        let evidence = prior.remaining(deadline);
        assert!(pending.timeout <= Some(Duration::from_secs(10)));
        assert!(prior.timeout <= pending.timeout && evidence.timeout <= prior.timeout);
        cancel.cancel();
        for phase in [&pending, &prior, &evidence] {
            assert!(phase.cancel.as_ref().unwrap().is_cancelled());
        }
        assert_eq!(
            base.remaining(Deadline::at(now - Duration::from_secs(1)))
                .timeout,
            Some(Duration::ZERO)
        );
        // No time limit and no deadline: none.
        base.timeout = None;
        assert_eq!(base.remaining(Deadline::NONE).timeout, None);
    }

    /// A commit's check reads its ontology within the request's envelope (SYN-61): where it
    /// can't hold the commit's statements (half of them), its verdict is unknown for memory,
    /// never consistent. The commit touches one small island, so the check itself fits: with
    /// the reading outside the envelope, the touched-islands check answered consistent there.
    #[test]
    fn a_gate_that_cannot_read_the_ontology_is_unknown_never_consistent() {
        use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
        use std::sync::Arc;
        let store = Arc::new(StoreService::new(crate::StoreConfig::in_memory()).unwrap());
        let pipeline = crate::MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Dl,
            ))),
        );
        let mut data = String::from(
            "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#> INSERT DATA { \
             :Person owl:equivalentClass [ owl:unionOf ( :Man :Woman ) ] . \
             :Man owl:disjointWith :Woman .",
        );
        for i in 0..20_000 {
            data.push_str(&format!(" :f{i} :q :g{i} ."));
        }
        data.push('}');
        // Committed through the pipeline: U1 is at the base, so the next commit's change is the
        // delta's, which reads no ontology, and the gate reads it.
        pipeline
            .apply(
                crate::MutationCommand::Update(crate::SparqlUpdateRequest::new(data)),
                &crate::Requester::all(),
                &crate::MutationTicket::new(),
            )
            .unwrap();
        let mut tx = store.engine().speculative();
        tx.insert(
            nrese_rdf::Quad::new(
                nrese_rdf::NamedNode::new_unchecked("urn:test:ann"),
                nrese_rdf::vocab::rdf::TYPE,
                nrese_rdf::NamedNode::new_unchecked("urn:test:Person"),
                nrese_rdf::GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        let check = |memory: &nrese_exec::Envelope| {
            let mut prepared =
                super::super::bounds::prepare(&store, &tx, nrese_reasoner::eval::NEVER);
            assert!(!prepared.proves_consistency, "U1 clashes: the gate decides");
            assert!(prepared.ontology.is_none(), "the gate reads the ontology");
            let reader = crate::ReadScope::All;
            let view = EvidenceView::new(&reader, &tx);
            let then: &Then<'_> = &|_, _| crate::mutation::evidence::nothing_shown();
            check_commit(&store, &tx, &|| false, &mut prepared, &view, then, memory).checked
        };
        // Guard: with room, the commit is consistent (Ann is a man or a woman).
        let roomy = check(&nrese_exec::Envelope::unlimited());
        assert_eq!(roomy.verdict, Verdict::Consistent, "{roomy:?}");
        let asserted = tx
            .quads_for_pattern_in(
                nrese_engine::ReadModel::Asserted,
                &nrese_engine::QuadPattern::all(),
            )
            .count();
        let limit = asserted * std::mem::size_of::<nrese_owl::Statement>() / 2;
        let tight = check(&store.envelope(Some(limit)));
        assert!(
            matches!(&tight.verdict, Verdict::Unknown(_)),
            "{limit} bytes for {asserted} statements: {tight:?}"
        );
    }

    #[test]
    fn bridge_observes_an_already_cancelled_request_before_starting_work() {
        cancellable(&|| true, |cancel| assert!(cancel.is_cancelled()));
    }

    #[test]
    fn rejection_evidence_cannot_restart_an_expired_or_cancelled_gate() {
        let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
        let revision = store.current_revision();
        let mut tx = store.engine().speculative();
        let quad = nrese_rdf::Quad::new(
            nrese_rdf::NamedNode::new_unchecked("urn:test:x"),
            nrese_rdf::vocab::rdf::TYPE,
            nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#Nothing"),
            nrese_rdf::GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
        let ontology = source::read_pending(&tx, nrese_exec::Envelope::unlimited()).unwrap();
        for stop in ["deadline", "cancelled", "running"] {
            let cancel = Cancel::default();
            if stop == "cancelled" {
                cancel.cancel();
            }
            let operation = budget(&store, Some(cancel), &nrese_exec::Envelope::unlimited());
            let deadline = if stop == "deadline" {
                Deadline::at(Instant::now() - Duration::from_secs(1))
            } else {
                Deadline::after(operation.timeout)
            };
            let reader = crate::ReadScope::All;
            let gate = rejection(
                &ontology,
                &EvidenceView::new(&reader, &tx),
                &|_, _| crate::mutation::evidence::nothing_shown(),
                Checked {
                    verdict: Verdict::Inconsistent,
                    engine: "context-core",
                    elapsed: Duration::ZERO,
                },
                &operation,
                deadline,
            );
            assert_eq!(gate.checked.verdict, Verdict::Inconsistent);
            let reject = gate.reject.expect("rejected");
            let evidence = reject.explanation.expect("explained").evidence;
            if stop == "running" {
                assert!(
                    !evidence.is_empty(),
                    "the fixture has an explanation with budget"
                );
                assert!(reject.detail.contains("have no model together"));
            } else {
                assert!(evidence.is_empty());
                assert!(reject.detail.contains("not obtained"));
            }
        }
        drop(tx);
        assert_eq!(
            store.current_revision(),
            revision,
            "the rejected transaction published nothing"
        );
    }
}
