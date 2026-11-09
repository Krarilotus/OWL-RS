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
//! allowed, at most `POLL_UNITS`, then stops at its next call (REAS-2's invariant, one level
//! down). Every loop of a counted read asks within that many units of work, with three steps
//! that can't be split as the only exception. Each is counted whole before it starts (in
//! runs of at most `POLL_UNITS`), then done whole: CPU work over memory the read already
//! holds, growing none but the list it works on.
//! - A merge step reads its cursors, one per run the read's ranges are in (`index::merge`).
//! - A union of two equality classes moves the smaller side's members
//!   (`nrese_exec::classes::Classes::union`).
//! - A class build sorts each class's members once, in place, and makes its maps at their
//!   final size, once (`Classes::of_with`).
//!
//! (REAS-3a sol-loop r1 S1, review r1 R1 and R2.)

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
