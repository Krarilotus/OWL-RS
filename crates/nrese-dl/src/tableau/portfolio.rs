//! Runs of one test with different clauses, raced: the first decided answer wins and the
//! others are cancelled. Both answers are right; which run decides first depends on the
//! ontology (lazily unfolded definitions decide the DL98 test k_grz at once and slow
//! k_d4 down to a timeout; the plain clauses the other way round).

use nrese_exec::workers::Workers;
use nrese_owl::{Normalised, Ontology};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Answer, Config, Outcome, consistency_of};

/// A flag that stops a run at its next budget check ([`Config::cancel`]).
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<Cancel>>,
}

impl Cancel {
    /// Shares a caller's cancellation flag without a polling thread.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self { flag, parent: None }
    }

    /// A child may stop its siblings without cancelling the request. Parent changes
    /// are observed at the same checkpoints, without a polling thread.
    fn child(parent: Option<&Self>) -> Self {
        Self {
            flag: Arc::default(),
            parent: parent.cloned().map(Arc::new),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
    }
}

impl PartialEq for Cancel {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.flag, &other.flag)
    }
}

impl Eq for Cancel {}

/// Standalone calls use at most one worker per variant. An embedded call cannot
/// enlarge its allowance or escape to another pool when the allowance is exhausted.
pub fn workers(config: &Config, variants: usize) -> Workers {
    config.workers.as_ref().map_or_else(
        || {
            let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
            Workers::new(available.min(variants).max(1)).unwrap_or_else(|_| Workers::serial())
        },
        |workers| workers.limited(variants),
    )
}

/// The consistency of `ontology` by whichever of `variants` decides first, each run on a
/// shared worker with an equal share of the capacity budget. The first decision
/// cancels siblings immediately; scope completion releases all sibling scratch.
pub fn race(
    ontology: &Ontology,
    variants: &[&Normalised],
    config: &Config,
    workers: &Workers,
) -> Outcome {
    let stop = Cancel::child(config.cancel.as_ref());
    let width = workers.for_items(variants.len());
    let mut outcomes = workers.map(variants, |normalised| {
        let mut c = config.clone();
        c.cancel = Some(stop.clone());
        c.workers = Some(workers.limited(1));
        c.max_memory = match config.max_memory {
            usize::MAX => usize::MAX,
            bytes => bytes / width,
        };
        let out = consistency_of(ontology, normalised, &c);
        if matches!(out.answer, Answer::Consistent | Answer::Inconsistent) {
            stop.cancel();
        }
        out
    });
    let decided = outcomes
        .iter()
        .position(|out| matches!(out.answer, Answer::Consistent | Answer::Inconsistent));
    outcomes.swap_remove(decided.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_stop_does_not_cancel_the_request_but_parent_cancellation_reaches_descendants() {
        let request = Cancel::default();
        let winner = Cancel::child(Some(&request));
        let sibling = winner.clone();
        winner.cancel();
        assert!(sibling.is_cancelled());
        assert!(!request.is_cancelled());
        let child = Cancel::child(Some(&request));
        let nested = Cancel::child(Some(&child));
        request.cancel();
        assert!(nested.is_cancelled());
    }
}
