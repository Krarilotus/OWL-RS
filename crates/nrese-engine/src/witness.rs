//! Test-only counters of the work producers do (REAS-2 fix check r1, attack list B10). A
//! stopped poller doesn't ask its stop again, so the work a read does after a stop can't be
//! seen from the stop; each counter counts it at the producer that does it, on the thread
//! that does it. The module and every use of it are compiled only for this crate's tests or
//! with the `witness` feature, which other crates' tests take as a dev-dependency (REAS-3: an
//! adapter's read does its work here): a use outside them doesn't compile in the library, and
//! product builds carry none.

use std::cell::Cell;

/// What a counter counts.
#[derive(Clone, Copy)]
pub enum Site {
    /// Tombstone bitmap words a held count reads (`PermutationRun::tombstones_in`).
    TombstoneWords,
    /// Merge steps a quad scan takes, each key it passes, deleted ones included
    /// (`QuadScan::next`).
    ScanSteps,
    /// Copies an equality read makes of a stored quad (`Expand::pull`'s product).
    ExpandCopies,
    /// Runs a read searches or probes: a scan's range search per run
    /// (`IndexVersion::scan_plan`), a membership probe per run (`IndexVersion::contains`).
    RunProbes,
}

thread_local! {
    static COUNTS: [Cell<u64>; 4] = const { [const { Cell::new(0) }; 4] };
}

/// Counts `n` units at `site` on this thread.
pub(crate) fn add(site: Site, n: u64) {
    COUNTS.with(|counts| {
        let count = &counts[site as usize];
        count.set(count.get() + n);
    });
}

/// The units counted at `site` on this thread so far.
pub fn get(site: Site) -> u64 {
    COUNTS.with(|counts| counts[site as usize].get())
}
