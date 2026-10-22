//! Stopping long work at bounded checkpoints (the admission design's Â§2, W1 step 1).
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
        Ticker::new(self.clone())
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

/// What a [`Ticker`] polls: a [`StopSource`], or the caller's own stop condition.
pub trait StopCheck {
    /// The stop, if there is one now.
    fn check(&self) -> Result<(), Stop>;
}

impl StopCheck for StopSource {
    fn check(&self) -> Result<(), Stop> {
        StopSource::check(self)
    }
}

/// A caller's own stop condition, a closure that turns true to stop: [`Stop::Cancelled`],
/// meaning stopped for a reason the closure's owner tells (the reasoner's stop combines its
/// caller's cancellation with the process's memory limit, and reports which itself).
impl<F: Fn() -> bool + Sync + ?Sized> StopCheck for &F {
    fn check(&self) -> Result<(), Stop> {
        if self() { Err(Stop::Cancelled) } else { Ok(()) }
    }
}

/// The countdown of one sequential loop or parallel task: it polls its [`StopCheck`] every
/// [`POLL_UNITS`] units ticked. `Send`, not `Sync`: each task has its own.
pub struct Ticker<C: StopCheck = StopSource> {
    source: C,
    left: u32,
    not_sync: PhantomData<Cell<()>>,
}

impl<C: StopCheck> std::fmt::Debug for Ticker<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticker").field("left", &self.left).finish()
    }
}

impl<C: StopCheck> Ticker<C> {
    /// A ticker polling `check`, after checking at once.
    pub fn new(check: C) -> Result<Self, Stop> {
        check.check()?;
        Ok(Self {
            source: check,
            left: POLL_UNITS,
            not_sync: PhantomData,
        })
    }

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
        StopCheck::check(&self.source)
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
            let mut after = None;
            for units in 1..=stop_at + POLL_UNITS {
                if units - 1 == stop_at {
                    flag.store(true, Ordering::Relaxed);
                }
                if let Err(stop) = ticker.tick(1) {
                    assert_eq!(stop, Stop::Cancelled);
                    after = Some(units - stop_at);
                    break;
                }
            }
            let after = after.expect("periodic poll must stop within the bounded attempts");
            assert!(
                after <= POLL_UNITS,
                "stop at {stop_at}: {after} units after it"
            );
        }
        // Count actual source calls, not just the final allowance: several polls could
        // otherwise reset to the same value and escape the guard.
        struct Counted<'a>(&'a StopSource, &'a std::sync::atomic::AtomicUsize);
        impl StopCheck for Counted<'_> {
            fn check(&self) -> Result<(), Stop> {
                self.1.fetch_add(1, Ordering::Relaxed);
                self.0.check()
            }
        }
        for units in [3 * POLL_UNITS, u32::MAX] {
            for stopped in [false, true] {
                let (flag, source) = flagged();
                let calls = std::sync::atomic::AtomicUsize::new(0);
                let mut ticker = Ticker::new(Counted(&source, &calls)).unwrap();
                assert_eq!(calls.load(Ordering::Relaxed), 1, "constructor checks once");
                ticker.tick(POLL_UNITS - 1).unwrap();
                assert_eq!(calls.load(Ordering::Relaxed), 1, "no early source call");
                flag.store(stopped, Ordering::Relaxed);
                let expected = if stopped {
                    Err(Stop::Cancelled)
                } else {
                    Ok(())
                };
                assert_eq!(ticker.tick(units), expected);
                assert_eq!(
                    calls.load(Ordering::Relaxed),
                    2,
                    "oversized tick polls once"
                );
                assert_eq!(ticker.left(), POLL_UNITS);
                assert_eq!(ticker.tick(units), expected);
                assert_eq!(
                    calls.load(Ordering::Relaxed),
                    3,
                    "already-stopped oversized tick polls once"
                );
            }
        }
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

    /// A ticker over a closure stops within `POLL_UNITS` units of the closure turning true;
    /// `Ticker::new` checks at once; true is `Stop::Cancelled`.
    #[test]
    fn a_closure_backed_ticker_polls_the_closure() {
        let stopped = AtomicBool::new(false);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let check = || {
            calls.fetch_add(1, Ordering::Relaxed);
            stopped.load(Ordering::Relaxed)
        };
        let closure: &(dyn Fn() -> bool + Sync) = &check;
        assert_eq!(StopCheck::check(&closure), Ok(()));
        let mut ticker = Ticker::new(closure).unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "direct and constructor checks"
        );
        for _ in 0..1000 {
            ticker.tick(1).unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 2, "no pre-checkpoint call");
        stopped.store(true, Ordering::Relaxed);
        let mut after = None;
        for units in 1..=POLL_UNITS {
            if let Err(stop) = ticker.tick(1) {
                assert_eq!(stop, Stop::Cancelled, "periodic error retains its reason");
                after = Some(units);
                break;
            }
        }
        assert_eq!(after, Some(POLL_UNITS - 1000), "bounded periodic stop");
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        stopped.store(false, Ordering::Relaxed);
        assert_eq!(ticker.tick(u32::MAX), Ok(()));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            4,
            "oversized closure tick polls once"
        );
        stopped.store(true, Ordering::Relaxed);
        assert_eq!(ticker.tick(u32::MAX), Err(Stop::Cancelled));
        assert_eq!(calls.load(Ordering::Relaxed), 5);
        assert_eq!(ticker.tick(u32::MAX), Err(Stop::Cancelled));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            6,
            "already-stopped oversized tick polls once"
        );
        assert_eq!(StopCheck::check(&closure), Err(Stop::Cancelled));
        assert!(matches!(Ticker::new(closure), Err(Stop::Cancelled)));
    }
}
