//! What the parts of an operation (island batches, portfolio variants) get of its
//! resources. Its deadline they share unchanged ([`super::Config::deadline`]).

use nrese_exec::workers::Workers;

use super::Config;

/// Finite capacity is divided by concurrent children, never raised to a minimum; none
/// stays none.
pub(crate) fn memory_share(bytes: Option<usize>, concurrent: usize) -> Option<usize> {
    bytes.map(|bytes| bytes / concurrent.max(1))
}

/// Standalone operations create one owner; embedded work only narrows its owner.
pub(crate) fn workers(config: &Config, tasks: usize) -> Workers {
    config.workers.as_ref().map_or_else(
        || {
            let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
            Workers::new(available.min(tasks))
        },
        |workers| workers.limited(tasks.max(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Answer, Cancel};
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_timeout_becomes_one_deadline_that_parts_inherit() {
        let config = Config {
            timeout: Some(Duration::from_secs(10)),
            ..Config::default()
        };
        let before = Instant::now();
        let started = config.started().into_owned();
        assert_eq!(started.timeout, None);
        let deadline = started.deadline.unwrap();
        assert!(deadline >= before + Duration::from_secs(10));
        // A part starting later keeps the operation's deadline, not a fresh timeout.
        let part = Config {
            timeout: Some(Duration::from_secs(10)),
            ..started.clone()
        };
        assert_eq!(part.started().deadline, Some(deadline));
        assert_eq!(Config::default().started().deadline, None);
        let cancel = Cancel::default();
        let cancelled = Config {
            cancel: Some(cancel.clone()),
            ..started
        };
        assert_eq!(cancelled.stopped(), None);
        cancel.cancel();
        assert_eq!(cancelled.stopped(), Some("cancelled"));
        let expired = Config {
            timeout: Some(Duration::ZERO),
            ..Config::default()
        };
        assert_eq!(
            expired.started().stopped(),
            Some(super::super::TIME_BUDGET.as_str())
        );
    }

    #[test]
    fn memory_shares_preserve_unlimited_and_tiny_finite_limits() {
        assert_eq!(memory_share(None, 4), None);
        assert_eq!(memory_share(Some(1), 4), Some(0));
        assert_eq!(memory_share(Some(0), 4), Some(0));
        assert_eq!(memory_share(Some(101), 4), Some(25));
    }

    #[test]
    fn stopped_requests_do_not_return_a_compressed_counted_model() {
        let ontology = crate::numbers::problem::tests::product(2, 3, 6);
        assert!(crate::numbers::model(&ontology).is_some());
        let normalised = nrese_owl::normalise(&ontology);
        let cancel = Cancel::default();
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
