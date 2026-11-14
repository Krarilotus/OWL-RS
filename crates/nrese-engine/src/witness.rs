//! Test-only counters of the work producers do (REAS-2 fix check r1, attack list B10). A
//! stopped poller doesn't ask its stop again, so the work a read does after a stop can't be
//! seen from the stop; each counter counts it at the producer that does it, on the thread
//! that does it. The module and every use of it are compiled only for this crate's tests or,
//! with the `witness` feature, which other crates' tests take as a dev-dependency (REAS-3: an
//! adapter's read does its work here), in a debug build: a use outside them doesn't compile in
//! the library. A dev-dependency's features reach that crate's examples too, `perf_lab` among
//! them, which every count and timing job builds; those builds are release builds, so they carry
//! none ([`MARK`] tells: the count jobs check a release binary doesn't hold it).

use std::cell::Cell;

/// In every build that has the counters, and only there: a count job's check that a release
/// binary carries none looks for these bytes.
#[used]
pub static MARK: [u8; 24] = *b"nrese-engine-witness-on\0";

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
    /// Identities an equality class build's finish handles: each member it copies out of the
    /// union kernel, each it checks for a literal and indexes as an alias (`Classes::of_with`;
    /// its sorts are [`Site::SortWork`], a union's moves the kernel's own counter,
    /// `nrese_exec::classes::witness`).
    ClassFinish,
    /// Merge cursors read outside a merge step. The remaining-entries hint
    /// (`SignedMerge::remaining`) read every cursor until REAS-3a review r1 R2; it now keeps
    /// its count as the cursors advance and reads none, so nothing counts here.
    CursorReads,
    /// A class build's sort work: each comparison of a run's sort and each identity a merge
    /// of sorted runs moves (`Classes::of_with`; REAS-3a fix check r1 F3).
    SortWork,
    /// Key comparisons a run's range or bound search makes (`PackedKeys::bound_in` and its
    /// callers; REAS-3a fix check r1 F1).
    SearchWork,
    /// A merge's work: each key comparison its selection makes, each cursor it advances and
    /// each key a cursor decodes (`index::merge`; REAS-3a fix check r1 F1).
    MergeWork,
    /// Reads that took a point read's own path (`CountedSnapshot::point_quads`), not one of
    /// the counted reads it falls back to.
    PointReads,
}

thread_local! {
    static COUNTS: [Cell<u64>; 10] = const { [const { Cell::new(0) }; 10] };
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
