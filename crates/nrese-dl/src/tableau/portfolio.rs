//! Runs of one test with different clauses, raced. Which run decides depends on the
//! ontology (lazily unfolded definitions decide the DL98 test k_grz at once and slow k_d4
//! down to a timeout; the plain clauses the other way round).

use nrese_exec::workers::Workers;
use nrese_owl::{Normalised, Ontology};

use super::budget::memory_share;
use super::{Answer, Cancel, Config, Outcome, consistency_of};

/// The consistency of `ontology`, each variant on one worker with an equal share of the
/// memory budget. The first decision cancels the siblings (not the request) and the
/// race waits for them; where several decide, the first in input order is returned.
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
    let stop = Cancel::child(config.cancel.as_ref());
    let width = workers.for_items(variants.len());
    let c = Config {
        cancel: Some(stop.clone()),
        workers: Some(workers.limited(1)),
        max_memory: memory_share(config.max_memory, width),
        ..config.started().into_owned()
    };
    let mut outcomes = workers.map(variants, |normalised| {
        // A queued variant whose sibling decided, or past the deadline, doesn't start.
        let out = match c.stopped() {
            Some(why) => Outcome::stopped(why),
            None => run(ontology, normalised, &c),
        };
        if matches!(out.answer, Answer::Consistent | Answer::Inconsistent) {
            stop.cancel();
        }
        out
    });
    let decided = outcomes
        .iter()
        .position(|out| matches!(out.answer, Answer::Consistent | Answer::Inconsistent));
    if outcomes.is_empty() {
        Outcome::stopped("no portfolio variants")
    } else {
        outcomes.swap_remove(decided.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_decided_variant_stops_a_queued_sibling_before_its_solver_starts() {
        use std::sync::atomic::AtomicUsize;
        let workers = Workers::new(1);
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
                    let mut out = Outcome::stopped("unused");
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
        let workers = Workers::new(2);
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
