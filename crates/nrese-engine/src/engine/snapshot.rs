//! Read-only, point-in-time view of the dataset.

use std::sync::Arc;

use oxrdf::{Quad, QuadRef, Term, TermRef};

use super::{ReadModel, Stack, Version};
use crate::quad::{AccessPlan, EncodedQuad, Permutation, QuadPattern};
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
    pub(super) fn new(version: Arc<Version>, dictionary: Arc<Dictionary>) -> Self {
        Self {
            version,
            dictionary,
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
        let plan = AccessPlan::for_pattern(pattern);
        Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack))
            .map(|stack| self.version.stack(stack).count_plan(&plan))
            .sum()
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

    /// For each distinct value of the first unbound component of `pattern` in `permutation`'s
    /// order, the number of matching quads in `model`, in id order. `None` if the pattern's
    /// bound components aren't a prefix of that order, or an included stack can't answer it.
    ///
    /// This answers `GROUP BY ?x` with `COUNT(*)` over one triple pattern without reading its
    /// matches: O(r · d · log n) for d groups (see `IndexVersion::group_counts`).
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
        let mut totals = std::collections::BTreeMap::new();
        for stack in Stack::ALL {
            if model.includes(stack) {
                self.version
                    .stack(stack)
                    .group_counts(&plan, bound, &mut totals);
            }
        }
        Some(
            totals
                .into_iter()
                .filter(|&(_, count)| count > 0)
                .map(|(value, count)| (TermId::from_raw(value), count as u64))
                .collect(),
        )
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

    pub fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.dictionary.decode_quad(quad)
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
