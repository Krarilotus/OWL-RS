//! Runs of one test with different clauses, raced: the first decided answer wins and the
//! others are cancelled. Both answers are right; which run decides first depends on the
//! ontology (lazily unfolded definitions decide the DL98 test k_grz at once and slow
//! k_d4 down to a timeout; the plain clauses the other way round).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use nrese_owl::{Normalised, Ontology};

use super::{Answer, Config, Outcome, consistency_of};

/// A flag that stops a run at its next budget check ([`Config::cancel`]).
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// Shares a caller's cancellation flag without a polling thread.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl PartialEq for Cancel {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Cancel {}

/// Whether two runs can go side by side.
pub fn cores_to_spare() -> bool {
    std::thread::available_parallelism().is_ok_and(|n| n.get() >= 2)
}

/// The consistency of `ontology` by whichever of `variants` decides first, each run on a
/// thread of its own with an equal share of the memory budget.
pub fn race(ontology: &Ontology, variants: &[&Normalised], config: &Config) -> Outcome {
    let stop = Cancel::default();
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for (i, normalised) in variants.iter().enumerate() {
            let mut c = config.clone();
            c.cancel = Some(stop.clone());
            c.max_memory = config.max_memory / variants.len();
            let tx = tx.clone();
            scope.spawn(move || {
                let _ = tx.send((i, consistency_of(ontology, normalised, &c)));
            });
        }
        drop(tx);
        let mut undecided: Option<Outcome> = None;
        let mut left = variants.len();
        while left > 0 {
            // The caller's flag reaches the runs through ours.
            let (_, out) = match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(r) => r,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if config.cancel.as_ref().is_some_and(Cancel::is_cancelled) {
                        stop.cancel();
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            left -= 1;
            if matches!(out.answer, Answer::Consistent | Answer::Inconsistent) {
                stop.cancel();
                return out;
            }
            if undecided.is_none() {
                undecided = Some(out);
            }
        }
        undecided.unwrap_or_else(|| unreachable!("every run sends its outcome"))
    })
}
