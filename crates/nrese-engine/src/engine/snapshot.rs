//! Read-only, point-in-time view of the dataset.

use std::sync::Arc;

use nrese_rdf::{Quad, QuadRef, Term, TermRef};

use super::equality::{Classes as EqualityClasses, Expand, Visibility as CopyVisibility};
use super::statistics::Statistics;
use super::{ReadModel, Stack, Version};
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};
use crate::term::{Dictionary, TermId, TermKind};

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
    canonical_data: Option<super::equality::View>,
    /// What the canonical views versions keep are charged to, beside the readers' own
    /// budgets ([`crate::Engine::set_canonical_view_budget`]).
    views: Arc<nrese_exec::Budget>,
    /// The reader's budget, where it brought one ([`Self::reading_within`]).
    reads: Option<ReadBudget>,
}

/// A reader's budget for what its reads hold while they run, and the first charge it
/// refused ([`Snapshot::reading_within`]).
#[derive(Debug)]
pub(crate) struct Reads {
    budget: Arc<nrese_exec::Budget>,
    refused: std::sync::OnceLock<nrese_exec::BudgetExceeded>,
}

impl Reads {
    /// Charges `bytes`; false, the refusal kept, where the budget has no room.
    pub(crate) fn charge(&self, bytes: usize) -> bool {
        match self.budget.charge(bytes) {
            Ok(()) => true,
            Err(refused) => {
                let _ = self.refused.set(refused);
                false
            }
        }
    }

    pub(crate) fn release(&self, bytes: usize) {
        self.budget.release(bytes);
    }
}

/// A handle on one reader's [`Reads`], shared by its snapshots and their scans. The state's
/// own allocation is charged to the reader's budget before it is made, and released once
/// the last handle has freed it.
#[derive(Debug)]
pub(crate) struct ReadBudget(Option<Arc<Reads>>);

impl ReadBudget {
    /// The bytes of the shared state: the Arc's two counts and [`Reads`].
    const BYTES: usize = 2 * size_of::<usize>() + size_of::<Reads>();

    fn new(budget: Arc<nrese_exec::Budget>) -> Result<Self, nrese_exec::BudgetExceeded> {
        budget.charge(Self::BYTES)?;
        Ok(Self(Some(Arc::new(Reads {
            budget,
            refused: std::sync::OnceLock::new(),
        }))))
    }
}

impl Clone for ReadBudget {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl std::ops::Deref for ReadBudget {
    type Target = Reads;

