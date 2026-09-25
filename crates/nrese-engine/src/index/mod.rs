//! The quad index: an immutable [`IndexVersion`] is a stack of sorted runs (oldest first).
//!
//! - **Reads** ([`IndexVersion::scan`]) pick one permutation per pattern
//!   ([`AccessPlan`]), binary-search the key range in every run and k-way merge the ranges
//!   with sign-sum visibility ([`merge`]). O(r log n + k·r) for r runs and k results.
//! - **Writes** never modify a version: [`IndexVersion::with_run`] returns a new version
//!   that shares all existing runs. Old versions stay valid for as long as a snapshot holds
//!   them, which is the whole MVCC mechanism.
//! - **Compaction** replaces a window of runs by their merge
//!   ([`IndexVersion::with_compacted`]); the window choice is in [`compaction`].
//!
//! Ownership: this module owns the physical layout and visibility rule. It knows nothing
//! about terms, transactions or durability.

pub(crate) mod compaction;
mod merge;
pub(crate) mod run;

use std::ops::Range;
use std::sync::Arc;

use merge::SignedMerge;
use run::Run;

use crate::quad::{AccessPlan, EncodedQuad, Permutation, QuadPattern};
use crate::term::TermId;

pub use compaction::CompactionPolicy;

#[derive(Debug, Clone, Default)]
pub(crate) struct IndexVersion {
    runs: Arc<[Arc<Run>]>,
    len: u64,
}

impl IndexVersion {
    /// A version holding exactly the (deduplicated) `quads` in one base run.
    pub(crate) fn from_quads(quads: Vec<EncodedQuad>) -> Self {
        Self::default().with_run(Run::from_quads(quads))
    }

    /// Number of visible quads. O(1).
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn runs(&self) -> &[Arc<Run>] {
        &self.runs
    }

    /// True if `quad` is visible. O(r log n).
    pub(crate) fn contains(&self, quad: &EncodedQuad) -> bool {
        self.runs.iter().map(|run| run.sign_of(quad)).sum::<i64>() > 0
    }

    /// All visible quads matching `pattern`, in the order of the chosen permutation.
    pub(crate) fn scan(&self, pattern: &QuadPattern) -> QuadScan<'_> {
        self.scan_plan(&AccessPlan::for_pattern(pattern))
    }

    /// The smallest named graph with a visible quad whose id is greater than `after`
    /// (or the smallest overall). A GSPO seek, O(r log n) per call when the graph is not
    /// fully deleted, so listing g graphs costs O(g·r log n) instead of a full scan.
    pub(crate) fn next_named_graph(&self, after: Option<TermId>) -> Option<TermId> {
        let first = match after {
            Some(graph) => graph.raw().checked_add(1)?,
            None => TermId::DEFAULT_GRAPH.raw() + 1,
        };
        let plan = AccessPlan {
            permutation: Permutation::Gspo,
            low: [first, 0, 0, 0],
            high: [u64::MAX; 4],
            exclude_default_graph: false,
        };
        self.scan_plan(&plan).next().map(|quad| quad.graph)
    }

    fn scan_plan(&self, plan: &AccessPlan) -> QuadScan<'_> {
        let parts = self.runs.iter().map(|run| {
            let perm = run.permutation(plan.permutation);
            let (start, end) = perm.range(&plan.low, &plan.high);
            (perm, start, end)
        });
        QuadScan {
            merge: SignedMerge::new(parts),
            permutation: plan.permutation,
            exclude_default_graph: plan.exclude_default_graph,
        }
    }

    /// A new version with `run` appended. The run must be an exact delta against `self`.
    pub(crate) fn with_run(&self, run: Run) -> Self {
        if run.entries() == 0 {
            return self.clone();
        }
        let len = self
            .len
            .checked_add_signed(run.net())
            .expect("exact delta can't make the quad count negative");
        let runs = self.runs.iter().cloned().chain([Arc::new(run)]).collect();
        Self { runs, len }
    }

    /// A new version where the runs in `window` are replaced by `merged`, which must be
    /// [`compaction::merge_runs`] of exactly those runs.
    pub(crate) fn with_compacted(&self, window: Range<usize>, merged: Run) -> Self {
        let runs = self.runs[..window.start]
            .iter()
            .cloned()
            .chain((merged.entries() > 0).then(|| Arc::new(merged)))
            .chain(self.runs[window.end..].iter().cloned())
            .collect();
        Self {
            runs,
            len: self.len,
        }
    }
}

/// Iterator returned by [`IndexVersion::scan`].
pub(crate) struct QuadScan<'a> {
    merge: SignedMerge<'a>,
    permutation: Permutation,
    exclude_default_graph: bool,
}

impl Iterator for QuadScan<'_> {
    type Item = EncodedQuad;

    #[inline]
    fn next(&mut self) -> Option<EncodedQuad> {
        loop {
            let (key, sign) = self.merge.next()?;
            if sign <= 0 {
                continue;
            }
            let quad = self.permutation.key_to_quad(&key);
            if self.exclude_default_graph && quad.graph.is_default_graph() {
                continue;
            }
            return Some(quad);
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.merge.remaining()))
    }
}

#[cfg(test)]
mod model_tests;
