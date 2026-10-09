//! Coarse operation checkpoints shared by preparation, island batches and portfolios.
//! Time spent splitting, compiling or queued consumes the caller's remaining timeout.
//! A stage is not preempted: cancellation and exhaustion are checked on either side.

use std::time::{Duration, Instant};

use nrese_exec::workers::Workers;

use super::{Answer, Config, Outcome, ProbeOutcome, Telemetry};

pub(crate) struct RunBudget {
    pub started: Instant,
    timeout: Option<Duration>,
}

impl RunBudget {
    pub fn new(config: &Config) -> Self {
        Self {
            started: Instant::now(),
            timeout: config.timeout,
        }
    }

    fn check_elapsed(&self, config: &Config, elapsed: Duration) -> Result<(), &'static str> {
        if config
            .cancel
            .as_ref()
            .is_some_and(super::Cancel::is_cancelled)
        {
            return Err("cancelled");
        }
        if self.timeout.is_some_and(|limit| elapsed >= limit) {
            return Err("the time budget ran out");
        }
        Ok(())
    }

    pub fn stage<T>(&self, config: &Config, run: impl FnOnce() -> T) -> Result<T, &'static str> {
        self.check_elapsed(config, self.started.elapsed())?;
        let result = run();
        self.check_elapsed(config, self.started.elapsed())?;
        Ok(result)
    }

    pub fn remaining(&self, config: &Config) -> Result<Config, &'static str> {
        self.remaining_at(config, self.started.elapsed())
    }

    fn remaining_at(&self, config: &Config, elapsed: Duration) -> Result<Config, &'static str> {
        self.check_elapsed(config, elapsed)?;
        let mut remaining = config.clone();
        remaining.timeout = self.timeout.map(|limit| limit.saturating_sub(elapsed));
        Ok(remaining)
    }

    pub fn run(&self, config: &Config, run: impl FnOnce(&Config) -> Outcome) -> Outcome {
        match self.remaining(config) {
            Ok(remaining) => run(&remaining),
            Err(why) => self.stopped(why),
        }
    }

    pub fn stopped(&self, why: &str) -> Outcome {
        Outcome {
            answer: Answer::GaveUp(why.into()),
            telemetry: Telemetry {
                total: self.started.elapsed(),
                ..Telemetry::default()
            },
            model: None,
            features: super::Features::default(),
        }
    }

    /// One probe allowance, including setup, labels and any fallback attempts.
    pub(crate) fn probe(
        &self,
        config: &Config,
        run: impl FnOnce(&Config) -> ProbeOutcome,
    ) -> ProbeOutcome {
        let mut out = match self.remaining(config) {
            Ok(remaining) => run(&remaining),
            Err(why) => self.stopped_probe(why),
        };
        if let Err(why) = self.check_elapsed(config, self.started.elapsed()) {
            out.answer = Answer::GaveUp(why.into());
            out.labels = None;
        }
        out.telemetry.total = self.started.elapsed();
        out
    }

    pub(crate) fn stopped_probe(&self, why: &str) -> ProbeOutcome {
        let out = self.stopped(why);
        ProbeOutcome {
            answer: out.answer,
            telemetry: out.telemetry,
            labels: None,
        }
    }
}

/// Finite capacity is divided by concurrent children, never raised to a minimum.
pub(crate) fn memory_share(bytes: usize, concurrent: usize) -> usize {
    match bytes {
        usize::MAX => usize::MAX,
        bytes => bytes / concurrent.max(1),
    }
}

