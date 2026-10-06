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

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nrese_dl::tableau::Cancel;
use nrese_engine::Transaction;

use super::consistency::{self, Budget, Checked, Verdict};
use super::{DlConsistency, DlStatus, source};
use crate::StoreService;

/// What the gate decided: the commit's status, and why it is rejected if it is.
pub(crate) struct Gate {
    pub checked: Checked,
    pub reject: Option<String>,
}

/// Runs `check` with a [`Cancel`] that fires when `cancelled` does (a cancelled commit).
fn cancellable(
    cancelled: &(dyn Fn() -> bool + Sync),
    check: impl FnOnce(Cancel) -> Checked,
) -> Checked {
    let cancel = Cancel::default();
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
        let checked = check(cancel);
        done.store(true, Ordering::Release);
        let _ = watcher.join();
        checked
    })
}

/// The budget of a check under `store`'s settings.
pub(crate) fn budget(store: &StoreService, cancel: Option<Cancel>) -> Budget {
    let dl = &store.config().dl;
    Budget {
        timeout: dl.timeout,
        memory_bytes: dl.memory_bytes,
        threads: dl.workers(),
        cancel,
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
) -> Gate {
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
                engine: "upper-bound",
                elapsed: Duration::ZERO,
            },
            reject: None,
        };
    }
    let ontology = match preparation.ontology.take() {
        Some(ontology) => ontology,
        None => {
            source::intern_vocabulary(tx);
            source::read_pending(tx)
        }
    };
    let checked = cancellable(cancelled, |cancel| {
        consistency::check(&ontology, &budget(store, Some(cancel)))
    });
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
        _ => {
            let ontology = source::read_snapshot(tx.base());
            let checked = cancellable(cancelled, |cancel| {
                consistency::check(&ontology, &budget(store, Some(cancel)))
            });
            checked.verdict == Verdict::Inconsistent
        }
    };
    let reject = (!was_inconsistent).then(|| {
        format!(
            "the mutation makes the data inconsistent under OWL 2 DL (found by the {} in {} ms)",
            checked.engine,
            checked.elapsed.as_millis()
        )
    });
    Gate { checked, reject }
}
