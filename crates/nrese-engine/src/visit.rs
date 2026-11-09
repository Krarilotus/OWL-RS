//! Reads its reader's stop sees: the contract of the engine's counted reads
//! ([`crate::CountedSnapshot`]; REAS-3, design note `2026-11-09-p2-reas3-design.md` §1).
//!
//! A counted read takes a **visitor**, `&dyn Fn(u64) -> bool`, and calls `visit(n)` before
//! each `n` units of work it does, `n` at most [`nrese_exec::POLL_UNITS`]. A unit is O(1) work
//! on one item. The units are:
//! - a key comparison a run's search makes, or a merge's heap;
//! - a cursor a merge advances, and each key it reads or decodes;
//! - a run a read looks at, and a stored position it passes (deleted and filtered ones too);
//! - a copy an equality read makes;
//! - a `sameAs` statement or identity an equality class build reads.
//!
//! Work is counted a portion at a time ([`Account`]), each portion at most [`PORTION`] units
//! (fix check r1 F1). Once `visit` answers false
//! the read does no more work and answers [`Stopped`]: never a value, never the end of the
//! data. `visit(0)` asks whether the reader has stopped without counting anything, and a
//! visitor that answered false keeps answering false: a reader's poller does both
//! (`nrese_reasoner::eval::Poller`).
//!
//! So after a reader's stop fires, a counted read does at most the units its visitor last
//! allowed, at most `POLL_UNITS`, then stops at its next call (REAS-2's invariant, one level
//! down). Work too large for that is done in portions, each counted before it: a union's
//! member moves (`nrese_exec::classes::Classes::union_asking`), a class build's sort (runs
//! sorted alone, then merged a chunk at a time) and its copies and indexing.
//!
//! Held for the owner in REAS-3a (fix check r1 F2): allocating or releasing memory in bulk is
//! one allocator call, or one pass over what is released, that is not counted. That covers a
//! class build's maps at their final size, a union's list or map growing, a merge's heap or
//! cursor list growing, and dropping the cursors of a scan stopped while it was being made
//! (design attack r2 E1).

/// A read its visitor stopped: an error its caller has to handle, never an answer (REAS-2
/// fix check r1, rule 2; one type for the engine and the reasoner).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

/// A counted read's visitor ([module docs](self)).
pub type Visitor<'v> = &'v dyn Fn(u64) -> bool;

/// Counts `n` units of work on `visit` before they are done, in runs of at most
/// `POLL_UNITS`: false once `visit` has stopped.
#[inline]
pub(crate) fn units(visit: Visitor<'_>, n: u64) -> bool {
    let run = u64::from(nrese_exec::POLL_UNITS);
    if n <= run {
        return visit(n);
    }
    let mut left = n;
    while left > 0 {
        let now = left.min(run);
        if !visit(now) {
            return false;
        }
        left -= now;
    }
    true
}

/// Whether `visit` has stopped, counting nothing.
#[inline]
pub(crate) fn stopped(visit: Visitor<'_>) -> bool {
    !visit(0)
}

/// The most units one portion of a read's work counts: a search's bound, a cursor's advance
/// with the block it decodes, a merge's heap repair (REAS-3a fix check r1 F1).
pub(crate) const PORTION: u64 = 256;

/// Where a read counts its work, a portion at a time, each before it is done: the plain reads
/// count nothing ([`Free`]), the counted ones on their visitor ([`Ledger`]). One body of a
/// read serves both (REAS-3a fix check r1 F1, design attack D3).
pub(crate) trait Account {
    /// Counts `n` units, at most [`PORTION`], before they are done: false once the read's
    /// visitor has stopped.
    fn take(&mut self, n: u64) -> bool;
}

/// The plain reads' account: counts nothing, never stops.
pub(crate) struct Free;

impl Account for Free {
    #[inline(always)]
    fn take(&mut self, _: u64) -> bool {
        true
    }
}

/// A visitor as an account: each portion asked of it alone, for reads that ask their visitor
/// between portions anyway (a count's tombstone words).
impl Account for Visitor<'_> {
    #[inline]
    fn take(&mut self, n: u64) -> bool {
        debug_assert!(n <= PORTION, "a portion of {n} units: at most {PORTION}");
        (*self)(n)
    }
}

/// A counted read's account: portions taken from a run its visitor was asked for. A run
/// is twice the one before, at least the portion and at most `POLL_UNITS`, so a short read
/// asks for little and a long one for full runs. A portion that doesn't fit what a run has
/// left asks for a new run, and the rest is dropped: after a yes, at most `POLL_UNITS` units
/// follow.
pub(crate) struct Ledger<'v> {
    visit: Visitor<'v>,
    /// Units asked for and not yet taken, and the last run asked for.
    pub(crate) left: u64,
    pub(crate) run: u64,
}

impl<'v> Ledger<'v> {
    /// A ledger on `visit` with nothing asked for yet.
    pub(crate) fn new(visit: Visitor<'v>) -> Self {
        Self {
            visit,
            left: 0,
            run: 0,
        }
    }
}

impl Account for Ledger<'_> {
    #[inline]
    fn take(&mut self, n: u64) -> bool {
        debug_assert!(n <= PORTION, "a portion of {n} units: at most {PORTION}");
        if self.left < n {
            let run = (2 * self.run).clamp(n, u64::from(nrese_exec::POLL_UNITS));
            if !(self.visit)(run) {
                return false;
            }
            (self.left, self.run) = (run, run);
        }
        self.left -= n;
        true
    }
}
