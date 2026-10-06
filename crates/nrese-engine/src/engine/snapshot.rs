//! Read-only, point-in-time view of the dataset.

use std::sync::Arc;

use nrese_rdf::{Quad, QuadRef, Term, TermRef};

use super::equality::{Classes as EqualityClasses, Expand, Visibility as CopyVisibility};
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
///
/// With equality by representatives on ([`crate::Engine::set_equality`]), reads of the
/// default graph with inferred statements expand the stored closure to every identity of
/// its terms ([`super::equality`]); [`Self::stored`] reads the statements as stored.
#[derive(Clone)]
pub struct Snapshot {
    version: Arc<Version>,
    dictionary: Arc<Dictionary>,
    statistics: Arc<Statistics>,
    /// `owl:sameAs`, when reads expand equality classes.
    equality: Option<TermId>,
    /// Reads give each class's statements once, over its representative (stage C,
    /// [`Self::with_canonical_equality`]), instead of expanding them.
    canonical: bool,
}

/// Inferred statements hidden from a reader, as runs stacked over the inferred stack
/// ([`Snapshot::with_inferred_mask`]): a first run of tombstones, then one per change,
/// tombstones for statements hidden since and inserts for those shown again (visible
/// now, or gone from the stack). A change costs O(c log c) for its c statements, not a
/// new run of everything hidden.
#[derive(Debug, Clone, Default)]
pub struct InferredMask {
    runs: Vec<Arc<crate::index::run::Run>>,
}

impl InferredMask {
    /// A mask hiding `hidden`: inferred statements `snapshot` holds, each once.
    pub fn hiding(snapshot: &Snapshot, hidden: &[EncodedQuad]) -> Self {
        Self::default().changed(snapshot, hidden, &[])
    }

    /// This mask, made for an earlier revision, over `snapshot`: `hide` are statements
    /// `snapshot`'s stack holds that it didn't hide, `show` statements it hid that are
    /// to be seen again or that `snapshot`'s stack no longer holds. Each once, the two
    /// disjoint.
    pub fn changed(&self, snapshot: &Snapshot, hide: &[EncodedQuad], show: &[EncodedQuad]) -> Self {
        let mut runs = self.runs.clone();
        if !(hide.is_empty() && show.is_empty()) {
            let layout = snapshot.version.inferred.layout();
            runs.push(Arc::new(crate::index::run::Run::from_delta(
                layout, show, hide,
            )));
        }
        Self { runs }
    }

    /// The runs it stacks; past a few, a new mask is cheaper to read.
    pub fn runs(&self) -> usize {
        self.runs.len()
    }
}

/// What a snapshot reads ([`Snapshot::identity`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotIdentity {
    pub revision: u64,
    /// Its version's statements.
    pub content: super::Content,
    /// `owl:sameAs`, when reads expand equality classes.
    pub equality: Option<TermId>,
    /// Whether reads give equality classes canonically.
    pub canonical: bool,
}

