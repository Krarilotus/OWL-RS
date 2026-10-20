//! Stopping long work at bounded checkpoints (the admission design's §2, W1 step 1).
//!
//! A request has one [`StopSource`]: its cancellation flag and, optionally, a deadline. Each
//! sequential loop or parallel task takes a [`Ticker`] from it and ticks the units of work it
//! does; every [`POLL_UNITS`] units the ticker polls the source, so interruptible work reaches
//! a checkpoint within that many units per executing task. Opaque calls (a decode, a regex
//! search) are checked before and after ([`Ticker::check`]): after a stop, at most the call
//! already running finishes.
//!
//! The common path of a tick is a subtraction and a predictable branch on a local count; the
//! poll itself (one relaxed load, and the clock only with a deadline) is out of line.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::budget::BudgetExceeded;
use crate::narrow::RepresentationLimit;

/// The units of work between two polls of a ticker.
pub const POLL_UNITS: u32 = 4096;

/// Why work stopped: a refused charge, a row limit, cancellation (a passed deadline too), or
/// a value past what its representation can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Memory(BudgetExceeded),
    Rows,
    Cancelled,
    TooLarge(RepresentationLimit),
}

impl From<BudgetExceeded> for Stop {
    fn from(exceeded: BudgetExceeded) -> Self {
        Self::Memory(exceeded)
    }
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(exceeded) => exceeded.fmt(f),
            Self::Rows => f.write_str("more rows than the limit allows"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::TooLarge(limit) => limit.fmt(f),
        }
    }
}

impl std::error::Error for Stop {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Memory(exceeded) => Some(exceeded),
            Self::TooLarge(limit) => Some(limit),
            Self::Rows | Self::Cancelled => None,
        }
    }
}

/// A request's stop condition: its cancellation flag and deadline, either optional.
/// Immutable; a clone shares the flag.
#[derive(Debug, Clone, Default)]
pub struct StopSource {
    flag: Option<Arc<AtomicBool>>,
    deadline: Option<Instant>,
}

impl StopSource {
    pub fn new(flag: Option<Arc<AtomicBool>>, deadline: Option<Instant>) -> Self {
        Self { flag, deadline }
    }

    /// Never stops.
    pub fn none() -> Self {
        Self::default()
    }

    /// A ticker for one loop or task, after checking at once.
    pub fn ticker(&self) -> Result<Ticker, Stop> {
        self.check()?;
        Ok(Ticker {
            source: self.clone(),
            left: POLL_UNITS,
            not_sync: PhantomData,
        })
    }

    /// [`Stop::Cancelled`] once the flag is set or the deadline has passed.
    pub fn check(&self) -> Result<(), Stop> {
        let cancelled = self
            .flag
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed));
        if cancelled
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Err(Stop::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// The countdown of one sequential loop or parallel task: it polls its [`StopSource`] every
/// [`POLL_UNITS`] units ticked. `Send`, not `Sync`: each task has its own.
#[derive(Debug)]
pub struct Ticker {
    source: StopSource,
    left: u32,
    not_sync: PhantomData<Cell<()>>,
}

impl Ticker {
    /// Counts `units` of work done; polls when the allowance runs out, then starts a new
    /// one. A tick past several intervals polls once.
    #[inline]
    pub fn tick(&mut self, units: u32) -> Result<(), Stop> {
        self.left = self.left.saturating_sub(units);
        if self.left == 0 {
            return self.poll();
        }
        Ok(())
    }

    /// Checks now, whatever is left: before and after an opaque call.
    pub fn check(&mut self) -> Result<(), Stop> {
        self.poll()
    }

    /// The units left before the next poll, for loops that work in chunks of at most that.
    pub fn left(&self) -> u32 {
        self.left
    }

    /// The closing check of a short task.
    pub fn finish(&mut self) -> Result<(), Stop> {
        self.poll()
    }

    #[cold]
    #[inline(never)]
    fn poll(&mut self) -> Result<(), Stop> {
        self.left = POLL_UNITS;
        self.source.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagged() -> (Arc<AtomicBool>, StopSource) {
        let flag = Arc::new(AtomicBool::new(false));
        (Arc::clone(&flag), StopSource::new(Some(flag), None))
    }

    /// A loop of one unit per tick sees a stop within `POLL_UNITS` units, wherever the stop
    /// falls in the interval; a tick of several intervals polls once.
    #[test]
    fn a_ticker_polls_within_its_interval_and_once_per_tick() {
        for stop_at in [1, 1000, 4095, 4096, 4097, 10_000] {
            let (flag, source) = flagged();
            let mut ticker = source.ticker().unwrap();
            let mut units = 0u32;
            let after = loop {
                if units == stop_at {
                    flag.store(true, Ordering::Relaxed);
                }
                units += 1;
                if ticker.tick(1).is_err() {
                    break units - stop_at;
                }
            };
            assert!(
                after <= POLL_UNITS,
                "stop at {stop_at}: {after} units after it"
            );
        }
        let (flag, source) = flagged();
        let mut ticker = source.ticker().unwrap();
        assert_eq!(ticker.tick(3 * POLL_UNITS), Ok(()));
        assert_eq!(
            ticker.left(),
            POLL_UNITS,
            "one poll, then a fresh allowance"
        );
        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            ticker.tick(1),
            Ok(()),
            "not polled before the allowance runs out"
        );
    }

    /// `ticker()` and `check()` check at once, and a deadline already passed stops the
    /// first poll.
    #[test]
    fn checks_are_immediate_and_a_past_deadline_stops() {
        let (flag, source) = flagged();
        let mut ticker = source.ticker().unwrap();
        flag.store(true, Ordering::Relaxed);
        assert_eq!(ticker.check(), Err(Stop::Cancelled));
        assert_eq!(ticker.finish(), Err(Stop::Cancelled));
        assert!(matches!(source.ticker(), Err(Stop::Cancelled)));
        assert_eq!(source.check(), Err(Stop::Cancelled));

        let past = StopSource::new(None, Some(Instant::now()));
        assert!(matches!(past.ticker(), Err(Stop::Cancelled)));
        assert_eq!(StopSource::none().check(), Ok(()));
    }

    #[test]
    fn a_refused_charge_is_a_memory_stop() {
        let exceeded = BudgetExceeded {
            limit: 10,
            requested: 20,
            used: 0,
            shared: false,
        };
        assert_eq!(Stop::from(exceeded), Stop::Memory(exceeded));
    }
}
