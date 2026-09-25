//! Size-tiered compaction: which runs to merge (policy) and how (merge).
//!
//! Policy (ADR-0002): starting from the newest run, the merge window extends to the next
//! older run while `older.entries <= fanout * window.entries`. After compaction, each run is
//! therefore more than `fanout` times larger than all newer runs together, which bounds the
//! run count at O(log_fanout n) and keeps scans at O(log n) k-way merges. Each entry is
//! rewritten O(log n) times, i.e. write cost is O(log n) amortised per quad.
//!
//! Windows up to [`CompactionPolicy::inline_entry_limit`] entries are merged inside the
//! commit (microseconds to a few milliseconds); larger ones go to the engine's background
//! compactor, so commit latency never includes a rewrite of the base run.

use std::ops::Range;
use std::sync::Arc;

use super::merge::SignedMerge;
use super::run::{PermutationRun, Run, build_all};
use crate::quad::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionPolicy {
    /// Size ratio between neighbouring runs; higher means fewer merges and more runs.
    pub fanout: u64,
    /// Largest window (in entries) merged synchronously inside a commit.
    pub inline_entry_limit: u64,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            fanout: 4,
            inline_entry_limit: 32 * 1024,
        }
    }
}

/// A window of runs to merge and its total entry count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionPlan {
    pub window: Range<usize>,
    pub entries: u64,
}

impl CompactionPolicy {
    /// The window of runs (a suffix of `runs`) that should be merged, if any.
    pub(crate) fn plan(&self, runs: &[Arc<Run>]) -> Option<CompactionPlan> {
        let newest = runs.len().checked_sub(1)?;
        let mut start = newest;
        let mut entries = runs[newest].entries();
        while start > 0 && runs[start - 1].entries() <= self.fanout.saturating_mul(entries) {
            start -= 1;
            entries += runs[start].entries();
        }
        (start < newest).then_some(CompactionPlan {
            window: start..runs.len(),
            entries,
        })
    }

    pub(crate) fn is_inline(&self, plan: &CompactionPlan) -> bool {
        plan.entries <= self.inline_entry_limit
    }
}

/// Merges a contiguous window of runs into one. Entries whose signs cancel are dropped, so
/// a window that includes the oldest run leaves no tombstones. O(e log k) for e entries.
pub(crate) fn merge_runs(runs: &[Arc<Run>]) -> Run {
    let layout = runs
        .first()
        .map_or_else(Default::default, |run| run.layout());
    debug_assert!(
        runs.iter().all(|run| run.layout() == layout),
        "merging runs of different stacks"
    );
    let total: u64 = runs.iter().map(|run| run.entries()).sum();
    let perms = build_all(layout, total as usize, |permutation| {
        let parts = runs.iter().map(|run| {
            let perm = run.permutation(permutation);
            (perm, 0, perm.keys.len())
        });
        let entries: Vec<(Key, bool)> = SignedMerge::new(parts)
            .filter(|&(_, sign)| sign != 0)
            .map(|(key, sign)| (key, sign < 0))
            .collect();
        PermutationRun::from_sorted(entries)
    });
    Run::from_permutations(layout, perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Layout;
    use crate::quad::EncodedQuad;
    use crate::term::{TermId, TermKind};

    fn quad(n: u64) -> EncodedQuad {
        let id = |v| TermId::new(TermKind::Iri, v);
        EncodedQuad::new(id(n), id(1), id(2), TermId::DEFAULT_GRAPH)
    }

    fn run_of(size: u64) -> Arc<Run> {
        Arc::new(Run::from_quads(
            Layout::Quads,
            (0..size).map(quad).collect(),
        ))
    }

    #[test]
    fn plan_merges_the_suffix_of_comparable_runs() {
        let policy = CompactionPolicy::default();
        let window = |runs: &[Arc<Run>]| policy.plan(runs).map(|plan| plan.window);
        assert_eq!(window(&[]), None);
        assert_eq!(window(&[run_of(10)]), None);
        assert_eq!(window(&[run_of(100), run_of(1)]), None);
        assert_eq!(window(&[run_of(100), run_of(3), run_of(1)]), Some(1..3));
        assert_eq!(window(&[run_of(8), run_of(1), run_of(1)]), Some(0..3));
        let plan = policy.plan(&[run_of(8), run_of(1), run_of(1)]).unwrap();
        assert_eq!(plan.entries, 10);
        assert!(policy.is_inline(&plan));
    }

    #[test]
    fn merging_into_the_oldest_run_drops_tombstones() {
        let base = run_of(4);
        let delta = Arc::new(Run::from_delta(
            Layout::Quads,
            &[quad(9)],
            &[quad(0), quad(1)],
        ));
        let merged = merge_runs(&[base, delta]);
        assert_eq!((merged.inserts(), merged.deletes()), (3, 0));
    }

    #[test]
    fn merging_newer_runs_keeps_unmatched_tombstones() {
        let a = Arc::new(Run::from_delta(Layout::Quads, &[quad(9)], &[quad(0)]));
        let b = Arc::new(Run::from_delta(Layout::Quads, &[], &[quad(9)]));
        let merged = merge_runs(&[a, b]);
        // quad(9) cancels out; the tombstone for quad(0) still shadows an older run.
        assert_eq!((merged.inserts(), merged.deletes()), (0, 1));
    }
}