    fn deref(&self) -> &Reads {
        self.0.as_ref().expect("a live handle")
    }
}

impl Drop for ReadBudget {
    /// The last handle frees the state, then releases its charge.
    fn drop(&mut self) {
        if let Some(Reads { budget, .. }) = self.0.take().and_then(Arc::into_inner) {
            budget.release(Self::BYTES);
        }
    }
}

/// The canonical view of `version` under `same_as` ([`Snapshot::canonical_data`]): made on
/// `views` and kept, else on `fallback` for this reader ([`super::equality::CanonicalCache`]);
/// `None` where it is `version` itself.
fn canonical_data(
    version: &Arc<Version>,
    same_as: Option<TermId>,
    views: &Arc<nrese_exec::Budget>,
    fallback: &Arc<nrese_exec::Budget>,
) -> Result<Option<super::equality::View>, nrese_exec::BudgetExceeded> {
    match same_as {
        Some(same_as) => super::equality::CanonicalCache::view(version, same_as, views, fallback),
        None => Ok(None),
    }
}

/// An unlimited budget, where a reader brings none: one for the process.
fn unlimited() -> &'static Arc<nrese_exec::Budget> {
    static UNLIMITED: std::sync::OnceLock<Arc<nrese_exec::Budget>> = std::sync::OnceLock::new();
    UNLIMITED.get_or_init(|| Arc::new(nrese_exec::Budget::unlimited()))
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
            let inferred = &snapshot.version.inferred;
            runs.push(Arc::new(crate::index::run::Run::from_delta(
                inferred.layout(),
                inferred.encoding(),
                show,
                hide,
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
            views,
            reads: None,
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
            ..self.clone()
        }
    }

    /// This snapshot charging what its reads hold while they run to `budget` (a query's): the
    /// copies an equality expansion holds until they are in order ([`Expand`]). A read the
    /// budget has no room for stops, and the refusal is kept: [`Self::read_refused`] tells the
    /// reader, which must not use what it read then. Without one, reads charge nothing. The
    /// reader's shared state is charged to `budget` too: refused where it has no room for it.
    pub fn reading_within(
        &self,
        budget: Arc<nrese_exec::Budget>,
    ) -> Result<Snapshot, nrese_exec::BudgetExceeded> {
        Ok(Snapshot {
            reads: Some(ReadBudget::new(budget)?),
            ..self.clone()
        })
    }

    /// The first charge a read refused since [`Self::reading_within`]: what was read since
    /// may be incomplete.
    pub fn read_refused(&self) -> Option<nrese_exec::BudgetExceeded> {
        self.reads.as_ref()?.refused.get().copied()
    }

    /// This snapshot reading the statements as stored: no equality expansion.
    pub fn stored(&self) -> Snapshot {
        Snapshot {
            equality: None,
            canonical: false,
            canonical_data: None,
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
        self.with_canonical_equality_within(unlimited())
            .unwrap_or_else(|_| unreachable!("an unlimited budget refuses nothing"))
    }

    /// [`Self::with_canonical_equality`], its view made on `fallback`, the reader's own
    /// budget (a query's envelope), where the view budget refuses it: then the view is this
    /// snapshot's alone, and holds its charge to `fallback` until the snapshot goes. An
    /// error where `fallback` refuses it too, before anything is made.
    pub fn with_canonical_equality_within(
        &self,
        fallback: &Arc<nrese_exec::Budget>,
    ) -> Result<Snapshot, nrese_exec::BudgetExceeded> {
        Ok(Snapshot {
            canonical: true,
            canonical_data: canonical_data(&self.version, self.equality, &self.views, fallback)?,
            ..self.clone()
        })
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
    fn canonical_view(&self, version: &Arc<Version>) -> Option<super::equality::View> {
        match self.canonical {
            true => canonical_data(version, self.equality, &self.views, unlimited())
                .unwrap_or_else(|_| unreachable!("an unlimited budget refuses nothing")),
            false => None,
        }
    }

    /// The version reads in `model` read: the canonical view for canonical reads in a
    /// model with inferred statements, else the snapshot's own.
    pub(crate) fn data(&self, model: ReadModel) -> &Arc<Version> {
        match &self.canonical_data {
            Some(view) if model != ReadModel::Asserted => view.version(),
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
                let inferred = &self.version.inferred;
                crate::index::IndexVersion::from_quads(inferred.layout(), inferred.encoding(), kept)
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
        let canonical_data = self.canonical_view(&version);
        Snapshot {
            canonical_data,
            views: Arc::clone(&self.views),
            reads: self.reads.clone(),
            version,
            dictionary: Arc::clone(&self.dictionary),
            statistics: Arc::new(self.statistics.for_view()),
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
        // Other statements than this snapshot's, at its revision.
        let content = super::Content::fresh();
        let touched = added
            .iter()
            .map(|quad| (quad.subject, quad.predicate, quad.object));
        self.statistics
            .corpora
            .record(self.version.content, content, touched);
        let version = Arc::new(Version {
            content,
            asserted: self.version.asserted.clone(),
            inferred: self.version.inferred.with_delta(added, &[]),
            revision: self.version.revision,
            dictionary_len: self.version.dictionary_len,
            equality: Default::default(),
            canonical: Default::default(),
        });
        let canonical_data = self.canonical_view(&version);
        Snapshot {
            canonical_data,
            views: Arc::clone(&self.views),
            reads: self.reads.clone(),
            version,
            dictionary: Arc::clone(&self.dictionary),
            statistics: Arc::new(self.statistics.for_view()),
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
        let canonical_data = self.canonical_view(&version);
        Some(Snapshot {
            canonical_data,
            views: Arc::clone(&self.views),
            reads: self.reads.clone(),
            version,
            dictionary: Arc::clone(&self.dictionary),
            statistics: Arc::new(self.statistics.for_view()),
            equality: self.equality,
            canonical: self.canonical,
        })
    }

    /// Notes that this view of `base` (made by [`Self::with_inferred_subset`] or
    /// [`Self::with_inferred_mask`]) is a reader's `earlier` view of `earlier_base` brought
    /// forward: the commits between the bases, and `changed` (the inferred statements it
    /// shows or hides now and didn't then), are all that differ. Full-text searches on it
    /// then derive their corpora from the earlier view's ([`super::corpora`]) instead of
    /// walking the store again. Nothing is noted where the engine no longer has the
    /// commits between the bases.
    pub fn continues(
        &self,
        base: &Snapshot,
        earlier: &Snapshot,
        earlier_base: &Snapshot,
        changed: &[EncodedQuad],
    ) {
        self.statistics.corpora.record_view(
            earlier.version.content,
            earlier_base.version.content,
            self.version.content,
            base.version.content,
            changed,
        );
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
        )
        .charging(self.reads.clone());
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

    /// The expanded matches of `pattern` in `model`, in the order of its access plan,
    /// merged as they are read: nothing is collected. Every layout keeps the permutation a
    /// pattern's access plan reads (`Layout::supports`, tested over every pattern shape).
    fn expanded<'a>(
        &'a self,
        classes: Arc<EqualityClasses>,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a> {
        let permutation = AccessPlan::for_pattern(&classes.normalised(pattern)).permutation;
        self.expanded_sorted(classes, model, pattern, permutation)
            .expect("every layout keeps the permutation of a pattern's access plan")
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
        let canonical_data = self.canonical_view(&version);
        Snapshot {
            canonical_data,
            views: Arc::clone(&self.views),
            reads: self.reads.clone(),
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
            !model.includes(stack) || self.data(model).stack(stack).layout().supports(permutation)
        });
        if bound >= 4 || plan.exclude_default_graph || !supported {
            return None;
        }
        plan.low[bound] = low.raw();
        plan.high[bound] = high.raw();
        Some(plan)
    }

    /// The number of distinct values of the first unbound component of `pattern` in
    /// `permutation`'s order among its matches in `model`: exact, counted while the index's
    /// groups are walked without keeping them where one run of one stack holds the matches
    /// and no deletion among them, else from [`Self::group_counts_in`]. `None` where that
    /// can't answer. `COUNT(DISTINCT ?x)` of one triple pattern.
    pub fn distinct_count_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<u64> {
        if self.expanding(model, pattern).is_some() {
            return None;
        }
        let plan = AccessPlan::in_permutation(pattern, permutation)?;
        let bound = plan
            .low
            .iter()
            .zip(&plan.high)
            .take_while(|(l, h)| l == h)
            .count();
        let supported = Stack::ALL.into_iter().all(|stack| {
            !model.includes(stack) || self.version.stack(stack).layout().supports(permutation)
        });
        if plan.exclude_default_graph || bound >= 4 || !supported {
            return None;
        }
        let with: Vec<Stack> = Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack) && self.version.stack(stack).any_plan(&plan))
            .collect();
        let counted = match with.as_slice() {
            [] => Some(0),
            [stack] => self.version.stack(*stack).count_groups(&plan, bound),
            _ => None,
        };
        counted.or_else(|| {
            self.group_counts_in(model, pattern, permutation)
                .map(|groups| groups.len() as u64)
        })
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
        self.group_counts_of(model, &plan)
    }

    /// [`Self::group_counts_in`] for `plan`: grouped by its first component not bound to
    /// one value (the whole component, or a range of it: [`Self::range_plan`]).
    fn group_counts_of(&self, model: ReadModel, plan: &AccessPlan) -> Option<Vec<(TermId, u64)>> {
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
            !model.includes(stack)
                || self
                    .data(model)
                    .stack(stack)
                    .layout()
                    .supports(plan.permutation)
        });
        if !supported {
            return None;
        }
        let plan = *plan;
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
    /// during that build helps with it and waits for it, on a Rayon worker too); `None`
    /// also where the graph's subjects have too many, or the build failed.
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
    /// [`Self::lookup`], what it allocates charged first through `charge`
    /// ([`Dictionary::lookup_charged`]: the bound and the lookup read one state of the
    /// dictionary, which a checkpoint may otherwise change between them).
    pub fn lookup_charged<E>(
        &self,
        term: TermRef<'_>,
        charge: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Option<TermId>, E> {
        self.dictionary
            .lookup_charged(term, self.version.dictionary_len, charge)
    }

    /// The most bytes a read of this snapshot in `model` holds while it runs
    /// ([`Self::quads_for_pattern_in`] without equality expansion): each stack's merge,
    /// its cursors and a decoded block per run. What a charged reader reserves first. The
    /// runs are this snapshot's own, which no checkpoint changes. An expansion holds its
    /// pending copies besides.
    pub fn scan_bytes(&self, model: ReadModel) -> usize {
        let data = self.data(model);
        Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack))
            .map(|stack| data.stack(stack).scan_bytes())
            .sum()
    }

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

    /// [`Self::decode`] of the dictionary term `id`, what it allocates charged first
    /// through `charge` ([`Dictionary::decode_charged`]: the bound and the decoding read one
    /// state of the dictionary). `Ok(None)` for inline ids, triple terms and ids this
    /// snapshot doesn't know.
    pub fn decode_charged<E>(
        &self,
        id: TermId,
        charge: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Option<Term>, E> {
        if id.kind().is_dictionary() && id.payload() >= self.version.dictionary_len {
            return Ok(None);
        }
        self.dictionary.decode_charged(id, charge)
    }

    /// An upper bound of the bytes [`Self::decode`] allocates for `id`, for the dictionary
    /// as stored now: a checkpoint may store the term compressed after, so a charge for a
    /// decode goes through [`Self::decode_charged`]. `None` for inline ids, triple terms and
    /// ids this snapshot doesn't know.
    pub fn decode_bound(&self, id: TermId) -> Option<usize> {
        if id.kind().is_dictionary() && id.payload() >= self.version.dictionary_len {
            return None;
        }
        self.dictionary.decode_bound(id)
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

    /// The string literals that are objects of statements this snapshot shows in
    /// `scope`'s graphs in `model` (as its own scans do, equality classes expanded or
    /// canonical as it reads them) and match `query`, best first, ranked among those
    /// literals alone ([`Self::text_corpus`]): no literal outside them (another version's,
    /// an open transaction's, a graph's outside the scope, one no visible statement has)
    /// changes a score. What building the corpus takes is charged to `budget` while the
    /// search holds it (the request's; `None`: unbounded), and what scoring takes, the
    /// matches until they drop; a kept corpus is the engine's. Malformed or unsupported
    /// operators return [`crate::EngineError::InvalidTextQuery`], a budget that has no room
    /// [`crate::EngineError::MemoryLimit`].
    pub fn text_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::TextQuery,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> crate::EngineResult<crate::TextMatches> {
        let unbounded;
        let request = match budget {
            Some(budget) => budget,
            None => {
                unbounded = Arc::new(nrese_exec::Budget::unlimited());
                &unbounded
            }
        };
        // The corpus, the index covering it, then the search: no lock held across
        // another's parallel parts ([`Dictionary::cover_text`](crate::Dictionary)).
        let corpus = self.text_corpus(Documents::Literals, model, scope, Some(request))?;
        self.dictionary.text_search(query, &corpus, request)
    }

    /// [`Self::text_search_in`] over every graph, asserted and inferred, unbounded.
    pub fn text_search(&self, query: &crate::TextQuery) -> crate::EngineResult<crate::TextMatches> {
        self.text_search_in(ReadModel::Materialised, &TextScope::EVERY, query, None)
    }

    /// The vector literals that are objects of statements this snapshot shows in `scope`'s
    /// graphs in `model` (its corpus of vectors, [`Self::text_corpus`]), among those
    /// `accept` takes, nearest to `query.vector` first (ties by id), with how the search
    /// went. Every one of them is compared (exact): no vector outside them (another
    /// version's, an open transaction's, a deleted one, one in a graph outside the scope)
    /// changes the answer. Building the corpus and the scan are charged to `budget` as
    /// [`Self::text_search_in`]'s are; refused before it allocates past
    /// [`crate::VectorQuery::max_hits`] or the budget, stopped by its `stop` flag
    /// ([`crate::VectorStop`]).
    pub fn vector_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::VectorQuery,
        accept: &(dyn Fn(TermId) -> bool + Sync),
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<(crate::VectorHits, crate::VectorSearchReport), crate::VectorStop> {
        let unbounded;
        let request = match budget {
            Some(budget) => budget,
            None => {
                unbounded = Arc::new(nrese_exec::Budget::unlimited());
                &unbounded
            }
        };
        // The corpus, the vectors covering it, then the scan: no lock held across another's
        // parallel parts.
        let corpus = self
            .text_corpus(Documents::Vectors, model, scope, Some(request))
            .map_err(crate::VectorStop::Memory)?;
        // The corpus decides what the search admits, holds and reports; `accept` narrows
        // its hits.
        let eligible = |id: TermId| corpus.contains(id.payload());
        let bound = usize::try_from(corpus.documents()).unwrap_or(usize::MAX);
        self.dictionary.vector_search(
            query,
            self.version.dictionary_len,
            &eligible,
            bound,
            accept,
            request,
        )
    }

    /// [`Self::vector_search_in`] over every graph, asserted and inferred.
    pub fn vector_search(
        &self,
        query: &crate::VectorQuery,
        accept: &(dyn Fn(TermId) -> bool + Sync),
    ) -> Result<(crate::VectorHits, crate::VectorSearchReport), crate::VectorStop> {
        self.vector_search_in(
            ReadModel::Materialised,
            &TextScope::EVERY,
            query,
            accept,
            None,
        )
    }

    /// Prepares what [`Self::vector_search`] reads before returning (a warm-up): the vectors
    /// of the dictionary and the corpus of every graph's. Refused
    /// ([`crate::EngineError::MemoryLimit`]) where the reader's budget refused a read
    /// ([`Self::reading_within`]): what it read may be incomplete, so nothing is kept.
    pub fn prepare_vector_search(&self) -> crate::EngineResult<()> {
        self.text_corpus(
            Documents::Vectors,
            ReadModel::Materialised,
            &TextScope::EVERY,
            None,
        )?;
        Ok(())
    }

    /// The IRIs that are subjects of statements this snapshot shows in `scope`'s graphs
    /// in `model` and whose local names match `query`, best first, ranked among those IRIs
    /// alone (as [`Self::text_search_in`] ranks literals, `budget` as there). Read
    /// canonically, a class is its representative, named by every identity: each
    /// identity's local name is a document while the representative is a subject, and a
    /// match is answered as the representative, each once, at its best identity's
    /// relevance ([`Self::corpus_classes`]).
    pub fn iri_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::TextQuery,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> crate::EngineResult<crate::TextMatches> {
        let unbounded;
        let request = match budget {
            Some(budget) => budget,
            None => {
                unbounded = Arc::new(nrese_exec::Budget::unlimited());
                &unbounded
            }
        };
        let corpus = self.text_corpus(Documents::Subjects, model, scope, Some(request))?;
        let mut found = self.dictionary.iri_search(query, &corpus, request)?;
        if let Some((classes, true)) = self.corpus_classes(Documents::Subjects, model) {
            found.as_representatives(|id| classes.representative(TermId::from_raw(id)).raw());
        }
        Ok(found)
    }

    /// The equality classes a corpus of `documents` in `model` reads by, and whether reads
    /// are canonical: IRIs' corpora of a reader of equality classes in a model with
    /// inferred statements, where a class of two or more identities exists. Expanded, its
    /// scans show every identity of a class with a statement; canonical, its scans show
    /// the representative, and its corpus holds every identity of a class whose
    /// representative it holds (what a probe of an identity, normalised to the
    /// representative, finds). Either way a statement touching one identity may change
    /// every identity's membership.
    fn corpus_classes(
        &self,
        documents: Documents,
        model: ReadModel,
    ) -> Option<(Arc<EqualityClasses>, bool)> {
        if documents != Documents::Subjects || model == ReadModel::Asserted {
            return None;
        }
        let same_as = self.equality?;
        let classes = self.version.equality.classes(&self.version, same_as);
        (!classes.is_empty()).then_some((classes, self.canonical))
    }

    /// The full-text corpora this snapshot's engine keeps, and how many were built by a
    /// walk or derived from an ancestor's ([`super::corpora`]; performance.md §0).
    pub fn text_corpus_stats(&self) -> super::TextCorpusStats {
        self.statistics.corpora.stats()
    }

    /// The number of documents and words of [`Self::text_search_in`]'s corpus for `model`
    /// and `scope` (with `iris`, of [`Self::iri_search_in`]'s), built (or found kept) as a
    /// search does, charged to `budget` as there: what a search's corpus takes, without its
    /// scoring.
    #[doc(hidden)]
    pub fn text_corpus_size(
        &self,
        model: ReadModel,
        scope: &TextScope,
        iris: bool,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<(u64, u64), nrese_exec::BudgetExceeded> {
        let documents = match iris {
            false => Documents::Literals,
            true => Documents::Subjects,
        };
        let corpus = self.text_corpus(documents, model, scope, budget)?;
        Ok((corpus.documents(), corpus.total_words()))
    }

    /// The documents of [`Self::text_search_in`]'s corpus for `model` and `scope` (with
    /// `iris`, of [`Self::iri_search_in`]'s): dictionary entries ascending, their number and
    /// their words, as a search finds them (kept, derived or walked). For checks against
    /// the statements this snapshot's scans show.
    #[doc(hidden)]
    pub fn text_corpus_members(
        &self,
        model: ReadModel,
        scope: &TextScope,
        iris: bool,
    ) -> (Vec<u64>, u64, u64) {
        let documents = match iris {
            false => Documents::Literals,
            true => Documents::Subjects,
        };
        let corpus = self
            .text_corpus(documents, model, scope, None)
            .expect("no budget to exceed");
        (corpus.members(), corpus.documents(), corpus.total_words())
    }

    /// The members of [`Self::vector_search_in`]'s corpus for `model` and `scope`: the
    /// typed literals that are objects of statements this snapshot shows there (the scan
    /// reads the vectors among them).
    #[doc(hidden)]
    pub fn vector_corpus_members(&self, model: ReadModel, scope: &TextScope) -> Vec<u64> {
        self.text_corpus(Documents::Vectors, model, scope, None)
            .expect("no budget to exceed")
            .members()
    }

    /// The documents of a full-text index this snapshot shows in `scope`, with their
    /// statistics from the index (which covers them: [`Dictionary::cover_text`] first);
    /// exact, kept by the statements' identity, model, graphs and how this snapshot reads
    /// equality ([`super::corpora`]). Membership is what this snapshot's own scans show
    /// (equality classes expanded or canonical, the visibility of their copies), never
    /// the statements as stored. Derived from the nearest kept ancestor's where the
    /// lineage has one: each term the steps since touched is probed, a member while a
    /// visible statement in the scope has it (an IRI of a class: every identity of it,
    /// [`Self::corpus_classes`]); else walked ([`Self::walk_corpus`]), and after a change
    /// of a `sameAs` statement (the classes change). The probes and the walk hold no lock
    /// of the dictionary (the walk runs on every core); only the counting does, on this
    /// thread.
    /// Nobody waits for another's build: a search that finds one under way builds its own
    /// and doesn't keep it. What the build takes, and a corpus not kept, is charged to
    /// `budget`, returned as the reservation the search holds until it is done.
    fn text_corpus(
        &self,
        documents: Documents,
        model: ReadModel,
        scope: &TextScope,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<super::corpora::CorpusRef, nrese_exec::BudgetExceeded> {
        use super::corpora::{CorpusRef, Found};
        use crate::term::text::{Bits, Corpus};
        let unbounded;
        let request = match budget {
            Some(budget) => budget,
            None => {
                unbounded = Arc::new(nrese_exec::Budget::unlimited());
                &unbounded
            }
        };
        // Copied only where it isn't written the one way yet, charged then; the charge
        // bound first, as bindings drop in reverse: the copy goes before its charge.
        let (_scope_charge, scope) = scope.normalised(request)?;
        let scope = &*scope;
        let store = &self.statistics.corpora;
        let kind = super::corpora::KindRef {
            documents,
            model,
            scope,
            equality: self.equality,
            canonical: self.canonical,
        };
        let (from, keep) = match store.find(&self.version, kind, request)? {
            Found::Kept(corpus) => {
                self.cover(documents);
                return Ok(CorpusRef::Kept(corpus));
            }
            Found::Build { from, keep } => (from, keep),
        };
        let iris = documents == Documents::Subjects;
        let vectors = documents == Documents::Vectors;
        // A touched IRI of a class stands for every identity of it (literals belong to no
        // class): each is probed.
        let classes = self.corpus_classes(documents, model);
        let span = self.version.dictionary_len;
        let built = match from {
            Some((base, touched)) => {
                self.cover(documents);
                let mut changes =
                    nrese_exec::ChargedVec::with_capacity(Arc::clone(request), touched.len())?;
                for &raw in touched.iter() {
                    let identities = match &classes {
                        Some((classes, _)) => classes
                            .class_of(TermId::from_raw(raw))
                            .unwrap_or(std::slice::from_ref(&raw)),
                        None => std::slice::from_ref(&raw),
                    };
                    for id in identities.iter().copied().map(TermId::from_raw) {
                        if documents.holds(id.kind()) && id.payload() < span {
                            let used = self.uses(model, scope, documents, id);
                            changes.push((id.payload(), used))?;
                        }
                    }
                }
                store.built(true);
                match vectors {
                    // Every typed literal is a member (of one word): the scan reads the
                    // vectors among them.
                    true => Bits::derived(&base, &changes, span, request, |_| 1)?,
                    false => self.dictionary.with_text_index(iris, |index| {
                        index.derive(&base, &changes, span, request)
                    })?,
                }
            }
            None => {
                store.built(false);
                self.walk_corpus(documents, model, scope, request)?
            }
        };
        // A read the reader's budget refused stopped early ([`Self::read_refused`]): what it
        // built may miss members, so it is neither kept nor used.
        if let Some(refused) = self.read_refused() {
            return Err(refused);
        }
        // Kept: built on the store's budget. Else the search's, on its own.
        let (_, bytes) = Corpus::finished_bytes(built.documents, span);
        let kept = keep
            .then(|| {
                store.keep_built(&self.version, kind, bytes, |budget| {
                    Corpus::finish(&built, span, budget)
                })
            })
            .flatten();
        Ok(match kept {
            Some(corpus) => CorpusRef::Kept(corpus),
            None => CorpusRef::Own(Corpus::finish(&built, span, request)?),
        })
    }

    /// The corpus by a walk of the statements this snapshot shows in the documents'
    /// kinds' ranges ([`Self::mark_values`]), with no lock of the dictionary held; then
    /// counted under the index's, on this thread. Its bits, and the most the walk's
    /// slices hold at once, are charged to `budget` first.
    fn walk_corpus(
        &self,
        documents: Documents,
        model: ReadModel,
        scope: &TextScope,
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<crate::term::text::Bits, nrese_exec::BudgetExceeded> {
        let span = self.version.dictionary_len;
        let words = span.div_ceil(64) as usize;
        let mut bits = nrese_exec::ChargedVec::with_capacity(Arc::clone(budget), words)?;
        bits.resize(words, 0)?;
        let mut scratch = nrese_exec::Reservation::new(Arc::clone(budget));
        let (running, bytes) = self.walk_scratch(words, budget.remaining());
        scratch.charge(bytes)?;
        // Per graph, each kind's walk; a scan (where the index can't walk them) marks every
        // kind at once. Only the count needs the index: on a pool thread it is covered
        // while the statements are walked (the cover, on this thread, as it would be
        // alone); elsewhere first.
        let marks: &mut [u64] = &mut bits;
        let mut walk = move || {
            let mut mark = |graph: GraphSelector| {
                for &kind in documents.kinds() {
                    if self.mark_values(model, graph, documents, kind, marks, running) {
                        break;
                    }
                }
            };
            match scope {
                TextScope::Graphs(graphs) => mark(*graphs),
                TextScope::Merge(graphs) => {
                    for &graph in graphs {
                        mark(GraphSelector::Exact(graph));
                    }
                }
            }
        };
        match rayon::current_thread_index() {
            Some(_) => {
                rayon::join(|| self.cover(documents), walk);
            }
            None => {
                self.cover(documents);
                walk();
            }
        }
        drop(scratch);
        // Read canonically, a class whose representative is a member: each identity too.
        if let Some((classes, true)) = self.corpus_classes(documents, model) {
            let span = self.version.dictionary_len;
            for (representative, identities) in classes.iter() {
                // Each identity is of one class: setting a class's bits sets no other
                // representative's.
                let representative = TermId::from_raw(representative);
                let at = representative.payload();
                let member = documents.holds(representative.kind())
                    && bits
                        .get((at / 64) as usize)
                        .is_some_and(|word| word >> (at % 64) & 1 == 1);
                if !member {
                    continue;
                }
                for id in identities.iter().copied().map(TermId::from_raw) {
                    if documents.holds(id.kind()) && id.payload() < span {
                        let at = id.payload();
                        bits[(at / 64) as usize] |= 1 << (at % 64);
                    }
                }
            }
        }
        Ok(match documents {
            Documents::Vectors => crate::term::text::Bits::counted(bits, |_| 1),
            Documents::Literals => self
                .dictionary
                .with_text_index(false, |index| index.corpus(bits)),
            Documents::Subjects => self
                .dictionary
                .with_text_index(true, |index| index.corpus(bits)),
        })
    }

    /// Extends the index of `documents` (the vectors, for vector literals) over the
    /// dictionary's entries.
    fn cover(&self, documents: Documents) {
        match documents {
            Documents::Literals => self.dictionary.cover_text(false),
            Documents::Subjects => self.dictionary.cover_text(true),
            Documents::Vectors => self.dictionary.cover_vectors(),
        }
    }

    /// How many slices of a corpus walk over `words` words of bits run at once, and the most
    /// they hold besides the bits: per slice its group counts (16 bytes a term, 64 terms a
    /// word of the slice) from each run, and as many again where they are merged or
    /// compared. As many at once as `room` holds, at least one, no more than the pool's
    /// threads nor than there are slices.
    fn walk_scratch(&self, words: usize, room: usize) -> (usize, usize) {
        let runs = self.version.runs().count().max(1);
        let per = slice_words(words);
        let slice = per * 64 * 16 * runs * 2;
        let most = words.div_ceil(per).min(rayon::current_num_threads()).max(1);
        let running = (room / slice).clamp(1, most);
        (running, running * slice)
    }

    /// Whether a statement this snapshot shows in `scope`'s graphs in `model` has `id` at
    /// `documents`' position (object or subject): a probe as its scans read (equality
    /// expanded or canonical), or for a merge of many graphs a scan of the term's
    /// statements.
    fn uses(&self, model: ReadModel, scope: &TextScope, documents: Documents, id: TermId) -> bool {
        let pattern = |graph| match documents {
            Documents::Literals | Documents::Vectors => QuadPattern {
                object: Some(id),
                graph,
                ..QuadPattern::all()
            },
            Documents::Subjects => QuadPattern {
                subject: Some(id),
                graph,
                ..QuadPattern::all()
            },
        };
        match scope {
            TextScope::Graphs(graphs) => self.exists_in(model, &pattern(*graphs)),
            TextScope::Merge(graphs) if graphs.len() <= 8 => graphs
                .iter()
                .any(|&graph| self.exists_in(model, &pattern(GraphSelector::Exact(graph)))),
            TextScope::Merge(graphs) => self
                .quads_for_pattern_in(model, &pattern(GraphSelector::Any))
                .any(|quad| graphs.binary_search(&quad.graph).is_ok()),
        }
    }

    /// Sets the bit of the dictionary entry of each term of `kind` at `documents`' position
    /// (object or subject) of the statements in `graphs` (one graph, every graph, or every
    /// named graph) in `model`. The entries in slices of whole words, `running` at once on
    /// the pool: each a walk over the index's groups in its range of ids, so no list of
    /// every term is held.
    /// Where the index can't walk them (a reader of equality classes), one scan of the
    /// statements marks the terms of every kind of `documents`: then `true`.
    fn mark_values(
        &self,
        model: ReadModel,
        graphs: GraphSelector,
        documents: Documents,
        kind: TermKind,
        bits: &mut [u64],
        running: usize,
    ) -> bool {
        use rayon::prelude::*;
        let per = slice_words(bits.len());
        let wave = |at: usize, wave: &mut [u64]| {
            wave.par_chunks_mut(per)
                .enumerate()
                .map(|(i, words)| {
                    self.dictionary.part_started();
                    let first = ((at + i) * per * 64) as u64;
                    let last = first + words.len() as u64 * 64 - 1;
                    let (low, high) = (TermId::new(kind, first), TermId::new(kind, last));
                    let counts = |graphs| self.range_counts(model, graphs, documents, low, high);
                    let mut mark = |value: TermId| {
                        let at = value.payload() - first;
                        words[(at / 64) as usize] |= 1 << (at % 64);
                    };
                    match graphs {
                        GraphSelector::AnyNamed => {
                            // Every graph's, less those only the default graph has.
                            let every = counts(GraphSelector::Any)?;
                            let default = counts(GraphSelector::Exact(TermId::DEFAULT_GRAPH))?;
                            let mut j = 0;
                            for (value, count) in every {
                                while j < default.len() && default[j].0 < value {
                                    j += 1;
                                }
                                let in_default = match default.get(j) {
                                    Some(&(v, c)) if v == value => c,
                                    _ => 0,
                                };
                                if count > in_default {
                                    mark(value);
                                }
                            }
                        }
                        _ => counts(graphs)?
                            .into_iter()
                            .for_each(|(value, _)| mark(value)),
                    }
                    Some(())
                })
                .collect::<Option<()>>()
        };
        // In waves of `running` slices; one the index can't walk (as none of them then)
        // goes to the scan.
        let walked = bits
            .chunks_mut(per * running)
            .enumerate()
            .try_for_each(|(w, words)| wave(w * running, words));
        if walked.is_some() {
            return false;
        }
        let pattern = QuadPattern {
            graph: graphs,
            ..QuadPattern::all()
        };
        for quad in self.quads_for_pattern_in(model, &pattern) {
            let value = match documents {
                Documents::Literals | Documents::Vectors => quad.object,
                Documents::Subjects => quad.subject,
            };
            if documents.holds(value.kind()) {
                let at = value.payload();
                bits[(at / 64) as usize] |= 1 << (at % 64);
            }
        }
        true
    }

    /// Per distinct term in `low..=high` at `documents`' position of the statements in
    /// `graphs` (one graph, or every graph) in `model`, how many there are, in id order: a
    /// walk over the index's groups. `None` where the index can't walk them.
    fn range_counts(
        &self,
        model: ReadModel,
        graphs: GraphSelector,
        documents: Documents,
        low: TermId,
        high: TermId,
    ) -> Option<Vec<(TermId, u64)>> {
        let (pattern, permutation) = match (graphs, documents) {
            (GraphSelector::Exact(graph), Documents::Literals | Documents::Vectors) => {
                (QuadPattern::in_graph(graph), Permutation::Gosp)
            }
            (GraphSelector::Exact(graph), Documents::Subjects) => {
                (QuadPattern::in_graph(graph), Permutation::Gspo)
            }
            (GraphSelector::Any, Documents::Literals | Documents::Vectors) => {
                (QuadPattern::all(), Permutation::Ospg)
            }
            (GraphSelector::Any, Documents::Subjects) => (QuadPattern::all(), Permutation::Spog),
            (GraphSelector::AnyNamed, _) => return None,
        };
        let plan = self.range_plan(model, &pattern, permutation, low, high)?;
        self.group_counts_of(model, &plan)
    }
}

/// Words of the bits each slice of a walk over `words` covers: four slices a thread of the
/// pool, at most [`SLICE_WORDS`].
fn slice_words(words: usize) -> usize {
    words
        .div_ceil(rayon::current_num_threads() * 4)
        .clamp(1, SLICE_WORDS)
}

/// Words of the bits a slice of a corpus walk covers at most ([`Snapshot::mark_values`]):
/// its group counts (16 bytes a term) are what the slice holds besides the bits.
const SLICE_WORDS: usize = 1024;

/// The graphs whose statements make the corpus of a full-text search
/// ([`Snapshot::text_search_in`]): the literals that are their objects (the IRIs that are
/// their subjects, for [`Snapshot::iri_search_in`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TextScope {
    /// One graph, every graph, or every named graph.
    Graphs(GraphSelector),
    /// The merge of these graphs (sorted, without repeats): a dataset's default graph of
    /// several graphs, or the graphs a reader may read.
    Merge(Vec<TermId>),
}

impl TextScope {
    /// Every graph.
    pub const EVERY: Self = Self::Graphs(GraphSelector::Any);

    /// The same graphs, written one way: a merge sorted and without repeats, a merge of one
    /// graph as that graph (so scopes of equal graph sets share a corpus). This scope where
    /// it is written so; else a copy, charged to `budget` first (its reservation first).
    fn normalised(
        &self,
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<
        (Option<nrese_exec::Reservation>, std::borrow::Cow<'_, Self>),
        nrese_exec::BudgetExceeded,
    > {
        use std::borrow::Cow;
        let Self::Merge(graphs) = self else {
            return Ok((None, Cow::Borrowed(self)));
        };
        if graphs.len() != 1 && graphs.windows(2).all(|pair| pair[0] < pair[1]) {
            return Ok((None, Cow::Borrowed(self)));
        }
        let mut copy = nrese_exec::ChargedVec::collect(
            Arc::clone(budget),
            graphs.len(),
            graphs.iter().copied(),
        )?;
        copy.sort_unstable();
        let mut len = 0;
        for i in 0..copy.len() {
            if len == 0 || copy[i] != copy[len - 1] {
                copy[len] = copy[i];
                len += 1;
            }
        }
        copy.truncate(len);
        let (graphs, charge) = copy.into_parts();
        let scope = match graphs[..] {
            [graph] => Self::Graphs(GraphSelector::Exact(graph)),
            _ => Self::Merge(graphs),
        };
        Ok((Some(charge), Cow::Owned(scope)))
    }
}

/// Which documents a corpus holds: string literals (objects) or IRIs by their local names
/// (subjects), for full-text searches; vector literals (objects), for vector searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Documents {
    Literals,
    Subjects,
    Vectors,
}

impl Documents {
    /// The kinds of term its documents are.
    fn kinds(self) -> &'static [TermKind] {
        match self {
            Self::Literals => &[TermKind::String, TermKind::LangString],
            Self::Subjects => &[TermKind::Iri],
            Self::Vectors => &[TermKind::TypedLiteral],
        }
    }

    fn holds(self, kind: TermKind) -> bool {
        self.kinds().contains(&kind)
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
            let view = canonical
                .canonical_data
                .as_ref()
                .map(|view| Arc::downgrade(view.version()));
            assert_eq!(view.is_some(), named);
            let kept = named && limit.is_none();
            assert_eq!(views.used() > 0, kept, "{named} {limit:?}");
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
