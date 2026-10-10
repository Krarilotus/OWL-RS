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
//! The pending-state check, prior-state check and rejection evidence share one deadline
//! and cancellation token. Evidence cannot restart a spent gate's reasoning budget.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nrese_dl::tableau::Cancel;
use nrese_engine::Transaction;

use super::consistency::{self, Budget, Checked, Verdict};
use super::{DlConsistency, DlStatus, source};
use crate::StoreService;

/// What the gate decided: the commit's status, and why it is rejected if it is.
pub(crate) struct Gate {
    pub checked: Checked,
    pub reject: Option<String>,
    /// For a rejection: a known-inconsistent set of axioms, minimal when the budget
    /// permits, each by its source triples ([`super::explain`]).
    pub evidence: Vec<nrese_reasoner::RejectEvidence>,
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

/// A nested check spends the operation's remaining time, never a fresh full timeout.
pub(super) fn remaining_budget(base: &Budget, deadline: Instant) -> Budget {
    let mut remaining = base.clone();
    remaining.timeout = base
        .timeout
        .min(deadline.saturating_duration_since(Instant::now()));
    remaining
}

/// The budget of a check under `store`'s settings.
pub(crate) fn budget(store: &StoreService, cancel: Option<Cancel>) -> Budget {
    let dl = &store.config().dl;
    let workers = store.runtime().workers().limited(dl.threads);
    Budget {
        timeout: dl.timeout,
        memory_bytes: dl.memory_per_worker(1),
        threads: workers.width(),
        workers: Some(workers),
        cancel,
        max_nodes: dl.max_nodes,
        max_branch_points: dl.max_branch_points,
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
    reader: &crate::ReadScope,
) -> Gate {
    let deadline = Instant::now() + store.config().dl.timeout;
    if store.config().dl.consistency == DlConsistency::Off {
        return Gate {
            checked: Checked {
                verdict: Verdict::Unknown("not checked (dl.consistency = off)".to_owned()),
                engine: "none",
                elapsed: Duration::ZERO,
            },
            reject: None,
            evidence: Vec::new(),
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
            evidence: Vec::new(),
        };
    }
    cancellable(&|| cancelled() || Instant::now() >= deadline, |cancel| {
        check_with_budget(
            store,
            tx,
            preparation,
            reader,
            &budget(store, Some(cancel)),
            deadline,
        )
    })
}

fn check_with_budget(
    store: &StoreService,
    tx: &Transaction<'_>,
    preparation: &mut super::bounds::Preparation,
    reader: &crate::ReadScope,
    operation: &Budget,
    deadline: Instant,
) -> Gate {
    let ontology = match preparation.ontology.take() {
        Some(ontology) => ontology,
        None => {
            source::intern_vocabulary(tx);
            source::read_pending(tx)
        }
    };
    let checked = consistency::check(&ontology, &remaining_budget(operation, deadline));
    if checked.verdict != Verdict::Inconsistent {
        return Gate {
            checked,
            reject: None,
            evidence: Vec::new(),
        };
    }
    // Inconsistent after the commit: rejected unless it was so before (quarantine).
    let before = tx.base().revision();
    let was_inconsistent = match store.dl().status() {
        Some(DlStatus {
            revision,
            consistency,
        }) if revision == before => consistency.verdict == Verdict::Inconsistent,
        _ => {
            let ontology = source::read_snapshot(tx.base());
            let checked = consistency::check(&ontology, &remaining_budget(operation, deadline));
            checked.verdict == Verdict::Inconsistent
        }
    };
    if was_inconsistent {
        return Gate {
            checked,
            reject: None,
            evidence: Vec::new(),
        };
    }
    rejection(tx, &ontology, reader, checked, operation, deadline)
}

fn rejection(
    tx: &Transaction<'_>,
    ontology: &nrese_owl::Ontology,
    reader: &crate::ReadScope,
    checked: Checked,
    operation: &Budget,
    deadline: Instant,
) -> Gate {
    // Why: a minimal set of axioms that has no model, each by its source triples.
    let justification = super::explain::justify(
        ontology,
        &super::explain::Goal::Inconsistent,
        &[],
        &remaining_budget(operation, deadline),
        deadline,
    );
    let decode = |t: u64| {
        tx.decode(nrese_engine::TermId::from_raw(t))
            .map_or_else(|| format!("#{t}"), |term| term.to_string())
    };
    // The committer sees the axioms of the graphs it may read; the others are withheld,
    // and counted (graph access: nothing from a graph a reader can't read reaches it).
    let readable = |graph: &Option<String>| match reader.access() {
        None => true,
        Some(access) => match graph {
            None => access.default_graph,
            // As the source gives it: `<iri>`.
            Some(graph) => access.allows(graph.trim_start_matches('<').trim_end_matches('>')),
        },
    };
    let mut withheld = 0usize;
    let mut evidence: Vec<nrese_reasoner::RejectEvidence> = Vec::new();
    for axiom in justification
        .iter()
        .flat_map(|j| super::explain::explained(ontology, j, &decode))
    {
        for (graph, triples) in axiom.sources {
            if !readable(&graph) {
                withheld += triples.len();
                continue;
            }
            evidence.extend(
                triples
                    .into_iter()
                    .map(|[s, p, o]| nrese_reasoner::RejectEvidence {
                        role: "axiom",
                        subject: s,
                        predicate: p,
                        object: o,
                        origin: "asserted".to_owned(),
                    }),
            );
        }
    }
    let axioms = justification.as_ref().map_or(0, |j| j.axioms.len());
    let hidden = match withheld {
        0 => String::new(),
        n => format!(" ({n} of their statements are in graphs you can't read, not shown)"),
    };
    let explanation = match &justification {
        Some(_) => format!("{axioms} axiom(s) have no model together{hidden}"),
        None => "rejection evidence was not obtained within the operation's budget".to_owned(),
    };
    let reject = Some(format!(
        "the mutation makes the data inconsistent under OWL 2 DL (found by the {} in {} ms); \
         {explanation}",
        checked.engine,
        checked.elapsed.as_millis()
    ));
    Gate {
        checked,
        reject,
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_checks_share_remaining_time_and_cancellation() {
        let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
        let cancel = Cancel::default();
        let mut base = budget(&store, Some(cancel.clone()));
        base.timeout = Duration::from_secs(30);
        let now = Instant::now();
        let deadline = now + Duration::from_secs(10);
        let pending = remaining_budget(&base, deadline);
        let prior = remaining_budget(&pending, deadline);
        let evidence = remaining_budget(&prior, deadline);
        assert!(pending.timeout <= Duration::from_secs(10));
        assert!(prior.timeout <= pending.timeout && evidence.timeout <= prior.timeout);
        cancel.cancel();
        for phase in [&pending, &prior, &evidence] {
            assert!(phase.cancel.as_ref().unwrap().is_cancelled());
        }
        assert_eq!(
            remaining_budget(&base, now - Duration::from_secs(1)).timeout,
            Duration::ZERO
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
        let ontology = source::read_pending(&tx);
        for stop in ["deadline", "cancelled", "running"] {
            let cancel = Cancel::default();
            if stop == "cancelled" {
                cancel.cancel();
            }
            let operation = budget(&store, Some(cancel));
            let deadline = if stop == "deadline" {
                Instant::now() - Duration::from_secs(1)
            } else {
                Instant::now() + operation.timeout
            };
            let gate = rejection(
                &tx,
                &ontology,
                &crate::ReadScope::All,
                Checked {
                    verdict: Verdict::Inconsistent,
                    engine: "context-core",
                    elapsed: Duration::ZERO,
                },
                &operation,
                deadline,
            );
            assert_eq!(gate.checked.verdict, Verdict::Inconsistent);
            if stop == "running" {
                assert!(
                    !gate.evidence.is_empty(),
                    "the fixture has an explanation with budget"
                );
                assert!(gate.reject.unwrap().contains("have no model together"));
            } else {
                assert!(gate.evidence.is_empty());
                assert!(gate.reject.unwrap().contains("not obtained"));
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
