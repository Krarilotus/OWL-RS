//! Reads its reader's stop sees: the contract of the engine's counted reads
//! ([`crate::CountedSnapshot`]; REAS-3, design note `2026-11-09-p2-reas3-design.md` §1).
//!
//! A counted read takes a **visitor**, `&dyn Fn(u64) -> bool`, and calls `visit(n)` before
//! each `n` units of work it does, `n` at most [`nrese_exec::POLL_UNITS`]. A unit is O(1) work
//! on one item: a cursor a merge step compares, a run a read searches or probes, a stored
//! position it passes (deleted and filtered ones too), a copy an equality read makes, a
//! `sameAs` statement or identity an equality class build reads. Once `visit` answers false
//! the read does no more work and answers [`Stopped`]: never a value, never the end of the
//! data. `visit(0)` asks whether the reader has stopped without counting anything, and a
//! visitor that answered false keeps answering false: a reader's poller does both
//! (`nrese_reasoner::eval::Poller`).
//!
//! So after a reader's stop fires, a counted read does at most the units its visitor last
//! allowed, then stops at its next call: `POLL_UNITS` plus the run in progress per task
//! (REAS-2's invariant, one level down). A step that can't be split, and costs more than
//! `POLL_UNITS` units, is counted whole (in runs of at most `POLL_UNITS`) before it is done:
//! a merge step over more cursors, a union of two equality classes (the smaller side's
//! members move), one class's copy and sort. The run in progress is then that step, bounded
//! by the largest class or the run count, not by the data a read passes over (REAS-3a,
//! sol-loop r1 S1; splitting a union is the union kernel's, `nrese_exec::classes`).

/// A read its visitor stopped: an error its caller has to handle, never an answer (REAS-2
/// fix check r1, rule 2; one type for the engine and the reasoner).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

/// A counted read's visitor ([module docs](self)).
pub type Visitor<'v> = &'v dyn Fn(u64) -> bool;

/// Counts `n` units of work on `visit` before they are done, in runs of at most
/// `POLL_UNITS`: false once `visit` has stopped. For work done in one piece after its count
/// (a merge step's cursors), `n` is that piece's cost.
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
