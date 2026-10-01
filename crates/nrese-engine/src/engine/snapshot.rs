//! Read-only, point-in-time view of the dataset.

use std::sync::Arc;

use nrese_rdf::{Quad, QuadRef, Term, TermRef};

use super::statistics::Statistics;
use super::{ReadModel, Stack, Version};
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};
use crate::term::{Dictionary, TermId};

/// A consistent view of one committed revision. Cheap to clone; holding it keeps that
/// revision's runs alive but never blocks writers or compaction.
///
/// Reads without a model argument use [`ReadModel::Materialised`] (asserted and inferred
/// statements); the `*_in` variants take the model explicitly.
///
/// Term lookups are bounded by the dictionary size at the snapshot's revision, so terms
/// interned later (even by an open transaction) are reported as unknown, which keeps term
/// identity stable for the lifetime of a query.
#[derive(Clone)]
pub struct Snapshot {
    version: Arc<Version>,
    dictionary: Arc<Dictionary>,
    statistics: Arc<Statistics>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("revision", &self.revision())
            .field("asserted", &self.len_in(ReadModel::Asserted))
            .field("inferred", &self.len_in(ReadModel::Inferred))
            .finish()
    }
}

impl Snapshot {
    pub(super) fn new(
        version: Arc<Version>,
        dictionary: Arc<Dictionary>,
        statistics: Arc<Statistics>,
    ) -> Self {
        Self {
            version,
            dictionary,
            statistics,
        }
    }

    /// Whether `other` shows the same version of the same engine (a cache key).
    pub fn same_version(&self, other: &Snapshot) -> bool {
        Arc::ptr_eq(&self.version, &other.version)
    }

    /// A snapshot of `version` over this one's dictionary and statistics.
    pub(super) fn with_version(&self, version: Version) -> Snapshot {
        Snapshot {
            version: Arc::new(version),
            dictionary: Arc::clone(&self.dictionary),
            statistics: Arc::clone(&self.statistics),
        }
    }

    pub(crate) fn version(&self) -> &Version {
        &self.version
    }

    pub(crate) fn dictionary(&self) -> &Dictionary {
        &self.dictionary
    }

    /// Dictionary entries visible to this snapshot.
    pub(crate) fn dictionary_len(&self) -> u64 {
        self.version.dictionary_len
    }

    pub fn revision(&self) -> u64 {
        self.version.revision
    }

    /// Number of asserted and inferred quads. O(1).
    pub fn len(&self) -> u64 {
        self.len_in(ReadModel::Materialised)
    }

