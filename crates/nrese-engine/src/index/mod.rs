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
//! - **Layouts** ([`Layout`]): the asserted stack holds arbitrary quads in the six pattern
//!   permutations plus GPSO (subject-sorted scans per predicate, for star joins); the
//!   inferred stack holds default-graph quads only and needs four.
//! - **Counts** ([`IndexVersion::count_plan`]) are exact without scanning when the runs hold
//!   no tombstones in range.
//!
//! Ownership: this module owns the physical layout and visibility rule. It knows nothing
//! about terms, transactions or durability.

pub(crate) mod compaction;
pub(crate) mod keys;
mod merge;
pub(crate) mod run;

use std::ops::Range;
use std::sync::Arc;

use std::borrow::Cow;

use keys::PackedKeys;
use merge::SignedMerge;
use run::{PermutationRun, Run};

use crate::quad::{AccessPlan, EncodedQuad, Permutation, QuadPattern};
use crate::term::TermId;

pub use compaction::CompactionPolicy;

/// Which quads a stack of runs may hold, and therefore which permutations it maintains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Layout {
    /// Any quads, in the six pattern permutations plus GPSO.
    #[default]
    Quads,
    /// Default-graph quads only (the inferred stack, and the asserted one until it holds a
    /// quad in a named graph), in SPOG, POSG, OSPG and PSOG. A graph-first order over one
    /// constant graph sorts exactly like the matching graph-last order, so the graph-first
    /// permutations would be redundant copies.
    DefaultGraph,
}

impl Layout {
    /// The layout that maintains exactly `permutations`, if one does.
    pub(crate) fn of(permutations: &[Permutation]) -> Option<Self> {
        [Self::Quads, Self::DefaultGraph]
            .into_iter()
            .find(|layout| {
                let wanted = layout.permutations();
                wanted.len() == permutations.len()
                    && wanted.iter().all(|p| permutations.contains(p))
            })
    }

    /// The smaller layout that can hold `quads`: the default-graph one unless one of them is
    /// in a named graph.
    pub(crate) fn holding(quads: &[EncodedQuad]) -> Self {
        match quads.iter().any(|quad| !quad.graph.is_default_graph()) {
            true => Self::Quads,
            false => Self::DefaultGraph,
        }
    }

