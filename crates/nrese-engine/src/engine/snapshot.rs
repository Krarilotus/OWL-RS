//! Read-only, point-in-time view of the dataset.

use std::sync::Arc;

use nrese_rdf::{Quad, QuadRef, Term, TermRef};

use super::equality::{Classes as EqualityClasses, Expand, Visibility as CopyVisibility};
use super::statistics::Statistics;
use super::{ReadModel, Stack, Version};
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};
use crate::term::{Dictionary, TermId};

mod cursor;
pub use cursor::{ProbeCursor, Seek};

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
    /// What canonical reads in a model with inferred statements read: the version with
    /// its named graphs over representatives (`equality::canonical_version`), where that
    /// differs from `version`. The canonical model's view, never `version` itself, which
    /// the asserted model and [`Self::stored`] read as stored ([`Self::data`]).
    canonical_data: Option<Arc<Version>>,
    /// The bytes of `canonical_data` the view budget refused: then it is this snapshot's
    /// alone ([`Self::canonical_view_uncharged`]).
    canonical_uncharged: usize,
    /// What the canonical views a version keeps are charged to, beside the queries'
    /// envelopes ([`crate::Engine::set_canonical_view_budget`]).
    views: Arc<nrese_exec::Budget>,
}

/// The canonical view of `version` under `same_as` ([`Snapshot::canonical_data`]), charged
/// to `views`, and its bytes `views` refused: `None` where it is `version` itself.
fn canonical_data(
    version: &Arc<Version>,
    same_as: Option<TermId>,
    views: &Arc<nrese_exec::Budget>,
) -> (Option<Arc<Version>>, usize) {
    match same_as.and_then(|same_as| super::equality::CanonicalCache::view(version, same_as, views))
    {
        Some(view) => (Some(view.version), view.uncharged),
        None => (None, 0),
    }
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
        views: Arc<nrese_exec::Budget>,
    ) -> Self {
        Self {
            version,
            dictionary,
            statistics,
            equality,
            canonical: false,
            canonical_data: None,
            canonical_uncharged: 0,
            views,
        }
    }

    /// This snapshot reading the default graph expanded over the `owl:sameAs` classes of
    /// its inferred stack (each identity other than its class's representative stored as
    /// `identity sameAs representative`), as snapshots read with equality by
    /// representatives on ([`crate::Engine::set_equality`]): for a stack kept so beside
    /// the store's own, as the OWL 2 DL upper bound is (`nrese-store`'s `dl` module).
    /// `None` reads the statements as stored ([`Self::stored`]).
    pub fn with_equality(&self, same_as: Option<TermId>) -> Snapshot {
        Snapshot {
            equality: same_as,
            canonical: false,
            canonical_data: None,
            canonical_uncharged: 0,
            ..self.clone()
        }
    }

    /// This snapshot reading the statements as stored: no equality expansion.
    pub fn stored(&self) -> Snapshot {
        Snapshot {
            equality: None,
            canonical: false,
            canonical_data: None,
            canonical_uncharged: 0,
            ..self.clone()
        }
    }

    /// This snapshot reading equality classes canonically (work package W4, stage C):
    /// each statement of the default graph once, over its terms' representatives, the
    /// pattern's constants in place; no copies for the other identities. Named graphs are
    /// read over representatives too (`equality::canonical_version`), so that every id a
    /// read gives in a model with inferred statements is a representative. Answers over it
    /// name one identity per class (`equality.answers = "canonical"`), and a query engine
    /// joins over it and expands the classes after the joins ([`Self::expand_late`]).
    /// Without equality classes it reads as this snapshot.
    pub fn with_canonical_equality(&self) -> Snapshot {
        let (canonical_data, canonical_uncharged) =
            canonical_data(&self.version, self.equality, &self.views);
        Snapshot {
            canonical: true,
            canonical_data,
            canonical_uncharged,
            ..self.clone()
        }
    }

    /// The bytes of this snapshot's canonical view that the view budget refused (0 where
    /// it keeps none, or the version keeps it): the view was made for this snapshot alone,
    /// and its reader charges them, a query to its own envelope.
    pub fn canonical_view_uncharged(&self) -> usize {
        self.canonical_uncharged
    }

    /// Whether a triple term this snapshot's statements hold names, at any depth, another
    /// identity of a class than its representative. In the quotient that term and its
    /// representatives' are one, which no stored id stands for: canonical answers can't
    /// answer over such a snapshot (`nrese_sparql` refuses it). Triple terms are objects
    /// only and their ids one range: a seek in the object-first index, then each distinct
    /// triple term once; made once per version.
    pub fn triple_terms_name_aliases(&self) -> bool {
        let Some(same_as) = self.equality else {
            return false;
        };
        let classes = self.version.equality.classes(&self.version, same_as);
        self.version.canonical.triple_terms(same_as, || {
            let classes = &*classes;
            {
                let (low, high) = TermId::kind_range(crate::TermKind::Triple);
                let plan = AccessPlan {
                    permutation: Permutation::Ospg,
                    low: [low.raw(), 0, 0, 0],
                    high: [high.raw(), u64::MAX, u64::MAX, u64::MAX],
                    exclude_default_graph: false,
                };
                let mut seen = std::collections::HashSet::new();
                let data = self.data(ReadModel::Materialised);
                Stack::ALL.into_iter().any(|stack| {
                    let index = data.stack(stack);
                    let objects: Box<dyn Iterator<Item = EncodedQuad>> =
                        match index.layout().supports(Permutation::Ospg) {
                            true => Box::new(index.scan_plan(&plan)),
                            false => Box::new(index.scan(&QuadPattern::all())),
                        };
                    objects
                        .map(|quad| quad.object)
                        .filter(|object| object.kind() == crate::TermKind::Triple)
                        .any(|object| self.names_alias(object, classes, &mut seen))
                })
            }
        })
    }

    /// Whether the triple term `id` names an alias in a part, at any depth (`seen`: the
    /// triple terms already found not to).
    fn names_alias(
        &self,
        id: TermId,
        classes: &EqualityClasses,
        seen: &mut std::collections::HashSet<TermId>,
    ) -> bool {
        if !seen.insert(id) {
            return false;
        }
        let parts = match self.components(id) {
            Some(parts) => parts.to_vec(),
            // Keyed by its text (the first RDF 1.2 version): its parts by lookup.
            None => match self.decode(id) {
                Some(Term::Triple(t)) => [
                    Term::from(t.subject.clone()),
                    t.predicate.clone().into(),
                    t.object.clone(),
                ]
                .iter()
                .filter_map(|part| self.lookup(part.as_ref()))
                .collect(),
                _ => Vec::new(),
            },
        };
        parts.into_iter().any(|part| {
            classes.is_alias(part.raw())
                || (part.kind() == crate::TermKind::Triple && self.names_alias(part, classes, seen))
        })
    }

    /// The canonical view of `version` this snapshot's reads take, if it reads canonically.
    fn canonical_view(&self, version: &Arc<Version>) -> (Option<Arc<Version>>, usize) {
        match self.canonical {
            true => canonical_data(version, self.equality, &self.views),
            false => (None, 0),
        }
    }

    /// The version reads in `model` read: the canonical view for canonical reads in a
    /// model with inferred statements, else the snapshot's own.
    pub(crate) fn data(&self, model: ReadModel) -> &Arc<Version> {
        match &self.canonical_data {
            Some(view) if model != ReadModel::Asserted => view,
            _ => &self.version,
        }
    }

    /// Whether this snapshot reads equality classes canonically.
    pub fn reads_canonically(&self) -> bool {
        self.canonical
    }

    /// The equality classes by which reads in `model` name terms canonically: `Some` when
    /// this snapshot reads canonically, the model has inferred statements and some class
    /// exists. Under it a term stands for its class, shown by
    /// [`EqualityClasses::representative`]. That is the one mapping of canonical answers
    /// (research-designs §1, "Canonical answers"): every id such a read gives, from any
    /// graph, is a representative; a constant of a pattern, in every graph (the graph's
    /// name included), is normalised by it; every other id that meets those reads (a
    /// query's constants, a
    /// computed IRI, a federated row, an id an index keyed by stored ids gives, such as
    /// the spatial or text index) must be mapped by it too. A value leaving those reads
    /// for something that names terms as stored (a remote endpoint) goes as every
    /// identity of its class ([`EqualityClasses::members_of`]). The asserted model, and
    /// the snapshot read as stored ([`Self::stored`]), read the statements as stored.
    pub fn canonical_classes(&self, model: ReadModel) -> Option<Arc<EqualityClasses>> {
        if !self.canonical {
            return None;
        }
        self.expanding(model, &QuadPattern::in_graph(TermId::DEFAULT_GRAPH))?;
        self.equality_classes()
    }

    /// `id` as reads of the default graph in `model` name it ([`Self::canonical_classes`]):
    /// its class's representative under canonical reads, else `id`. For one-off lookups; a
    /// loop takes the classes once.
    pub fn canonical_id(&self, model: ReadModel, id: TermId) -> TermId {
        self.canonical_classes(model)
            .map_or(id, |classes| classes.representative(id))
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
        let version = Arc::new(Version {
            content: super::Content::fresh(),
            asserted: self.version.asserted.clone(),
            inferred,
            revision: self.version.revision,
            dictionary_len: self.version.dictionary_len,
            equality: Default::default(),
            canonical: Default::default(),
        });
        let (canonical_data, canonical_uncharged) = self.canonical_view(&version);
        Snapshot {
            canonical_data,
            canonical_uncharged,
            views: Arc::clone(&self.views),
            version,
            dictionary: Arc::clone(&self.dictionary),
            statistics: Arc::default(),
            equality: self.equality,
            canonical: self.canonical,
        }
    }

    /// This snapshot with `added` statements in its inferred stack, for a reader of facts
    /// the store keeps apart from its stacks (the `owl2-dl` mode's upper bound): never
    /// published, so no other reader sees them. Like [`Self::with_inferred_subset`] it has
    /// statistics and equality classes of its own. O(k log k) for k statements, as one
    /// more run.
    pub fn with_inferred_added(&self, added: &[EncodedQuad]) -> Snapshot {
        let version = Arc::new(Version {
            // Other statements than this snapshot's, at its revision.
            content: super::Content::fresh(),
            asserted: self.version.asserted.clone(),
            inferred: self.version.inferred.with_delta(added, &[]),
            revision: self.version.revision,
            dictionary_len: self.version.dictionary_len,
            equality: Default::default(),
            canonical: Default::default(),
        });
        let (canonical_data, canonical_uncharged) = self.canonical_view(&version);
        Snapshot {
            canonical_data,
            canonical_uncharged,
            views: Arc::clone(&self.views),
            version,
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
        let version = Arc::new(Version {
            content: super::Content::fresh(),
            asserted: self.version.asserted.clone(),
            inferred: stack.with_runs(&mask.runs),
            revision: self.version.revision,
            dictionary_len: self.version.dictionary_len,
            equality: Default::default(),
            canonical: Default::default(),
        });
        let (canonical_data, canonical_uncharged) = self.canonical_view(&version);
        Some(Snapshot {
            canonical_data,
            canonical_uncharged,
            views: Arc::clone(&self.views),
            version,
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

    /// `pattern` as a read in `model` takes it: under canonical reads, in a model with
    /// inferred statements, its constants (the graph's name included) by their
    /// representatives, in every graph, as those reads give representatives from every
    /// graph ([`Self::with_canonical_equality`]); else as given.
    pub(crate) fn read_pattern<'p>(
        &self,
        model: ReadModel,
        pattern: &'p QuadPattern,
    ) -> std::borrow::Cow<'p, QuadPattern> {
        use std::borrow::Cow;
        if !self.canonical || model == ReadModel::Asserted {
            return Cow::Borrowed(pattern);
        }
        match self.equality {
            Some(same_as) => Cow::Owned(
                self.version
                    .equality
                    .canonical(&self.version, same_as)
                    .normalised_everywhere(pattern),
            ),
            None => Cow::Borrowed(pattern),
        }
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
        let data = self.data(model);
        let normalised = classes.normalised(pattern);
        let plan = AccessPlan::in_permutation(&normalised, permutation)?;
        let supported = Stack::ALL
            .into_iter()
            .all(|stack| data.stack(stack).layout().supports(permutation));
        if !supported {
            return None;
        }
        // Named graphs are read as stored, with the pattern's own constants: from this
        // scan when the representatives are those constants, else from one of their own.
        // A canonical snapshot's named graphs are over representatives: from this scan.
        let named = model == ReadModel::Materialised && pattern.graph == GraphSelector::Any;
        let renamed = normalised != *pattern && !self.canonical;
        let named_plan = match named && renamed {
            true => Some(AccessPlan::in_permutation(pattern, permutation)?),
            false => None,
        };
        let stored = SortedMerge {
            left: data.asserted.scan_plan(&plan).peekable(),
            right: data.inferred.scan_plan(&plan).peekable(),
            permutation,
        };
        let expanded = Expand::new(
            stored,
            classes,
            *pattern,
            permutation,
            named && !renamed,
            model,
            data,
        );
        let named = named_plan
            .into_iter()
            .flat_map(move |plan| data.asserted.scan_plan(&plan))
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
        let data = self.data(model);
        let permutation = AccessPlan::for_pattern(&classes.normalised(pattern)).permutation;
        match self.expanded_sorted(Arc::clone(&classes), model, pattern, permutation) {
            Some(sorted) => Either::Left(sorted),
            None => {
                // A layout without the permutation: every match, then sorted.
                let mut all: Vec<EncodedQuad> = Vec::new();
                let normalised = classes.normalised(pattern);
                for stack in Stack::ALL {
                    for quad in data.stack(stack).scan(&normalised) {
                        if !quad.graph.is_default_graph() {
                            if model == ReadModel::Materialised
                                && (normalised == *pattern || self.canonical)
                            {
                                all.push(quad);
                            }
                            continue;
                        }
                        if !classes.is_canonical(&quad) {
                            continue;
                        }
                        let touching = classes.touches(&quad);
                        let visibility = CopyVisibility::new(data, model);
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
                    && !self.canonical
                {
                    all.extend(
                        data.asserted
                            .scan(pattern)
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
        let data = self.data(model);
        let normalised = classes.normalised(pattern);
        let visibility = CopyVisibility::new(data, model);
        let mut count = 0;
        for stack in Stack::ALL {
            for quad in data.stack(stack).scan(&normalised) {
                if !quad.graph.is_default_graph() {
                    count += u64::from(
                        model == ReadModel::Materialised
                            && (normalised == *pattern || self.canonical),
                    );
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
            && !self.canonical
        {
            count += self
                .data(model)
                .asserted
                .scan(pattern)
                .filter(|quad| !quad.graph.is_default_graph())
                .count() as u64;
        }
        count
    }

    /// An estimate of [`count_in`](Self::count_in) for planning, O(r log n): with
    /// equality classes, the stored matches without weighing the classes. Zero only if
    /// nothing matches.
    pub fn estimate_in(&self, model: ReadModel, pattern: &QuadPattern) -> u64 {
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        let Some(classes) = self.expanding(model, pattern) else {
            return self.count_in(model, pattern);
        };
        let stored = Snapshot {
            version: Arc::clone(self.data(model)),
            ..self.stored()
        };
        let normalised = classes.normalised(pattern);
        let mut estimate = stored.count_in(ReadModel::Materialised, &normalised);
        // Named graphs are read with the pattern's own constants (a canonical snapshot's,
        // over representatives, with the normalised ones).
        if normalised != *pattern && pattern.graph == GraphSelector::Any && !self.canonical {
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
        let version = Arc::new(version);
        let (canonical_data, canonical_uncharged) = self.canonical_view(&version);
        Snapshot {
            canonical_data,
            canonical_uncharged,
            views: Arc::clone(&self.views),
            version,
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

    /// Whether dictionary IDs denote the same terms in both snapshots. This is distinct
    /// from content identity: derived views can share a dictionary while reading different
    /// statements. It says nothing about query-local computed IDs.
    pub fn shares_dictionary_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.dictionary, &other.dictionary)
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
            .map(|stack| self.data(model).stack(stack).len())
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
        let named = self
            .read_pattern(
                model,
                &QuadPattern {
                    subject: Some(quad.subject),
                    predicate: Some(quad.predicate),
                    object: Some(quad.object),
                    graph: GraphSelector::Exact(quad.graph),
                },
            )
            .into_owned();
        let GraphSelector::Exact(graph) = named.graph else {
            unreachable!("an exact graph stays exact")
        };
        let quad = &EncodedQuad::new(
            named.subject.unwrap_or(quad.subject),
            named.predicate.unwrap_or(quad.predicate),
            named.object.unwrap_or(quad.object),
            graph,
        );
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
                .any(|stack| self.data(model).stack(stack).contains(&canonical));
            return stored
                && CopyVisibility::new(self.data(model), model)
                    .shows(quad, classes.touches(&canonical));
        }
        Stack::ALL
            .into_iter()
            .any(|stack| model.includes(stack) && self.data(model).stack(stack).contains(quad))
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        if let Some(classes) = self.expanding(model, pattern) {
            return Either::Left(self.expanded(classes, model, pattern));
        }
        let pattern = *pattern;
        let data = self.data(model);
        Either::Right(
            Stack::ALL
                .into_iter()
                .filter(move |&stack| model.includes(stack))
                .flat_map(move |stack| data.stack(stack).scan(&pattern)),
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
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
            .map(|stack| self.data(model).stack(stack).count_plan(&plan))
            .sum()
    }

    /// True if some quad matches `pattern` in `model`; cheaper than
    /// [`count_in`](Self::count_in)` > 0`.
    pub fn exists_in(&self, model: ReadModel, pattern: &QuadPattern) -> bool {
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        if let Some(classes) = self.expanding(model, pattern) {
            return self.expanded(classes, model, pattern).next().is_some();
        }
        if pattern.graph == GraphSelector::AnyNamed {
            return self.count_in(model, pattern) > 0;
        }
        let plan = AccessPlan::for_pattern(pattern);
        Stack::ALL
            .into_iter()
            .any(|stack| model.includes(stack) && self.data(model).stack(stack).any_plan(&plan))
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        if let Some(classes) = self.expanding(model, pattern) {
            return self
                .expanded_sorted(classes, model, pattern, permutation)
                .map(Either::Left);
        }
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        let supported = Stack::ALL.into_iter().all(|stack| {
            !model.includes(stack) || self.data(model).stack(stack).layout().supports(permutation)
        });
        if !supported {
            return None;
        }
        let scan = |stack: Stack| {
            model
                .includes(stack)
                .then(|| self.data(model).stack(stack).scan_plan(&plan))
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
                .then(|| self.data(model).stack(stack).scan_plan(&plan))
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
                .map(|stack| self.data(model).stack(stack).count_plan(&plan))
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
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
            !model.includes(stack) || self.data(model).stack(stack).layout().supports(permutation)
        });
        if !supported {
            return None;
        }
        let mut counts = Vec::new();
        let mut sources = 0;
        for stack in Stack::ALL {
            if model.includes(stack) {
                let before = counts.len();
                self.data(model)
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        if self.expanding(model, pattern).is_some() {
            return None;
        }
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        if plan.exclude_default_graph {
            return None;
        }
        if Stack::ALL.into_iter().any(|stack| {
            model.includes(stack) && !self.data(model).stack(stack).layout().supports(permutation)
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
            let index = self.data(model).stack(stack);
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
        let pattern = self
            .read_pattern(
                model,
                &QuadPattern {
                    graph: graphs,
                    ..QuadPattern::all()
                },
            )
            .into_owned();
        let graphs = pattern.graph;
        if self.expanding(model, &pattern).is_some()
            || (self.canonical_data.is_some() && model != ReadModel::Asserted)
        {
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
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
            !model.includes(stack) || self.data(model).stack(stack).layout().supports(permutation)
        });
        if bound >= 4 || !supported {
            return None;
        }
        let mut values = Vec::new();
        let mut sources = 0;
        for stack in Stack::ALL {
            if model.includes(stack) {
                let before = values.len();
                self.data(model)
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
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        self.statistics.distinct(self, model, pattern, permutation)
    }

    /// The characteristic sets of the default graph in `model`, for estimates of star
    /// joins ([`super::characteristic`]): kept (drifted ones while one rebuild replaces
    /// them), read, or built before this returns, the first time (a caller that arrives
    /// during that build waits for it; on a Rayon worker it gets `None`); `None` also
    /// where the graph's subjects have too many.
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
        self.named_graphs_in(ReadModel::Asserted)
    }

    /// [`Self::named_graphs`] as reads in `model` name them (canonical reads: by their
    /// representatives).
    pub fn named_graphs_in(&self, model: ReadModel) -> impl Iterator<Item = TermId> + '_ {
        let index = &self.data(model).asserted;
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

    /// The triple term `<<( subject predicate object )>>` of these ids as of this snapshot:
    /// one hash probe, nothing decoded. `None` if the snapshot has no such triple term (so
    /// no statement uses it).
    pub fn triple_term(
        &self,
        subject: TermId,
        predicate: TermId,
        object: TermId,
    ) -> Option<TermId> {
        self.dictionary
            .triple_term_bounded([subject, predicate, object], self.version.dictionary_len)
    }

    /// The subject, predicate and object ids of the triple term `id`, nothing decoded.
    /// `None` for another kind of term, one this snapshot doesn't know, and a triple term
    /// the first RDF 1.2 version keyed by its text ([`crate::Dictionary::components`]).
    pub fn components(&self, id: TermId) -> Option<[TermId; 3]> {
        if id.payload() >= self.version.dictionary_len {
            return None;
        }
        self.dictionary.components(id)
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

    /// [`Self::matching_strings`], and what its scan did ([`crate::StringScan`]).
    pub fn matching_strings_scan(
        &self,
        test: &crate::StringTest<'_>,
    ) -> (Vec<TermId>, crate::StringScan) {
        self.dictionary
            .matching_strings_scan(test, self.version.dictionary_len)
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

#[cfg(test)]
mod canonical_lifetime_tests {
    use super::*;
    use nrese_rdf::{GraphName, NamedNode};

    /// A canonical view lives no longer than its version, and is charged to the view
    /// budget while kept: once the engine and every snapshot are gone, nothing holds
    /// either and the charge is released (the view's cache once held its own version,
    /// through the equality cell the versions pass on). A view the budget refuses is made
    /// for its reader alone, which reads canonically through it.
    #[test]
    fn canonical_views_die_with_their_version() {
        let ex = |local: &str| NamedNode::new_unchecked(format!("http://example.com/{local}"));
        for (named, limit) in [(false, None), (true, None), (true, Some(1))] {
            let engine = crate::Engine::new(crate::EngineConfig::default()).unwrap();
            let views =
                Arc::new(limit.map_or_else(nrese_exec::Budget::unlimited, nrese_exec::Budget::new));
            engine.set_canonical_view_budget(Arc::clone(&views));
            let mut tx = engine.transaction();
            let rep = tx.intern(ex("Rep").as_ref().into());
            let alias = tx.intern(ex("Alias").as_ref().into());
            let same_as = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
            let same_as = tx.intern(same_as.as_ref().into());
            tx.insert(Quad::new(ex("Rep"), ex("p"), ex("o"), GraphName::DefaultGraph).as_ref());
            if named {
                tx.insert(Quad::new(ex("Alias"), ex("in"), ex("z"), ex("g")).as_ref());
            }
            tx.insert_inferred(crate::EncodedTriple {
                subject: alias,
                predicate: same_as,
                object: rep,
            });
            tx.commit().unwrap();
            engine.set_equality(Some(same_as));
            let snapshot = engine.snapshot();
            let canonical = snapshot.with_canonical_equality();
            assert!(!canonical.triple_terms_name_aliases());
            let version = Arc::downgrade(&canonical.version);
            let view = canonical.canonical_data.as_ref().map(Arc::downgrade);
            assert_eq!(view.is_some(), named);
            let kept = named && limit.is_none();
            assert_eq!(views.used() > 0, kept, "{named} {limit:?}");
            assert_eq!(canonical.canonical_view_uncharged() > 0, named && !kept);
            if named {
                let g = canonical.lookup(ex("g").as_ref().into()).unwrap();
                let pattern = QuadPattern::in_graph(g);
                let read: Vec<_> = canonical
                    .quads_for_pattern_in(ReadModel::Materialised, &pattern)
                    .collect();
                assert!(
                    read.iter().all(|q| q.subject == rep) && !read.is_empty(),
                    "{read:?}"
                );
            }
            drop((canonical, snapshot, engine));
            assert_eq!(version.strong_count(), 0, "named graphs: {named}");
            assert!(view.is_none_or(|view| view.strong_count() == 0));
            assert_eq!(
                views.used(),
                0,
                "the view's charge is released with its version"
            );
        }
    }
}
