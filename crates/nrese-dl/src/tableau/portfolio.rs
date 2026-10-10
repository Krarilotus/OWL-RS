//! Runs of one test with different clauses, raced: a decision signals sibling cancellation
//! immediately, then dispatch waits for all runs to finish. Several runs may decide before
//! observing cancellation; the returned decided outcome is first in input order, so its
//! telemetry does not identify the earliest wall-clock decision. Which run decides depends
//! on the ontology (lazily unfolded definitions decide the DL98 test k_grz at once and slow
//! k_d4 down to a timeout; the plain clauses the other way round).

use nrese_exec::workers::Workers;
use nrese_owl::{Normalised, Ontology};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::budget::{RunBudget, memory_share};
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

/// The consistency of `ontology`, each variant run on a shared worker with an equal
/// share of the capacity budget. A decision signals sibling cancellation immediately;
/// scope completion waits for every run and releases all sibling scratch. If several
/// decide, return the first decided outcome in input order, not wall-clock order.
pub fn race(
    ontology: &Ontology,
    variants: &[&Normalised],
    config: &Config,
    workers: &Workers,
) -> Outcome {
    race_with(ontology, variants, config, workers, consistency_of)
}

fn race_with(
    ontology: &Ontology,
    variants: &[&Normalised],
    config: &Config,
    workers: &Workers,
    run: impl Fn(&Ontology, &Normalised, &Config) -> Outcome + Sync + Send,
) -> Outcome {
    let budget = RunBudget::new(config);
    let stop = Cancel::child(config.cancel.as_ref());
    let width = workers.for_items(variants.len());
    let mut outcomes = workers.map(variants, |normalised| {
        let mut c = config.clone();
        c.cancel = Some(stop.clone());
        c.workers = Some(workers.limited(1));
        c.max_memory = memory_share(config.max_memory, width);
        // This executes after admission: a queued loser must not start compiling,
        // and its wait consumes the same timeout as the winning/parent operation.
        let out = budget.run(&c, |remaining| run(ontology, normalised, remaining));
        if matches!(out.answer, Answer::Consistent | Answer::Inconsistent) {
            stop.cancel();
        }
        out
    });
    let decided = outcomes
        .iter()
        .position(|out| matches!(out.answer, Answer::Consistent | Answer::Inconsistent));
    if outcomes.is_empty() {
        budget.stopped("no portfolio variants")
    } else {
        outcomes.swap_remove(decided.unwrap_or(0))
    }
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

    #[test]
    fn a_decided_variant_stops_a_queued_sibling_before_its_solver_starts() {
        use std::sync::atomic::AtomicUsize;
        let workers = Workers::pooled(1).unwrap();
        let request = Cancel::default();
        let config = Config {
            cancel: Some(request.clone()),
            ..Config::default()
        };
        let ontology = Ontology::default();
        let normalised = Normalised::default();
        let calls = AtomicUsize::new(0);
        let out = workers.install(|| {
            race_with(
                &ontology,
                &[&normalised, &normalised],
                &config,
                &workers,
                |_, _, child| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    assert_eq!(child.workers.as_ref().unwrap().width(), 1);
                    let mut out = RunBudget::new(child).stopped("unused");
                    out.answer = Answer::Consistent;
                    out
                },
            )
        });
        assert_eq!(out.answer, Answer::Consistent);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(!request.is_cancelled());
    }

    #[test]
    fn cancelled_or_expired_portfolios_start_no_solvers() {
        use std::time::Duration;
        let workers = Workers::pooled(2).unwrap();
        let request = Cancel::default();
        request.cancel();
        let ontology = Ontology::default();
        let normalised = Normalised::default();
        for config in [
            Config {
                cancel: Some(request),
                ..Config::default()
            },
            Config {
                timeout: Some(Duration::ZERO),
                ..Config::default()
            },
        ] {
            let out = race_with(
                &ontology,
                &[&normalised, &normalised],
                &config,
                &workers,
                |_, _, _| panic!("stopped variant entered its solver"),
            );
            assert!(matches!(out.answer, Answer::GaveUp(_)));
        }
    }
}