    pub(crate) const fn permutations(self) -> &'static [Permutation] {
        match self {
            Self::Quads => &[
                Permutation::Spog,
                Permutation::Posg,
                Permutation::Ospg,
                Permutation::Gspo,
                Permutation::Gpos,
                Permutation::Gosp,
                Permutation::Gpso,
            ],
            Self::DefaultGraph => &[
                Permutation::Spog,
                Permutation::Posg,
                Permutation::Ospg,
                Permutation::Psog,
            ],
        }
    }

    /// True if scans in `permutation` can be answered: the layout maintains it, or (for the
    /// single-graph layout) its graph-last equivalent.
    pub(crate) fn supports(self, permutation: Permutation) -> bool {
        let maintained = |p: Permutation| self.permutations().contains(&p);
        maintained(permutation)
            || (self == Self::DefaultGraph && permutation.graph_last().is_some_and(maintained))
    }

    /// The plan that answers `plan` over this layout, or `None` if no quad the layout can
    /// hold matches it. The plan's permutation must be [`supported`](Self::supports).
    fn adapt(self, plan: &AccessPlan) -> Option<AccessPlan> {
        if self == Self::Quads {
            return Some(*plan);
        }
        if plan.exclude_default_graph {
            return None;
        }
        let Some(permutation) = plan.permutation.graph_last() else {
            return Some(*plan); // graph-last plans never bind the graph
        };
        let default = TermId::DEFAULT_GRAPH.raw();
        // Only the default graph can match, so the graph range must include it. Rotate the
        // graph from the front to the back.
        if !(plan.low[0] <= default && default <= plan.high[0]) {
            return None; // an exact named graph, or "all named graphs"
        }
        if plan.low[0] == plan.high[0] {
            return Some(AccessPlan {
                permutation,
                low: [plan.low[1], plan.low[2], plan.low[3], default],
                high: [plan.high[1], plan.high[2], plan.high[3], default],
                exclude_default_graph: false,
            });
        }
        // An unbound graph (executors' sorted scans): the prefix stops at the graph, so
        // nothing after it is bound, and every quad of this layout matches.
        Some(AccessPlan {
            permutation,
            low: [0, 0, 0, default],
            high: [u64::MAX, u64::MAX, u64::MAX, default],
            exclude_default_graph: false,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct IndexVersion {
    layout: Layout,
    runs: Arc<[Arc<Run>]>,
    len: u64,
}

impl IndexVersion {
    pub(crate) fn empty(layout: Layout) -> Self {
        Self {
            layout,
            ..Self::default()
        }
    }

    /// A version holding one base run built from packed permutations (a checkpoint): every
    /// permutation of a layout exactly once, all with the same number of keys. The layout
    /// is the one they make up.
    pub(crate) fn from_packed(packed: Vec<(Permutation, PackedKeys)>) -> Result<Self, String> {
        let permutations: Vec<Permutation> = packed.iter().map(|(p, _)| *p).collect();
        let layout = Layout::of(&permutations)
            .ok_or("the checkpoint doesn't hold every permutation of a layout once")?;
        let len = packed[0].1.len();
        if packed.iter().any(|(_, keys)| keys.len() != len) {
            return Err("checkpoint permutations differ in size".into());
        }
        let mut perms: [PermutationRun; Permutation::COUNT] = Default::default();
        for (permutation, keys) in packed {
            perms[permutation as usize] = PermutationRun {
                keys,
                tombstones: Box::default(),
            };
        }
        Ok(Self::empty(layout).with_run(Run::from_permutations(layout, perms)))
    }

    /// The visible keys of `permutation` as one packed array: the run's own array when the
    /// version is a single run without tombstones, else packed from the merged scan.
    pub(crate) fn packed(&self, permutation: Permutation) -> Cow<'_, PackedKeys> {
        if let [run] = &*self.runs {
            let perm = run.permutation(permutation);
            if perm.tombstones.is_empty() {
                return Cow::Borrowed(&perm.keys);
            }
        }
        let parts = self.runs.iter().map(|run| {
            let perm = run.permutation(permutation);
            (perm, 0, perm.keys.len())
        });
        Cow::Owned(PackedKeys::from_sorted_iter(
            SignedMerge::new(parts)
                .filter(|&(_, sign)| sign > 0)
                .map(|(key, _)| key),
        ))
    }

    /// A version holding exactly the (deduplicated) `quads` in one base run.
    pub(crate) fn from_quads(layout: Layout, quads: Vec<EncodedQuad>) -> Self {
        Self::empty(layout).with_run(Run::from_quads(layout, quads))
    }

    /// Number of visible quads. O(1).
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }

    /// `self` with the exact delta `inserts` and `deletes` appended as a run. A default-graph
    /// stack that receives a quad in a named graph turns into a quad stack first
    /// ([`with_quads_layout`](Self::with_quads_layout)).
    pub(crate) fn with_delta(&self, inserts: &[EncodedQuad], deletes: &[EncodedQuad]) -> Self {
        let named = |quads: &[EncodedQuad]| Layout::holding(quads) == Layout::Quads;
        let base = match self.layout == Layout::DefaultGraph && (named(inserts) || named(deletes)) {
            true => self.with_quads_layout(),
            false => self.clone(),
        };
        let run = Run::from_delta(base.layout, inserts, deletes);
        base.with_run(run)
    }

    /// The same quads in the quad layout: each run's graph-first permutations are its
    /// graph-last ones with the graph moved to the front (all its quads are in the default
    /// graph, so both sort alike). O(n) for n entries, once per store: at its first quad in
    /// a named graph.
    pub(crate) fn with_quads_layout(&self) -> Self {
        if self.layout == Layout::Quads {
            return self.clone();
        }
        Self {
            layout: Layout::Quads,
            runs: self
                .runs
                .iter()
                .map(|run| Arc::new(run.with_quads_layout()))
                .collect(),
            len: self.len,
        }
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

    /// Exact number of visible quads matching `plan`. By the exact-delta invariant each
    /// tombstone cancels exactly one insert of the same key in an older run, and both lie in
    /// any key range that holds either, so the count is the sum over runs of (entries in range
    /// − 2 × tombstones in range): O(r log n) for tombstone-free runs, plus a popcount over
    /// the range otherwise. Plans with a default-graph post-filter count by scanning.
    pub(crate) fn count_plan(&self, plan: &AccessPlan) -> u64 {
        let Some(adapted) = self.layout.adapt(plan) else {
            return 0;
        };
        if adapted.exclude_default_graph {
            return self.scan_plan(plan).count() as u64;
        }
        let signed: i64 = self
            .runs
            .iter()
            .map(|run| {
                let perm = run.permutation(adapted.permutation);
                let (start, end) = perm.range(&adapted.low, &adapted.high);
                (end - start) as i64 - 2 * perm.tombstones_in(start, end) as i64
            })
            .sum();
        u64::try_from(signed).expect("exact deltas never make a range count negative")
    }

    /// True if some visible quad matches `plan`: one bound search per run, where no run has
    /// tombstones (otherwise the exact count decides).
    pub(crate) fn any_plan(&self, plan: &AccessPlan) -> bool {
        let Some(adapted) = self.layout.adapt(plan) else {
            return false;
        };
        let has_tombstones = self
            .runs
            .iter()
            .any(|run| !run.permutation(adapted.permutation).tombstones.is_empty());
        if adapted.exclude_default_graph || has_tombstones {
            return self.count_plan(plan) > 0;
        }
        self.runs.iter().any(|run| {
            let keys = &run.permutation(adapted.permutation).keys;
            let start = keys.bound_in(0, keys.len(), &adapted.low, false);
            start < keys.len() && keys.get(start) <= adapted.high
        })
    }

    /// For each distinct value at key `position` of `plan`'s permutation (the first component
    /// after the plan's bound prefix), the signed number of quads with that value in each
    /// run, appended to `out`: sorted by value within a run, a value once per run that has
    /// it. Each run's range is walked group by group ([`PackedKeys::groups`]): no search per
    /// group, so many small groups cost a scan of the column, few large ones a search each.
    pub(crate) fn group_counts(
        &self,
        plan: &AccessPlan,
        position: usize,
        out: &mut Vec<(u64, i64)>,
    ) {
        let Some(adapted) = self.layout.adapt(plan) else {
            return;
        };
        debug_assert!(
            !adapted.exclude_default_graph,
            "group counts need an exact range"
        );
        // Adapting a graph-first plan to the single-graph layout moves the graph to the end.
        // Grouping by that graph puts every quad of this layout in the default graph's group.
        let position = if adapted.permutation == plan.permutation {
            position
        } else if position == 0 {
            out.push((TermId::DEFAULT_GRAPH.raw(), self.count_plan(plan) as i64));
            return;
        } else {
            position - 1
        };
        for run in self.runs.iter() {
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm.range(&adapted.low, &adapted.high);
            perm.keys.groups(start, end, position, |value, i, j| {
                out.push((value, (j - i) as i64 - 2 * perm.tombstones_in(i, j) as i64));
            });
        }
    }

    /// Appends the distinct values at key `position` of each run's range for `plan` (sorted
    /// per run; values may repeat across runs). Tombstones are ignored: for estimates.
    pub(crate) fn distinct_values(&self, plan: &AccessPlan, position: usize, out: &mut Vec<u64>) {
        let Some(adapted) = self.layout.adapt(plan) else {
            return;
        };
        let position = if adapted.permutation == plan.permutation {
            position
        } else if position == 0 {
            if self.count_plan(plan) > 0 {
                out.push(TermId::DEFAULT_GRAPH.raw());
            }
            return;
        } else {
            position - 1
        };
        for run in self.runs.iter() {
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm.range(&adapted.low, &adapted.high);
            perm.keys
                .groups(start, end, position, |value, _, _| out.push(value));
        }
    }

    /// Appends, for each of `components` (quad components: subject 0, predicate 1, object
    /// 2, graph 3), its values among the quads matching `plan`, in the plan's order, to
    /// the matching vector of `out`, decoded a block and a column at a time. Answers only
    /// for a version of at most one run without tombstones (`false`, nothing appended,
    /// otherwise: then quads must be merged and cancelled one by one).
    pub(crate) fn scan_columns(
        &self,
        plan: &AccessPlan,
        components: &[usize],
        out: &mut [Vec<u64>],
    ) -> bool {
        let Some(adapted) = self.layout.adapt(plan) else {
            return true; // nothing this layout holds matches
        };
        if adapted.exclude_default_graph {
            return false;
        }
        let run = match &*self.runs {
            [] => return true,
            [run] => run,
            _ => return false,
        };
        let perm = run.permutation(adapted.permutation);
        if !perm.tombstones.is_empty() {
            return false;
        }
        let order = adapted.permutation.order();
        let positions: Vec<usize> = components
            .iter()
            .map(|c| {
                order
                    .iter()
                    .position(|x| x == c)
                    .expect("every component has a key position")
            })
            .collect();
        let (start, end) = perm.range(&adapted.low, &adapted.high);
        perm.keys.decode_columns(start, end, &positions, out);
        true
    }

    pub(crate) fn scan_plan(&self, plan: &AccessPlan) -> QuadScan<'_> {
        let Some(plan) = self.layout.adapt(plan) else {
            return QuadScan {
                merge: SignedMerge::new(std::iter::empty()),
                permutation: plan.permutation,
                exclude_default_graph: false,
            };
        };
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
        debug_assert_eq!(run.layout(), self.layout, "run built for another stack");
        let len = self
            .len
            .checked_add_signed(run.net())
            .expect("exact delta can't make the quad count negative");
        let runs = self.runs.iter().cloned().chain([Arc::new(run)]).collect();
        Self {
            layout: self.layout,
            runs,
            len,
        }
    }

    /// A new version where the first `covered` runs are replaced by the runs of `base`, which
    /// must hold exactly what those runs make visible (a checkpoint of them).
    pub(crate) fn with_base(&self, covered: usize, base: &IndexVersion) -> Self {
        debug_assert_eq!(base.layout, self.layout, "base built for another stack");
        let runs = base
            .runs
            .iter()
            .chain(&self.runs[covered..])
            .cloned()
            .collect();
        Self {
            layout: self.layout,
            runs,
            len: self.len,
        }
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
            layout: self.layout,
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
