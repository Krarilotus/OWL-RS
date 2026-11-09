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
//! down). Work too large for that is done in portions, each counted before it: a union's
//! member moves (`nrese_exec::classes::Classes::union_asking`), a class build's sort (runs
//! sorted alone, then merged a chunk at a time) and its copies and indexing.
//!
//! Open in REAS-3a (fix check r1): a merge step still compares every cursor of the read's
//! runs after one count of them (F1, its design under attack), and allocating memory (a class
//! build's maps at their final size, a union's list or map growing) is one allocator call
//! each, held for the owner (F2).

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