    /// Number of quads visible in `model`. O(1); exact because the stacks are disjoint.
    pub fn len_in(&self, model: ReadModel) -> u64 {
        Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack))
            .map(|stack| self.version.stack(stack).len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, quad: &EncodedQuad) -> bool {
        self.contains_in(ReadModel::Materialised, quad)
    }

    /// O(r log n) per included stack.
    pub fn contains_in(&self, model: ReadModel, quad: &EncodedQuad) -> bool {
        Stack::ALL
            .into_iter()
            .any(|stack| model.includes(stack) && self.stack_contains(stack, quad))
    }

    pub(crate) fn stack_contains(&self, stack: Stack, quad: &EncodedQuad) -> bool {
        self.version.stack(stack).contains(quad)
    }

    /// All asserted and inferred quads matching `pattern`.
    pub fn quads_for_pattern<'a>(
        &'a self,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        self.quads_for_pattern_in(ReadModel::Materialised, pattern)
    }

    /// All quads matching `pattern` in `model`: asserted matches first, then inferred ones,
    /// each part in the order of its access permutation. O(r log n + k·r) for k results over
    /// r runs. The iterator borrows only the snapshot, not `pattern`.
    pub fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        let pattern = *pattern;
        Stack::ALL
            .into_iter()
            .filter(move |&stack| model.includes(stack))
            .flat_map(move |stack| self.stack_quads(stack, &pattern))
    }

    /// Exact number of quads matching `pattern`, asserted and inferred.
    pub fn count(&self, pattern: &QuadPattern) -> u64 {
        self.count_in(ReadModel::Materialised, pattern)
    }

    /// Exact number of quads matching `pattern` in `model`, without producing them: O(r log n)
    /// per stack when the matching runs hold no tombstones in range (see
    /// `IndexVersion::count_plan`). The stacks are disjoint, so their counts add up.
    pub fn count_in(&self, model: ReadModel, pattern: &QuadPattern) -> u64 {
        if pattern.graph == GraphSelector::AnyNamed {
            // Every graph minus the default graph: two range counts instead of a scan.
            let every = QuadPattern {
                graph: GraphSelector::Any,
                ..*pattern
            };
            let default = QuadPattern {
                graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
                ..*pattern
            };
            return self.count_in(model, &every) - self.count_in(model, &default);
        }
        let plan = AccessPlan::for_pattern(pattern);
        Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack))
            .map(|stack| self.version.stack(stack).count_plan(&plan))
            .sum()
    }

    /// True if some quad matches `pattern` in `model`; cheaper than
    /// [`count_in`](Self::count_in)` > 0`.
    pub fn exists_in(&self, model: ReadModel, pattern: &QuadPattern) -> bool {
        if pattern.graph == GraphSelector::AnyNamed {
            return self.count_in(model, pattern) > 0;
        }
        let plan = AccessPlan::for_pattern(pattern);
        Stack::ALL
            .into_iter()
            .any(|stack| model.includes(stack) && self.version.stack(stack).any_plan(&plan))
    }

    /// Quads matching `pattern` in `model`, sorted by `permutation`'s key order, or `None` if
    /// the pattern's bound components aren't a prefix of that order (see
    /// [`Permutation::order`]). Executors use this to get scans in the order a merge join
    /// needs. Asserted and inferred matches are merged into one sorted stream; the stacks are
    /// disjoint, so no quad appears twice.
    pub fn scan_sorted_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<impl Iterator<Item = EncodedQuad> + use<'a>> {
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        let supported = Stack::ALL
            .into_iter()
            .all(|stack| !model.includes(stack) || stack.layout().supports(permutation));
        if !supported {
            return None;
        }
        let scan = |stack: Stack| {
            model
                .includes(stack)
                .then(|| self.version.stack(stack).scan_plan(&plan))
                .into_iter()
                .flatten()
        };
        Some(SortedMerge {
            left: scan(Stack::Asserted).peekable(),
            right: scan(Stack::Inferred).peekable(),
            permutation,
        })
    }

    /// Like [`scan_sorted_in`](Self::scan_sorted_in), with the first unbound component of the
    /// permutation's order restricted to `low..=high` (FILTER ranges over ordered ids, such
    /// as inline integers and dates, and whole kinds via [`TermId::kind_range`]).
    pub fn scan_range_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        low: TermId,
        high: TermId,
    ) -> Option<impl Iterator<Item = EncodedQuad> + use<'a>> {
        let plan = self.range_plan(model, pattern, permutation, low, high)?;
        let scan = move |stack: Stack| {
            model
                .includes(stack)
                .then(|| self.version.stack(stack).scan_plan(&plan))
                .into_iter()
                .flatten()
        };
        Some(SortedMerge {
            left: scan(Stack::Asserted).peekable(),
            right: scan(Stack::Inferred).peekable(),
            permutation,
        })
    }

    /// Exact number of quads [`scan_range_in`](Self::scan_range_in) yields.
    pub fn count_range_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        low: TermId,
        high: TermId,
    ) -> Option<u64> {
        let plan = self.range_plan(model, pattern, permutation, low, high)?;
        Some(
            Stack::ALL
                .into_iter()
                .filter(|&stack| model.includes(stack))
                .map(|stack| self.version.stack(stack).count_plan(&plan))
                .sum(),
        )
    }

    fn range_plan(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        low: TermId,
        high: TermId,
    ) -> Option<AccessPlan> {
        let mut plan = AccessPlan::in_permutation(pattern, permutation)?;
        let bound = plan
            .low
            .iter()
            .zip(&plan.high)
            .take_while(|(l, h)| l == h)
            .count();
        let supported = Stack::ALL
            .into_iter()
            .all(|stack| !model.includes(stack) || stack.layout().supports(permutation));
        if bound >= 4 || plan.exclude_default_graph || !supported {
            return None;
        }
        plan.low[bound] = low.raw();
        plan.high[bound] = high.raw();
        Some(plan)
    }

    /// For each distinct value of the first unbound component of `pattern` in `permutation`'s
    /// order, the number of matching quads in `model`, in id order. `None` if the pattern's
    /// bound components aren't a prefix of that order, or an included stack can't answer it.
    ///
    /// This answers `GROUP BY ?x` with `COUNT(*)` over one triple pattern without reading its
    /// matches: a walk over the groups of each run (see `IndexVersion::group_counts`).
    pub fn group_counts_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<Vec<(TermId, u64)>> {
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        if plan.exclude_default_graph {
            return None;
        }
        let bound = plan
            .low
            .iter()
            .zip(&plan.high)
            .take_while(|(l, h)| l == h)
            .count();
        if bound >= 4 {
            return None;
        }
        let supported = Stack::ALL
            .into_iter()
            .all(|stack| !model.includes(stack) || stack.layout().supports(permutation));
        if !supported {
            return None;
        }
        let mut counts = Vec::new();
        let mut sources = 0;
        for stack in Stack::ALL {
            if model.includes(stack) {
                let before = counts.len();
                self.version
                    .stack(stack)
                    .group_counts(&plan, bound, &mut counts);
                sources += usize::from(counts.len() > before);
            }
        }
        // One run of one stack yields each value once, in order; otherwise merge.
        if sources > 1 || !counts.is_sorted_by(|a, b| a.0 < b.0) {
            counts.sort_unstable_by_key(|&(value, _)| value);
            counts.dedup_by(|later, kept| {
                let same = later.0 == kept.0;
                if same {
                    kept.1 += later.1;
                }
                same
            });
        }
        Some(
            counts
                .into_iter()
                .filter(|&(_, count)| count > 0)
                .map(|(value, count)| (TermId::from_raw(value), count as u64))
                .collect(),
        )
    }

    /// The values of `components` (subject 0, predicate 1, object 2, graph 3) of the quads
    /// matching `pattern` in `model`, as columns in `permutation`'s order: what
    /// [`scan_sorted_in`](Self::scan_sorted_in) yields, decoded a block and a column at a
    /// time instead of a quad at a time. `None` where that can't answer: the matches lie
    /// in both stacks, or in several runs or a run with deletions, which must be merged
    /// quad by quad; or the plan needs a post-filter.
    pub fn scan_columns_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        components: &[usize],
    ) -> Option<Vec<Vec<u64>>> {
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        if plan.exclude_default_graph {
            return None;
        }
        let mut out = vec![Vec::new(); components.len()];
        let mut answered = false;
        for stack in Stack::ALL {
            if !model.includes(stack) {
                continue;
            }
            if !stack.layout().supports(permutation) {
                return None;
            }
            let index = self.version.stack(stack);
            if !index.any_plan(&plan) {
                continue;
            }
            if answered || !index.scan_columns(&plan, components, &mut out) {
                return None;
            }
            answered = true;
        }
        Some(out)
    }

    /// The number of distinct terms used as subject or object in `graphs` (one graph, or
    /// every graph) in `model`: what zero-length property paths start from. Exact; kept
    /// for this version once computed. The two walks (subjects, objects) run in parallel.
    /// `None` where [`Self::group_counts_in`] can't walk the graphs.
    pub fn node_count_in(&self, model: ReadModel, graphs: GraphSelector) -> Option<u64> {
        self.statistics
            .node_count(&self.version, model, graphs, || {
                let (pattern, by_subject, by_object) = match graphs {
                    GraphSelector::Exact(graph) => (
                        QuadPattern::in_graph(graph),
                        Permutation::Gspo,
                        Permutation::Gosp,
                    ),
                    _ => (QuadPattern::all(), Permutation::Spog, Permutation::Ospg),
                };
                let distinct = |permutation| -> Option<Vec<TermId>> {
                    let counts = self.group_counts_in(model, &pattern, permutation)?;
                    Some(counts.into_iter().map(|(id, _)| id).collect())
                };
                let (subjects, objects) =
                    rayon::join(|| distinct(by_subject), || distinct(by_object));
                let (subjects, objects) = (subjects?, objects?);
                // |S ∪ O| by a merge of the two sorted lists.
                let (mut i, mut j, mut both) = (0, 0, 0u64);
                while i < subjects.len() && j < objects.len() {
                    match subjects[i].cmp(&objects[j]) {
                        std::cmp::Ordering::Less => i += 1,
                        std::cmp::Ordering::Greater => j += 1,
                        std::cmp::Ordering::Equal => {
                            both += 1;
                            i += 1;
                            j += 1;
                        }
                    }
                }
                Some(subjects.len() as u64 + objects.len() as u64 - both)
            })
    }

    /// The number of distinct values of the first unbound component of `pattern` in
    /// `permutation`'s order, among its matches in `model`: a walk over each run's groups,
    /// with d values of memory. Exact unless deleted quads still shadow values in unmerged
    /// runs (then an upper bound). `None` if the pattern's bound components aren't a prefix
    /// of that order, or an included stack can't answer it. Planners should call the cached
    /// [`distinct_estimate_in`](Self::distinct_estimate_in) instead.
    pub fn distinct_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<u64> {
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        if plan.exclude_default_graph {
            return None;
        }
        let bound = plan
            .low
            .iter()
            .zip(&plan.high)
            .take_while(|(l, h)| l == h)
            .count();
        let supported = Stack::ALL
            .into_iter()
            .all(|stack| !model.includes(stack) || stack.layout().supports(permutation));
        if bound >= 4 || !supported {
            return None;
        }
        let mut values = Vec::new();
        let mut sources = 0;
        for stack in Stack::ALL {
            if model.includes(stack) {
                let before = values.len();
                self.version
                    .stack(stack)
                    .distinct_values(&plan, bound, &mut values);
                sources += usize::from(values.len() > before);
            }
        }
        // One run of one stack yields sorted distinct values already.
        if sources > 1 || !values.is_sorted() {
            values.sort_unstable();
            values.dedup();
        }
        Some(values.len() as u64)
    }

    /// [`distinct_in`](Self::distinct_in), cached by the engine across revisions until the
    /// pattern's match count drifts by more than a quarter. For cost estimates only: the
    /// value may be slightly stale.
    pub fn distinct_estimate_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<u64> {
        self.statistics.distinct(self, model, pattern, permutation)
    }

    pub(crate) fn stack_quads<'a>(
        &'a self,
        stack: Stack,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        self.version.stack(stack).scan(pattern)
    }

    /// True if the named graph `graph` contains at least one quad. O(r log n).
    pub fn contains_named_graph(&self, graph: TermId) -> bool {
        !graph.is_default_graph()
            && self
                .quads_for_pattern(&QuadPattern::in_graph(graph))
                .next()
                .is_some()
    }

    /// Ids of all named graphs that contain at least one quad, in id order. Inferences live
    /// in the default graph, so this reads the asserted stack only.
    pub fn named_graphs(&self) -> impl Iterator<Item = TermId> + '_ {
        let index = &self.version.asserted;
        std::iter::successors(index.next_named_graph(None), |&graph| {
            index.next_named_graph(Some(graph))
        })
    }

    /// Id of `term` as of this snapshot; `None` if the term is unknown (so no quad uses it).
    pub fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.dictionary
            .lookup_bounded(term, self.version.dictionary_len)
    }

    pub fn lookup_quad(&self, quad: QuadRef<'_>) -> Option<EncodedQuad> {
        self.dictionary
            .lookup_quad_bounded(quad, self.version.dictionary_len)
    }

    pub fn decode(&self, id: TermId) -> Option<Term> {
        self.dictionary.decode(id)
    }

    /// Calls `f` with a borrowed view of the dictionary term `id` (see
    /// [`Dictionary::with_view`](crate::Dictionary::with_view)); `None` for inline ids and
    /// ids this snapshot doesn't know.
    pub fn with_view<R>(&self, id: TermId, f: impl FnOnce(crate::TermView<'_>) -> R) -> Option<R> {
        if id.kind().is_dictionary() && id.payload() >= self.version.dictionary_len {
            return None;
        }
        self.dictionary.with_view(id, f)
    }

    /// Calls `f` with a lookup of the views of this snapshot's dictionary terms, under one
    /// read lock ([`Self::with_view`] for many terms).
    pub fn with_views<R>(
        &self,
        f: impl for<'v> FnOnce(&'v dyn Fn(TermId) -> Option<crate::TermView<'v>>) -> R,
    ) -> R {
        self.dictionary.with_views(self.version.dictionary_len, f)
    }

    pub fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.dictionary.decode_quad(quad)
    }

    /// The dictionary terms this snapshot knows whose text passes `test`, sorted (whether
    /// statements still use them is the caller's to check). One pass over the dictionary:
    /// worth it where a pattern has many more rows than the dictionary has bytes per row
    /// of a random read.
    pub fn matching_strings(&self, test: &crate::StringTest<'_>) -> Vec<TermId> {
        self.dictionary
            .matching_strings(test, self.version.dictionary_len)
    }

    /// Whether a prefix test ([`crate::Placement::Start`]) is answered from the text order
    /// by binary search, rather than by a pass over the dictionary.
    pub fn text_order_ready(&self) -> bool {
        self.dictionary.text_order_ready()
    }

    /// The size of this snapshot's dictionary text in bytes (the arena: what
    /// [`matching_strings`](Self::matching_strings) reads).
    pub fn dictionary_bytes(&self) -> u64 {
        self.dictionary.stats().arena_bytes
    }

    /// The string literals this snapshot's dictionary holds that match `query`, best first
    /// (whether statements still use them is the caller's to check).
    pub fn text_search(&self, query: &crate::TextQuery) -> Vec<crate::TextMatch> {
        let known = self.version.dictionary_len;
        let mut matches = self.dictionary.text_search(query);
        matches.retain(|m| TermId::from_raw(m.id).payload() < known);
        matches
    }
}

/// Two quad streams sorted by one permutation, merged into one sorted stream. The inputs
/// come from disjoint stacks, so equal keys never occur.
struct SortedMerge<L: Iterator<Item = EncodedQuad>, R: Iterator<Item = EncodedQuad>> {
    left: std::iter::Peekable<L>,
    right: std::iter::Peekable<R>,
    permutation: Permutation,
}

impl<L, R> Iterator for SortedMerge<L, R>
where
    L: Iterator<Item = EncodedQuad>,
    R: Iterator<Item = EncodedQuad>,
{
    type Item = EncodedQuad;

    #[inline]
    fn next(&mut self) -> Option<EncodedQuad> {
        match (self.left.peek(), self.right.peek()) {
            (Some(l), Some(r)) => {
                if self.permutation.to_key(l) <= self.permutation.to_key(r) {
                    self.left.next()
                } else {
                    self.right.next()
                }
            }
            (Some(_), None) => self.left.next(),
            (None, _) => self.right.next(),
        }
    }
}
