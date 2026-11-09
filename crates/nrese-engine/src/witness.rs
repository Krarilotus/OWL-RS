//! Test-only counters of the work producers do (REAS-2 fix check r1, attack list B10). A
//! stopped poller doesn't ask its stop again, so the work a read does after a stop can't be
//! seen from the stop; each counter counts it at the producer that does it, on the thread
//! that does it. The module and every use of it are `#[cfg(test)]`: a use outside a test
//! doesn't compile in the library, and release builds carry none.

use std::cell::Cell;

/// What a counter counts.
#[derive(Clone, Copy)]
pub(crate) enum Site {
    /// Tombstone bitmap words a held count reads (`PermutationRun::tombstones_in`).
    TombstoneWords,
    /// Merge steps a quad scan takes, each key it passes, deleted ones included
    /// (`QuadScan::next`).
    ScanSteps,
}

thread_local! {
    static COUNTS: [Cell<u64>; 2] = const { [Cell::new(0), Cell::new(0)] };
}

/// Counts `n` units at `site` on this thread.
pub(crate) fn add(site: Site, n: u64) {
    COUNTS.with(|counts| {
        let count = &counts[site as usize];
        count.set(count.get() + n);
    });
}

/// The units counted at `site` on this thread so far.
pub(crate) fn get(site: Site) -> u64 {
    COUNTS.with(|counts| counts[site as usize].get())
}