/// Standalone operations create one owner; embedded work only narrows its owner.
pub(crate) fn workers(config: &Config, tasks: usize) -> Workers {
    config.workers.as_ref().map_or_else(
        || {
            let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
            Workers::new(available.min(tasks).max(1)).unwrap_or_else(|_| Workers::serial())
        },
        |workers| workers.limited(tasks.max(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn cancelled_stages_never_start_and_cancellation_during_a_stage_stops_the_next() {
        let cancel = super::super::Cancel::default();
        let config = Config {
            cancel: Some(cancel.clone()),
            ..Config::default()
        };
        let budget = RunBudget::new(&config);
        let stages = Cell::new(0);
        assert_eq!(
            budget.stage(&config, || {
                stages.set(stages.get() + 1);
                cancel.cancel();
            }),
            Err("cancelled")
        );
        assert_eq!(
            budget.stage(&config, || stages.set(stages.get() + 1)),
            Err("cancelled")
        );
        assert_eq!(stages.get(), 1);
    }

    #[test]
    fn elapsed_queue_split_and_compile_time_is_not_a_fresh_child_timeout() {
        let config = Config {
            timeout: Some(Duration::from_secs(10)),
            ..Config::default()
        };
        let budget = RunBudget::new(&config);
        assert_eq!(
            budget
                .remaining_at(&config, Duration::from_secs(3))
                .unwrap()
                .timeout,
            Some(Duration::from_secs(7))
        );
        assert!(
            budget
                .remaining_at(&config, Duration::from_secs(10))
                .is_err()
        );
        assert!(
            budget
                .remaining_at(&config, Duration::from_secs(11))
                .is_err()
        );
        let spent = RunBudget::new(&Config {
            timeout: Some(Duration::ZERO),
            ..config.clone()
        });
        assert!(matches!(
            spent
                .run(&config, |_| panic!("expired child started"))
                .answer,
            Answer::GaveUp(_)
        ));
        assert!(
            spent
                .stage(&config, || panic!("expired stage started"))
                .is_err()
        );
        assert_eq!(
            RunBudget::new(&Config::default())
                .remaining(&Config::default())
                .unwrap()
                .timeout,
            None
        );
    }

    #[test]
    fn memory_shares_preserve_unlimited_and_tiny_finite_limits() {
        assert_eq!(memory_share(usize::MAX, 4), usize::MAX);
        assert_eq!(memory_share(1, 4), 0);
        assert_eq!(memory_share(0, 4), 0);
        assert_eq!(memory_share(101, 4), 25);
    }

    #[test]
    fn probe_fallbacks_share_the_budget_and_preserve_work_when_stopped() {
        let cancel = super::super::Cancel::default();
        let config = Config {
            cancel: Some(cancel.clone()),
            ..Config::default()
        };
        let budget = RunBudget::new(&config);
        let out = budget.probe(&config, |_| {
            cancel.cancel();
            let mut stopped = budget.probe(&config, |_| panic!("cancelled fallback started"));
            stopped.telemetry.nodes_created = 7;
            stopped
        });
        assert!(matches!(out.answer, Answer::GaveUp(_)));
        assert_eq!(out.telemetry.nodes_created, 7);
        assert!(out.labels.is_none());

        let config = Config {
            timeout: Some(Duration::from_secs(2)),
            ..Config::default()
        };
        let budget = RunBudget {
            started: Instant::now() - Duration::from_secs(3),
            timeout: config.timeout,
        };
        assert!(matches!(
            budget
                .probe(&config, |_| panic!("expired fallback restarted"))
                .answer,
            Answer::GaveUp(_)
        ));
        let budget = RunBudget {
            started: Instant::now() - Duration::from_secs(1),
            timeout: config.timeout,
        };
        budget.probe(&config, |remaining| {
            assert!(remaining.timeout.unwrap() <= Duration::from_secs(1));
            budget.stopped_probe("test")
        });
    }

    #[test]
    fn stopped_requests_do_not_return_a_compressed_counted_model() {
        let ontology = crate::numbers::problem::tests::product(2, 3, 6);
        assert!(crate::numbers::model(&ontology).is_some());
        let normalised = nrese_owl::normalise(&ontology);
        let cancel = super::super::Cancel::default();
        cancel.cancel();
        for config in [
            Config {
                cancel: Some(cancel),
                ..Config::default()
            },
            Config {
                timeout: Some(Duration::ZERO),
                ..Config::default()
            },
        ] {
            let outcome = super::super::consistency_of(&ontology, &normalised, &config);
            assert!(matches!(outcome.answer, Answer::GaveUp(_)));
            assert_eq!(outcome.telemetry.compile, Duration::ZERO);
        }
    }
}