/// The inferred statements a restricted snapshot keeps ([`Snapshot::with_inferred_subset`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferredSubset {
    /// All but these.
    Without(Vec<EncodedQuad>),
    /// These alone.
    Only(Vec<EncodedQuad>),
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
        equality: Option<TermId>,
    ) -> Self {
        Self {
            version,
            dictionary,
            statistics,
            equality,
            canonical: false,
        }
    }

    /// This snapshot reading the statements as stored: no equality expansion.
    pub fn stored(&self) -> Snapshot {
        Snapshot {
            equality: None,
            canonical: false,
            ..self.clone()
        }
    }

    /// This snapshot reading equality classes canonically (work package W4, stage C):
    /// each statement of the default graph once, over its terms' representatives, the
    /// pattern's constants in place; no copies for the other identities. Answers over it
    /// name one identity per class (`equality.answers = "canonical"`), and a query engine
    /// joins over it and expands the classes after the joins ([`Self::expand_late`]).
    /// Without equality classes it reads as this snapshot.
    pub fn with_canonical_equality(&self) -> Snapshot {
        Snapshot {
            canonical: true,
            ..self.clone()
        }
    }

    /// Whether this snapshot reads equality classes canonically.
    pub fn reads_canonically(&self) -> bool {
        self.canonical
    }

    /// The classes to expand after joins over [`Self::with_canonical_equality`], if reads
    /// in `model` may be done so: equality classes exist, the model shows every copy of a
    /// statement (the materialised one), and no named graph can hold a copy that should be
    /// hidden (the asserted stack keeps no named graphs). Joins over the canonical reads,
    /// expanded, then give the joins over the expanded reads (work package W4, stage C).
    pub fn expand_late(&self, model: ReadModel) -> Option<Arc<EqualityClasses>> {
        if self.canonical || model != ReadModel::Materialised {
            return None;
        }
        if self.version.asserted.layout() == crate::index::Layout::Quads {
            return None;
        }
        self.equality_classes()
            .filter(|classes| !classes.is_empty())
    }

    /// This snapshot with only a subset of its inferred statements, for a reader who may
    /// see no more (inferences under graph access): every read, counts and sorted scans
    /// included, sees the subset alone. `subset` names inferred statements this snapshot
    /// holds, each once.
    ///
    /// O(k log k) for k named statements: the smaller side of the split is named, as
    /// tombstones over the stack ([`InferredSubset::Without`]) or as a stack of its own
    /// ([`InferredSubset::Only`]). The result has statistics and equality classes of its
    /// own, so nothing it reads is cached for, or taken from, the whole snapshot.
    pub fn with_inferred_subset(&self, subset: InferredSubset) -> Snapshot {
        let inferred = match subset {
            InferredSubset::Without(hidden) => self.version.inferred.with_delta(&[], &hidden),
            InferredSubset::Only(kept) => {
                crate::index::IndexVersion::from_quads(self.version.inferred.layout(), kept)
            }
        };
        Snapshot {
            version: Arc::new(Version {
                content: super::Content::fresh(),
                asserted: self.version.asserted.clone(),
                inferred,
                revision: self.version.revision,
                dictionary_len: self.version.dictionary_len,
                equality: Default::default(),
            }),
            dictionary: Arc::clone(&self.dictionary),
            statistics: Arc::default(),
            equality: self.equality,
            canonical: self.canonical,
        }
    }

    /// This snapshot with the inferred statements `mask` hides removed, as
    /// [`Self::with_inferred_subset`] does, by stacking its runs: O(r) for its r runs.
    /// `None` if the mask was built for another layout of the stack.
    pub fn with_inferred_mask(&self, mask: &InferredMask) -> Option<Snapshot> {
        let stack = &self.version.inferred;
        if mask.runs.iter().any(|run| run.layout() != stack.layout()) {
            return None;
        }
        Some(Snapshot {
            version: Arc::new(Version {
                content: super::Content::fresh(),
                asserted: self.version.asserted.clone(),
                inferred: stack.with_runs(&mask.runs),
                revision: self.version.revision,
                dictionary_len: self.version.dictionary_len,
                equality: Default::default(),
            }),
            dictionary: Arc::clone(&self.dictionary),
            statistics: Arc::default(),
            equality: self.equality,
            canonical: self.canonical,
        })
    }

    /// The `owl:sameAs` classes reads expand, if equality by representatives is on.
    pub fn equality_classes(&self) -> Option<Arc<EqualityClasses>> {
        let same_as = self.equality?;
        Some(self.version.equality.classes(&self.version, same_as))
    }

    /// The classes a read of `pattern` in `model` expands: equality is on, some class
    /// exists, the model has inferred statements and the pattern can match the default
    /// graph.
    fn expanding(&self, model: ReadModel, pattern: &QuadPattern) -> Option<Arc<EqualityClasses>> {
        let default_graph = match pattern.graph {
            GraphSelector::Any => true,
            GraphSelector::AnyNamed => false,
            GraphSelector::Exact(graph) => graph.is_default_graph(),
        };
        if model == ReadModel::Asserted || !default_graph {
            return None;
        }
        let same_as = self.equality?;
        let classes = match self.canonical {
            // The classes without their members: aliases still normalise constants and
            // mark the statements a representative's copy stands for; nothing expands.
            true => self.version.equality.canonical(&self.version, same_as),
            false => self.version.equality.classes(&self.version, same_as),
        };
        (!classes.is_empty()).then_some(classes)
    }

    /// The expanded matches of `pattern` in `model`, sorted by `permutation` (module docs
    /// of [`super::equality`]). `None` if `permutation` can't answer the pattern or a stack
    /// doesn't keep it.
    fn expanded_sorted<'a>(
        &'a self,
        classes: Arc<EqualityClasses>,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<impl Iterator<Item = EncodedQuad> + use<'a>> {
        let normalised = classes.normalised(pattern);
        let plan = AccessPlan::in_permutation(&normalised, permutation)?;
        let supported = Stack::ALL
            .into_iter()
            .all(|stack| self.version.stack(stack).layout().supports(permutation));
        if !supported {
            return None;
        }
        // Named graphs are read as stored, with the pattern's own constants: from this
        // scan when the representatives are those constants, else from one of their own.
        let named = model == ReadModel::Materialised && pattern.graph == GraphSelector::Any;
        let renamed = normalised != *pattern;
        let named_plan = match named && renamed {
            true => Some(AccessPlan::in_permutation(pattern, permutation)?),
            false => None,
        };
        let stored = SortedMerge {
            left: self.version.asserted.scan_plan(&plan).peekable(),
            right: self.version.inferred.scan_plan(&plan).peekable(),
            permutation,
        };
        let expanded = Expand::new(
            stored,
            classes,
            *pattern,
            permutation,
            named && !renamed,
            model,
            &self.version,
        );
        let named = named_plan
            .into_iter()
            .flat_map(move |plan| self.version.asserted.scan_plan(&plan))
            .filter(|quad| !quad.graph.is_default_graph());
        Some(SortedMerge {
            left: expanded.peekable(),
            right: named.peekable(),
            permutation,
        })
    }

    /// The expanded matches of `pattern` in `model`, in the order of its access plan.
    fn expanded<'a>(
        &'a self,
        classes: Arc<EqualityClasses>,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        let permutation = AccessPlan::for_pattern(&classes.normalised(pattern)).permutation;
        match self.expanded_sorted(Arc::clone(&classes), model, pattern, permutation) {
            Some(sorted) => Either::Left(sorted),
            None => {
                // A layout without the permutation: every match, then sorted.
                let mut all: Vec<EncodedQuad> = Vec::new();
                let normalised = classes.normalised(pattern);
                for stack in Stack::ALL {
                    for quad in self.stack_quads(stack, &normalised) {
                        if !quad.graph.is_default_graph() {
                            if model == ReadModel::Materialised && normalised == *pattern {
                                all.push(quad);
                            }
                            continue;
                        }
                        if !classes.is_canonical(&quad) {
                            continue;
                        }
                        let touching = classes.touches(&quad);
                        let visibility = CopyVisibility::new(&self.version, model);
                        classes.expand(&quad, pattern, &mut |copy| {
                            if visibility.shows(&copy, touching) {
                                all.push(copy);
                            }
                        });
                    }
                }
                if model == ReadModel::Materialised
                    && pattern.graph == GraphSelector::Any
                    && normalised != *pattern
                {
                    all.extend(
                        self.stack_quads(Stack::Asserted, pattern)
                            .filter(|quad| !quad.graph.is_default_graph()),
                    );
                }
                all.sort_unstable_by_key(|quad| permutation.to_key(quad));
                Either::Right(all.into_iter())
            }
        }
    }

    /// The number of quads matching `pattern` in `model`, expanded: the stored matches over
    /// representatives weighted by how many quads each stands for.
    fn expanded_count(
        &self,
        classes: &EqualityClasses,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> u64 {
        let normalised = classes.normalised(pattern);
        let visibility = CopyVisibility::new(&self.version, model);
        let mut count = 0;
        for stack in Stack::ALL {
            for quad in self.stack_quads(stack, &normalised) {
                if !quad.graph.is_default_graph() {
                    count += u64::from(model == ReadModel::Materialised && normalised == *pattern);
                    continue;
                }
                if !classes.is_canonical(&quad) {
                    continue;
                }
                let touching = classes.touches(&quad);
                count += match visibility.shows_all(touching) {
                    true => classes.multiplicity(&quad, pattern),
                    false => {
                        let mut copies = 0;
                        classes.expand(&quad, pattern, &mut |copy| {
                            copies += u64::from(visibility.shows(&copy, touching));
                        });
                        copies
                    }
                };
            }
        }
        if model == ReadModel::Materialised
            && pattern.graph == GraphSelector::Any
            && normalised != *pattern
        {
            count += self
                .stack_quads(Stack::Asserted, pattern)
                .filter(|quad| !quad.graph.is_default_graph())
                .count() as u64;
        }
        count
    }

    /// An estimate of [`count_in`](Self::count_in) for planning, O(r log n): with
    /// equality classes, the stored matches without weighing the classes. Zero only if
    /// nothing matches.
    pub fn estimate_in(&self, model: ReadModel, pattern: &QuadPattern) -> u64 {
        let Some(classes) = self.expanding(model, pattern) else {
            return self.count_in(model, pattern);
        };
        let stored = self.stored();
        let normalised = classes.normalised(pattern);
        let mut estimate = stored.count_in(ReadModel::Materialised, &normalised);
        // Named graphs are read with the pattern's own constants.
        if normalised != *pattern && pattern.graph == GraphSelector::Any {
            estimate += stored.count_in(ReadModel::Asserted, pattern);
        }
        estimate
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
            equality: self.equality,
            canonical: self.canonical,
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

    /// What this snapshot reads, for caches of results computed on it: two snapshots with
    /// the same identity answer every read alike. A transaction's pending state or a
    /// masked view has an identity of its own, though it keeps its base's revision.
    pub fn identity(&self) -> SnapshotIdentity {
        SnapshotIdentity {
            revision: self.version.revision,
            content: self.version.content,
            equality: self.equality,
            canonical: self.canonical,
        }
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
        if quad.graph.is_default_graph()
            && let Some(classes) =
                self.expanding(model, &QuadPattern::in_graph(TermId::DEFAULT_GRAPH))
        {
            // Held iff its copy over representatives is stored (and, for the inferred
            // model, it isn't asserted).
            let canonical = EncodedQuad::new(
                classes.representative(quad.subject),
                classes.representative(quad.predicate),
                classes.representative(quad.object),
                quad.graph,
            );
            let stored = Stack::ALL
                .into_iter()
                .any(|stack| self.stack_contains(stack, &canonical));
            return stored
                && CopyVisibility::new(&self.version, model)
                    .shows(quad, classes.touches(&canonical));
        }
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
        if let Some(classes) = self.expanding(model, pattern) {
            return Either::Left(self.expanded(classes, model, pattern));
        }
        let pattern = *pattern;
        Either::Right(
            Stack::ALL
                .into_iter()
                .filter(move |&stack| model.includes(stack))
                .flat_map(move |stack| self.stack_quads(stack, &pattern)),
        )
    }

    /// Exact number of quads matching `pattern`, asserted and inferred.
    pub fn count(&self, pattern: &QuadPattern) -> u64 {
        self.count_in(ReadModel::Materialised, pattern)
    }

    /// Exact number of quads matching `pattern` in `model`, without producing them: O(r log n)
    /// per stack when the matching runs hold no tombstones in range (see
    /// `IndexVersion::count_plan`). The stacks are disjoint, so their counts add up.
    pub fn count_in(&self, model: ReadModel, pattern: &QuadPattern) -> u64 {
        if let Some(classes) = self.expanding(model, pattern) {
            return self.expanded_count(&classes, model, pattern);
        }
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
        if let Some(classes) = self.expanding(model, pattern) {
            return self.expanded(classes, model, pattern).next().is_some();
        }
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
        if let Some(classes) = self.expanding(model, pattern) {
            return self
                .expanded_sorted(classes, model, pattern, permutation)
                .map(Either::Left);
        }
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        let supported = Stack::ALL.into_iter().all(|stack| {
            !model.includes(stack) || self.version.stack(stack).layout().supports(permutation)
        });
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
        Some(Either::Right(SortedMerge {
            left: scan(Stack::Asserted).peekable(),
            right: scan(Stack::Inferred).peekable(),
            permutation,
        }))
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

    /// [`Self::scan_range_in`] as columns of `components`, decoded a block and a column at
    /// a time ([`Self::scan_columns_in`]); `None` where that can't answer.
    pub fn scan_range_columns_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        low: TermId,
        high: TermId,
        components: &[usize],
    ) -> Option<Vec<Vec<u64>>> {
        let plan = self.range_plan(model, pattern, permutation, low, high)?;
        self.columns_of_stacks(model, &plan, permutation, components)
    }

    /// Whether a statement read in `model` has a triple term (RDF 1.2): triple terms are
    /// objects only and their ids one range, so this is one seek in the object-first index.
    pub fn holds_triple_terms(&self, model: ReadModel) -> bool {
        let (low, high) = TermId::kind_range(crate::TermKind::Triple);
        self.scan_range_in(
            model,
            &QuadPattern::all(),
            crate::quad::Permutation::Ospg,
            low,
            high,
        )
        .is_some_and(|mut quads| quads.next().is_some())
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
        if self.expanding(model, pattern).is_some() {
            return None;
        }
        let mut plan = AccessPlan::in_permutation(pattern, permutation)?;
        let bound = plan
            .low
            .iter()
            .zip(&plan.high)
            .take_while(|(l, h)| l == h)
            .count();
        let supported = Stack::ALL.into_iter().all(|stack| {
            !model.includes(stack) || self.version.stack(stack).layout().supports(permutation)
        });
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
        if self.expanding(model, pattern).is_some() {
            return None;
        }
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
        let supported = Stack::ALL.into_iter().all(|stack| {
            !model.includes(stack) || self.version.stack(stack).layout().supports(permutation)
        });
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
        if self.expanding(model, pattern).is_some() {
            return None;
        }
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        if plan.exclude_default_graph {
            return None;
        }
        if Stack::ALL.into_iter().any(|stack| {
            model.includes(stack) && !self.version.stack(stack).layout().supports(permutation)
        }) {
            return None;
        }
        self.columns_of_stacks(model, &plan, permutation, components)
    }

    /// The columns `components` of the quads matching `plan` in each stack of `model`, each
    /// decoded a block and a column at a time, and merged in `permutation`'s order where
    /// both stacks have matches (they hold no quad twice): asserted and inferred
    /// statements of one pattern, as reasoning gives them. `None` where a stack can't be
    /// decoded so (several runs, deletions).
    fn columns_of_stacks(
        &self,
        model: ReadModel,
        plan: &AccessPlan,
        permutation: Permutation,
        components: &[usize],
    ) -> Option<Vec<Vec<u64>>> {
        let mut parts: Vec<Vec<Vec<u64>>> = Vec::new();
        for stack in Stack::ALL {
            if !model.includes(stack) {
                continue;
            }
            let index = self.version.stack(stack);
            if !index.any_plan(plan) {
                continue;
            }
            let mut out = vec![Vec::new(); components.len()];
            if !index.scan_columns(plan, components, &mut out) {
                return None;
            }
            parts.push(out);
        }
        match parts.len() {
            0 => Some(vec![Vec::new(); components.len()]),
            1 => parts.pop(),
            _ => {
                let right = parts.pop().expect("two parts");
                let left = parts.pop().expect("two parts");
                Some(merge_columns(left, right, permutation, components))
            }
        }
    }

    /// The number of distinct terms used as subject or object in `graphs` (one graph, or
    /// every graph) in `model`: what zero-length property paths start from. Exact; kept
    /// for this version once computed. The two walks (subjects, objects) run in parallel.
    /// `None` where [`Self::group_counts_in`] can't walk the graphs.
    pub fn node_count_in(&self, model: ReadModel, graphs: GraphSelector) -> Option<u64> {
        let pattern = QuadPattern {
            graph: graphs,
            ..QuadPattern::all()
        };
        if self.expanding(model, &pattern).is_some() {
            return None;
        }
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
        if self.expanding(model, pattern).is_some() {
            return None;
        }
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
        let supported = Stack::ALL.into_iter().all(|stack| {
            !model.includes(stack) || self.version.stack(stack).layout().supports(permutation)
        });
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

    /// The characteristic sets of the default graph in `model`, for estimates of star
    /// joins ([`super::characteristic`]); `None` while they are being built for a large
    /// graph, or where the graph's subjects have too many.
    pub fn characteristic_sets_in(
        &self,
        model: ReadModel,
    ) -> Option<Arc<super::characteristic::CharacteristicSets>> {
        self.statistics.characteristic_sets(self, model)
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

    /// The vector literals this snapshot's dictionary holds that are nearest to
    /// `query.vector`, among those `accept` takes, nearest first, with how the search went
    /// (whether statements still use them is the caller's to check, in `accept`).
    pub fn vector_search(
        &self,
        query: &crate::VectorQuery,
        accept: &(dyn Fn(TermId) -> bool + Sync),
    ) -> (Vec<(TermId, f32)>, crate::VectorSearchReport) {
        let (hits, report) =
            self.dictionary
                .vector_search(query, self.version.dictionary_len, accept);
        let hits = hits
            .into_iter()
            .map(|hit| (TermId::from_raw(hit.id), hit.distance))
            .collect();
        (hits, report)
    }

    /// Builds the graph searches like `query` use, covering every vector of its
    /// dimension, before returning: a warm-up (searches build it themselves, a large one
    /// on a thread of its own while they scan exactly).
    pub fn prepare_vector_graph(&self, query: &crate::VectorQuery) {
        self.dictionary.prepare_vector_graph(query);
    }

    /// The IRIs this snapshot's dictionary holds whose local names match `query`, best
    /// first (whether statements still use them is the caller's to check).
    pub fn iri_search(&self, query: &crate::TextQuery) -> Vec<crate::TextMatch> {
        let known = self.version.dictionary_len;
        let mut matches = self.dictionary.iri_search(query);
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

/// Two column sets (`components` of quads), each sorted in `permutation`'s order, merged
/// into one in that order: rows compared on the components by their place in the order.
fn merge_columns(
    left: Vec<Vec<u64>>,
    right: Vec<Vec<u64>>,
    permutation: Permutation,
    components: &[usize],
) -> Vec<Vec<u64>> {
    let order = permutation.order();
    let mut keys: Vec<usize> = (0..components.len()).collect();
    keys.sort_by_key(|&c| order.iter().position(|&o| o == components[c]));
    let rows = |columns: &[Vec<u64>]| columns.first().map_or(0, Vec::len);
    let (n, m) = (rows(&left), rows(&right));
    let mut out: Vec<Vec<u64>> = (0..components.len())
        .map(|_| Vec::with_capacity(n + m))
        .collect();
    let less = |a: usize, b: usize| {
        keys.iter()
            .map(|&k| left[k][a].cmp(&right[k][b]))
            .find(|ordering| ordering.is_ne())
            .is_some_and(|ordering| ordering.is_le())
    };
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        let from_left = j == m || (i < n && less(i, j));
        for (c, column) in out.iter_mut().enumerate() {
            column.push(if from_left { left[c][i] } else { right[c][j] });
        }
        if from_left {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// One of two iterators.
enum Either<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> Iterator for Either<L, R>
where
    L: Iterator<Item = EncodedQuad>,
    R: Iterator<Item = EncodedQuad>,
{
    type Item = EncodedQuad;

    #[inline]
    fn next(&mut self) -> Option<EncodedQuad> {
        match self {
            Either::Left(left) => left.next(),
            Either::Right(right) => right.next(),
        }
    }
}
