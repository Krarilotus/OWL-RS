//! Read-only, point-in-time view of the dataset.

use std::sync::Arc;

use nrese_rdf::{Quad, QuadRef, Term, TermRef};

use super::equality::{Classes as EqualityClasses, Expand, Visibility as CopyVisibility};
use super::statistics::Statistics;
use super::{ReadModel, Stack, Version};
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Permutation, QuadPattern};
use crate::term::resolve::Vocabulary;
use crate::term::{Dictionary, TermId};

/// A pattern's heavy groups as the engine keeps them ([`Snapshot::heavy_groups_in`]): values
/// with their match counts, in id order, charged for as long as any holder keeps them.
pub type HeavyGroups = Arc<nrese_exec::ChargedVec<(TermId, u64)>>;

mod cursor;
pub use cursor::{ProbeCursor, Seek};

mod reader_search;
pub use reader_search::TextScope;
pub(crate) use reader_search::{Admission, Documents};

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
    /// What its ids mean: the engine's dictionary, bounded by `version.dictionary_len`, or a
    /// private transaction's overlay view. Every snapshot made from this one keeps it.
    vocabulary: Vocabulary,
    /// Whether its statements are a published version's, or a restriction of them: what
    /// [`Self::shared_state`] grants. Set at construction, copied by the constructors that
    /// only restrict or reinterpret (the design note's matrix), cleared by those that add
    /// unpublished statements, never set on an existing snapshot.
    published: bool,
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
    /// What a restricted inferred view keeps is charged to ([`Self::with_inferred_mask`],
    /// [`Self::with_inferred_only`]), shared with the masks it came from and the snapshots
    /// made from it. Last: released after everything else the snapshot holds is freed.
    held: Held,
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
/// new run of everything hidden. Each run, and each mask's lists, is charged before it is
/// made, to a reservation the masks and views over it share: released once the last of
/// them lets go.
#[derive(Debug, Default)]
pub struct InferredMask {
    runs: Vec<Arc<crate::index::run::Run>>,
    /// Whether it ever showed a statement the stack it was made over didn't hold, so that
    /// a view through it adds statements never published ([`Snapshot::with_inferred_mask`]).
    /// Kept by every mask changed from it: an addition stays one.
    adds: bool,
    /// The charge of its runs and its list, the earlier masks' among its parts. After
    /// `runs`: released once they are freed.
    held: Held,
}

/// The bytes an `Arc` of a `size`-byte value allocates: the value and its two counts.
fn boxed(size: usize) -> usize {
    size + 2 * std::mem::size_of::<usize>()
}

/// A charge for memory masks and views keep, shared by every holder of it. The last holder
/// takes the charge out of its box (freeing the box), drops its parts (the charges of what
/// it keeps besides: the masks its runs come from, the snapshot it was made from), and
/// releases it last, so that nothing it covers is still there when it goes. It covers its
/// own box and its list of parts too.
#[derive(Debug, Clone, Default)]
pub(crate) struct Held(Option<Arc<HeldData>>);

#[derive(Debug)]
struct HeldData {
    parts: Vec<Held>,
    /// Last: released after the parts and their list are freed.
    own: nrese_exec::Reservation,
}

impl Held {
    /// `own` with `parts`. `own` holds the charge of what it covers and of this holder's
    /// box and list ([`Self::overhead`]), taken in its first charge, before anything it
    /// covers is made: nothing is charged once the payload exists, so a refusal can't
    /// release a charge while its payload is still the caller's.
    fn new(parts: Vec<Held>, own: nrese_exec::Reservation) -> Self {
        Self(Some(Arc::new(HeldData { parts, own })))
    }

    /// What a charge with a list of `parts` parts allocates for itself.
    fn overhead(parts: usize) -> usize {
        boxed(std::mem::size_of::<HeldData>()) + parts * std::mem::size_of::<Held>()
    }

    /// The parts of a charge made beside this one ([`Self::with`]): one if this is one.
    fn count(&self) -> usize {
        usize::from(self.0.is_some())
    }

    /// The parts list of a charge made beside this one: this one, if it is any, with room
    /// for `more` parts.
    fn with(&self, more: usize) -> Vec<Held> {
        let mut parts = Vec::with_capacity(self.count() + more);
        parts.extend(self.0.is_some().then(|| self.clone()));
        parts
    }

    /// The bytes charged, its parts' included (a part shared by two charges counts for
    /// each).
    fn bytes(&self) -> usize {
        self.0.as_ref().map_or(0, |data| {
            data.own.bytes() + data.parts.iter().map(Held::bytes).sum::<usize>()
        })
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // The last holder: its box freed by `into_inner`, then the parts, then the charge.
        if let Some(data) = self.0.take().and_then(Arc::into_inner) {
            drop(data);
        }
    }
}

impl InferredMask {
    /// A mask hiding `hidden` (inferred statements `snapshot` holds, each once), charged to
    /// `budget` first.
    pub fn hiding(
        snapshot: &Snapshot,
        hidden: &[EncodedQuad],
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<Self, nrese_exec::BudgetExceeded> {
        Self::default().changed(snapshot, hidden, &[], budget)
    }

    /// This mask, made for an earlier revision, over `snapshot`: `hide` are statements
    /// `snapshot`'s stack holds that it didn't hide, `show` statements it hid that are
    /// to be seen again or that `snapshot`'s stack no longer holds. Each once, the two
    /// disjoint. Its lists and its new run charged to `budget` before they are made; a
    /// refusal leaves nothing made.
    pub fn changed(
        &self,
        snapshot: &Snapshot,
        hide: &[EncodedQuad],
        show: &[EncodedQuad],
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<Self, nrese_exec::BudgetExceeded> {
        use std::mem::size_of;
        let mut held = nrese_exec::Reservation::new(Arc::clone(budget));
        let new_run = !(hide.is_empty() && show.is_empty());
        let runs_len = self.runs.len() + usize::from(new_run);
        held.charge(
            runs_len * size_of::<Arc<crate::index::run::Run>>()
                + usize::from(new_run) * boxed(size_of::<crate::index::run::Run>())
                + Held::overhead(self.held.count()),
        )?;
        // After `held`: dropped first, so a refusal below frees the runs before it releases.
        let mut runs = Vec::with_capacity(runs_len);
        runs.extend(self.runs.iter().cloned());
        if new_run {
            let layout = snapshot.version.inferred.layout();
            let run = crate::index::run::Run::from_delta_within(layout, show, hide, &mut held)?;
            runs.push(Arc::new(run));
        }
        // A statement shown that the stack doesn't hold, visible through the mask: the
        // view adds it (the store's support views show facts asserted out of a reader's
        // reach). One sign sum per shown statement, over the stack's runs and the mask's,
        // as the view will read it; nothing allocated.
        let stack = &snapshot.version.inferred;
        let visible = |quad: &EncodedQuad| {
            let sign: i64 = (stack.runs().iter().chain(&runs))
                .map(|run| run.sign_of(quad))
                .sum();
            sign > 0
        };
        let adds = self.adds
            || show
                .iter()
                .any(|quad| !stack.contains(quad) && visible(quad));
        Ok(Self {
            runs,
            adds,
            held: Held::new(self.held.with(0), held),
        })
    }

    /// The runs it stacks; past a few, a new mask is cheaper to read.
    pub fn runs(&self) -> usize {
        self.runs.len()
    }

    /// The bytes charged for it, its runs' included (shared with the masks it was made
    /// from, counted for each).
    pub fn charged(&self) -> usize {
        self.held.bytes()
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

/// How many quads a count found ([`Snapshot::count_in`]): all of them, or, where equality
/// expansion stands for more than a `u64` holds, at least this many. Planning and existence
/// take [`Self::at_least`]; an answer that must be exact takes [`Self::exact`] and says so
/// where there is none (PR #70 fix check 2 F3: a capped count was published as an exact
/// SPARQL `COUNT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Count {
    Exact(u64),
    AtLeast(u64),
}

impl Count {
    /// The count, where it is exact.
    pub fn exact(self) -> Option<u64> {
        match self {
            Count::Exact(n) => Some(n),
            Count::AtLeast(_) => None,
        }
    }

    /// A lower bound: the count where it is exact.
    pub fn at_least(self) -> u64 {
        match self {
            Count::Exact(n) | Count::AtLeast(n) => n,
        }
    }
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
    /// A snapshot of `version`, not published: no [`SharedState`]. The bulk path's checkpoint
    /// image is made so, before its version is installed.
    pub(super) fn new(
        version: Arc<Version>,
        dictionary: Arc<Dictionary>,
        statistics: Arc<Statistics>,
        equality: Option<TermId>,
        views: Arc<nrese_exec::Budget>,
    ) -> Self {
        Self {
            version,
            vocabulary: Vocabulary::Shared(dictionary),
            published: false,
            statistics,
            equality,
            canonical: false,
            canonical_data: None,
            views,
            reads: None,
            held: Held::default(),
        }
    }

    /// [`Self::new`] of a version the engine has installed or recovered: published, so it
    /// grants a [`SharedState`]. Only the engine's own snapshot acquisition calls it.
    pub(super) fn published(
        version: Arc<Version>,
        dictionary: Arc<Dictionary>,
        statistics: Arc<Statistics>,
        equality: Option<TermId>,
        views: Arc<nrese_exec::Budget>,
    ) -> Self {
        Self {
            published: true,
            ..Self::new(version, dictionary, statistics, equality, views)
        }
    }

    /// The capability to admit into state other operations reach, for a published snapshot
    /// alone (the lifecycle contract's S3); `None` for a pending, private, staged or
    /// added-to one, which computes for its request instead. Reading entries made from
    /// published snapshots needs none.
    pub fn shared_state(&self) -> Option<SharedState<'_>> {
        self.published.then_some(SharedState { snapshot: self })
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
        let (inferred, of) = match subset {
            InferredSubset::Without(hidden) => (
                self.version.inferred.with_delta(&[], &hidden),
                ViewOf::Restriction,
            ),
            InferredSubset::Only(kept) => {
                let of = self.kept_among_inferred(&kept);
                let layout = self.version.inferred.layout();
                (crate::index::IndexVersion::from_quads(layout, kept), of)
            }
        };
        self.restricted(inferred, self.held.clone(), of)
    }

    /// What a view keeping only `kept` of its inferred statements is to this snapshot: a
    /// restriction if they are all among them, an addition otherwise. The store's support
    /// views merge facts asserted out of a reader's reach into their lists, so this is
    /// checked in every build. It costs about as much as building the view: 24.5 against
    /// 21.2 ms for 100 k of 1 M statements (sizing probe `dictlc-p30-probe-d6cf4e5a`), once
    /// per view built.
    fn kept_among_inferred(&self, kept: &[EncodedQuad]) -> ViewOf {
        match kept.iter().all(|quad| self.version.inferred.contains(quad)) {
            true => ViewOf::Restriction,
            false => ViewOf::Addition,
        }
    }

    /// [`Self::with_inferred_subset`] of [`InferredSubset::Only`] `kept` (sorted, each
    /// once), everything it makes charged to `budget` before it is made and held by the
    /// view until its last reader lets it go. O(k log k) for its k statements, on one
    /// thread. A refusal leaves nothing made.
    pub fn with_inferred_only(
        &self,
        kept: &[EncodedQuad],
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<Snapshot, nrese_exec::BudgetExceeded> {
        let of = self.kept_among_inferred(kept);
        let layout = self.version.inferred.layout();
        let empty = crate::index::IndexVersion::empty(layout);
        let mut held = nrese_exec::Reservation::new(Arc::clone(budget));
        held.charge(
            Self::restricted_bytes()
                + empty.runs_bytes(1)
                + boxed(std::mem::size_of::<crate::index::run::Run>())
                + Held::overhead(self.held.count()),
        )?;
        let run = crate::index::run::Run::from_delta_within(layout, kept, &[], &mut held)?;
        let held = Held::new(self.held.with(0), held);
        let inferred = empty.with_run(run);
        Ok(self.restricted(inferred, held, of))
    }

    /// This snapshot with `added` statements in its inferred stack, for a reader of facts
    /// the store keeps apart from its stacks (the `owl2-dl` mode's upper bound): never
    /// published, so no other reader sees them. Like [`Self::with_inferred_subset`] it has
    /// statistics and equality classes of its own. O(k log k) for k statements, as one
    /// more run.
    pub fn with_inferred_added(&self, added: &[EncodedQuad]) -> Snapshot {
        // Other statements than this snapshot's, at its revision.
        self.restricted(
            self.version.inferred.with_delta(added, &[]),
            self.held.clone(),
            ViewOf::Addition,
        )
    }

    /// [`Self::with_inferred_added`] for `added` in the default graph, everything the view
    /// makes charged to `budget` before it is made and held by the view until its last
    /// reader lets it go: its run (each permutation's arrays), the stack's list of runs, its
    /// version, statistics and equality classes. Past the run builder's threshold the
    /// permutations are built in parallel on the caller's pool, each charging its own
    /// arrays first. A refusal leaves nothing made. O(k log k) for k statements.
    pub fn with_inferred_added_within(
        &self,
        added: &[crate::quad::EncodedTriple],
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<Snapshot, nrese_exec::BudgetExceeded> {
        use std::mem::size_of;
        let stack = &self.version.inferred;
        let mut held = nrese_exec::Reservation::new(Arc::clone(budget));
        held.charge(
            Self::restricted_bytes()
                + stack.runs_bytes(1)
                + boxed(size_of::<crate::index::run::Run>())
                + Held::overhead(self.held.count()),
        )?;
        // The statements as quads, while the run is built from them.
        let mut scratch = nrese_exec::Reservation::new(Arc::clone(budget));
        scratch.charge(added.len() * size_of::<EncodedQuad>())?;
        let quads: Vec<EncodedQuad> = added.iter().map(|t| t.in_default_graph()).collect();
        let built = stack.with_delta_within(&quads, budget);
        drop(quads);
        drop(scratch);
        let (inferred, run) = built?;
        let held =
            nrese_exec::Reservation::adopt(Arc::clone(budget), held.transfer() + run.transfer());
        Ok(self.restricted(
            inferred,
            Held::new(self.held.with(0), held),
            ViewOf::Addition,
        ))
    }

    /// This snapshot with the inferred statements `mask` hides removed, as
    /// [`Self::with_inferred_subset`] does, by stacking its runs: O(r) for its r runs.
    /// `Ok(None)` if the mask was built for another layout of the stack. What the view
    /// makes is charged to `budget` first; it holds the mask's charges besides.
    pub fn with_inferred_mask(
        &self,
        mask: &InferredMask,
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<Option<Snapshot>, nrese_exec::BudgetExceeded> {
        let stack = &self.version.inferred;
        if mask.runs.iter().any(|run| run.layout() != stack.layout()) {
            return Ok(None);
        }
        let mut held = nrese_exec::Reservation::new(Arc::clone(budget));
        held.charge(
            Self::restricted_bytes()
                + stack.runs_bytes(mask.runs.len())
                + Held::overhead(self.held.count() + 1),
        )?;
        let mut parts = self.held.with(1);
        parts.push(mask.held.clone());
        let held = Held::new(parts, held);
        let inferred = stack.with_runs(&mask.runs);
        let of = match mask.adds {
            true => ViewOf::Addition,
            false => ViewOf::Restriction,
        };
        Ok(Some(self.restricted(inferred, held, of)))
    }

    /// What [`Self::restricted`] allocates for a view: its version, equality cell and
    /// statistics.
    fn restricted_bytes() -> usize {
        use std::mem::size_of;
        boxed(size_of::<Version>())
            + boxed(size_of::<super::equality::EqualityCell>())
            + boxed(size_of::<Statistics>())
    }

    /// This snapshot with `inferred` as its inferred stack, holding `held`: a version of
    /// its own (new statements at its revision), with statistics and equality classes of
    /// its own. Published as its source is if `inferred` only restricts the source's
    /// inferred statements; never if it adds any ([`ViewOf`]).
    fn restricted(&self, inferred: crate::index::IndexVersion, held: Held, of: ViewOf) -> Snapshot {
        let version = Arc::new(Version {
            content: super::Content::fresh(),
            // A masked or restricted view: the same asserted stack.
            asserted_content: self.version.asserted_content,
            asserted: self.version.asserted.clone(),
            inferred,
            revision: self.version.revision,
            committed_micros: self.version.committed_micros,
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
            vocabulary: self.vocabulary.clone(),
            published: match of {
                ViewOf::Restriction => self.published,
                ViewOf::Addition => false,
            },
            statistics: Arc::new(self.statistics.for_view()),
            equality: self.equality,
            canonical: self.canonical,
            held,
        }
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

    /// The number of quads matching `pattern` in `model`, expanded, counted up to `limit`:
    /// the stored matches over representatives weighted by how many quads each stands for,
    /// without making the copies. With `limit` 1, whether a read finds one (PR #70 r1 S1).
    /// [`Count::AtLeast`] where it stopped at `limit`, or where the copies pass what a `u64`
    /// holds (its maximum then; PR #70 fix check 2 F3).
    fn expanded_count(
        &self,
        classes: &EqualityClasses,
        model: ReadModel,
        pattern: &QuadPattern,
        limit: u64,
    ) -> Count {
        let data = self.data(model);
        let normalised = classes.normalised(pattern);
        let visibility = CopyVisibility::new(data, model);
        let mut count: u64 = 0;
        for stack in Stack::ALL {
            for quad in data.stack(stack).scan(&normalised) {
                if count >= limit {
                    return Count::AtLeast(count);
                }
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
                let copies = match visibility.shows_all(touching) {
                    true => classes.multiplicity(&quad, pattern),
                    false => {
                        let (mut copies, wanted) = (0, limit - count);
                        classes.expand(&quad, pattern, &mut |copy| {
                            copies += u64::from(visibility.shows(&copy, touching));
                            match copies >= wanted {
                                true => std::ops::ControlFlow::Break(()),
                                false => std::ops::ControlFlow::Continue(()),
                            }
                        });
                        Some(copies)
                    }
                };
                match copies.and_then(|copies| count.checked_add(copies)) {
                    Some(sum) => count = sum,
                    None => return Count::AtLeast(u64::MAX),
                }
            }
        }
        if count < limit
            && model == ReadModel::Materialised
            && pattern.graph == GraphSelector::Any
            && normalised != *pattern
            && !self.canonical
        {
            count += self
                .data(model)
                .asserted
                .scan(pattern)
                .filter(|quad| !quad.graph.is_default_graph())
                .take(usize::try_from(limit - count).unwrap_or(usize::MAX))
                .count() as u64;
        }
        match count >= limit {
            true => Count::AtLeast(count),
            false => Count::Exact(count),
        }
    }

    /// An estimate of [`count_in`](Self::count_in) for planning, O(r log n): with
    /// equality classes, the stored matches without weighing the classes. Zero only if
    /// nothing matches.
    pub fn estimate_in(&self, model: ReadModel, pattern: &QuadPattern) -> u64 {
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        let Some(classes) = self.expanding(model, pattern) else {
            return self.count_in(model, pattern).at_least();
        };
        let stored = Snapshot {
            version: Arc::clone(self.data(model)),
            ..self.stored()
        };
        let normalised = classes.normalised(pattern);
        let mut estimate = stored
            .count_in(ReadModel::Materialised, &normalised)
            .at_least();
        // Named graphs are read with the pattern's own constants (a canonical snapshot's,
        // over representatives, with the normalised ones).
        if normalised != *pattern && pattern.graph == GraphSelector::Any && !self.canonical {
            estimate += stored.count_in(ReadModel::Asserted, pattern).at_least();
        }
        estimate
    }

    /// Whether `other` shows the same version of the same engine (a cache key).
    pub fn same_version(&self, other: &Snapshot) -> bool {
        Arc::ptr_eq(&self.version, &other.version)
    }

    /// A snapshot of `version` over this one's vocabulary and statistics: a transaction's
    /// pending state, never published, whatever this one is.
    pub(super) fn with_version(&self, version: Version) -> Snapshot {
        let version = Arc::new(version);
        let canonical_data = self.canonical_view(&version);
        Snapshot {
            canonical_data,
            views: Arc::clone(&self.views),
            reads: self.reads.clone(),
            version,
            vocabulary: self.vocabulary.clone(),
            published: false,
            statistics: Arc::clone(&self.statistics),
            equality: self.equality,
            canonical: self.canonical,
            held: self.held.clone(),
        }
    }

    /// This snapshot with `view` as its vocabulary: a private transaction's pending state
    /// ([`super::PrivateTransaction`]). Every snapshot made from it keeps the view. Its
    /// statistics are its own, as a restricted view's are: nothing built from its statements,
    /// whose own ids mean nothing to the engine, is kept for published snapshots.
    pub(super) fn private(mut self, view: crate::term::overlay::OverlayView) -> Snapshot {
        self.vocabulary = Vocabulary::Private(view);
        self.published = false;
        self.statistics = Arc::new(self.statistics.for_view());
        self
    }

    pub(crate) fn version(&self) -> &Version {
        &self.version
    }

    /// The published dictionary, for readers of published state (the checkpoint, the
    /// statistics' saved sets): `None` for a private snapshot, whose ids past its start mean
    /// terms the dictionary doesn't have. Those readers refuse or skip it.
    pub(crate) fn shared_dictionary(&self) -> Option<&Arc<Dictionary>> {
        self.vocabulary.dictionary()
    }

    /// The dictionary under this snapshot's vocabulary, private or not, for what reads its
    /// shared state only (planning, sizes, search's indexes). Never a decoder of this
    /// snapshot's ids: a private snapshot's own ids aren't in it.
    fn underlying(&self) -> &Dictionary {
        self.vocabulary.underlying()
    }

    /// Whether dictionary IDs denote the same terms in both snapshots. This is distinct
    /// from content identity: derived views can share a dictionary while reading different
    /// statements. It says nothing about query-local computed IDs. Two private snapshots
    /// share one only if they are views of the same transaction's vocabulary; a private one
    /// never shares one with a published one.
    pub fn shares_dictionary_with(&self, other: &Self) -> bool {
        self.vocabulary.same_as(&other.vocabulary)
    }

    /// Whether this snapshot's ids include a private transaction's own terms.
    pub fn is_private(&self) -> bool {
        self.vocabulary.is_private()
    }

    /// Dictionary entries visible to this snapshot.
    pub(crate) fn dictionary_len(&self) -> u64 {
        self.version.dictionary_len
    }

    pub fn revision(&self) -> u64 {
        self.version.revision
    }

    /// What its asserted statements are ([`super::AssertedIdentity`]): shared across the
    /// views of one asserted stack, so a cache keyed by it must not depend on the view's
    /// scope or equality mode; a transaction's pending state has its own.
    pub fn asserted_identity(&self) -> super::AssertedIdentity {
        self.version.asserted_content
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

    /// Number of asserted and inferred quads the stacks hold ([`Self::len_in`]). O(1).
    pub fn len(&self) -> u64 {
        self.len_in(ReadModel::Materialised)
    }

    /// Number of quads `model`'s stacks hold. O(1); exact because the stacks are disjoint.
    /// It is the number visible in `model` where reads take the stacks as they are. Under
    /// equality ([`crate::Engine::set_equality`]) they don't: reads skip the default graph's
    /// statements that name an alias (the inferred `alias sameAs representative` among
    /// them) and expand what representatives hold to every identity, so the visible number
    /// may be lower or higher; over [`Self::with_canonical_equality`], which expands
    /// nothing, it is at most this one. [`Self::count_in`] counts what reads see.
    pub fn len_in(&self, model: ReadModel) -> u64 {
        Stack::ALL
            .into_iter()
            .filter(|&stack| model.includes(stack))
            .map(|stack| self.data(model).stack(stack).len())
            .sum()
    }

    /// Whether reads see no quad. O(1) where reads take the stacks as they are; under
    /// equality, which can hide every quad the stacks hold, whether a read finds a first one
    /// ([`Self::exists_in`]: a scan up to it, making no copies) (PR #70 r1 F1, S1).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
            || (self.equality.is_some()
                && !self.exists_in(ReadModel::Materialised, &QuadPattern::all()))
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

    /// Number of quads matching `pattern`, asserted and inferred ([`Self::count_in`]).
    pub fn count(&self, pattern: &QuadPattern) -> Count {
        self.count_in(ReadModel::Materialised, pattern)
    }

    /// Number of quads matching `pattern` in `model`, without producing them: O(r log n)
    /// per stack when the matching runs hold no tombstones in range (see
    /// `IndexVersion::count_plan`). The stacks are disjoint, so their counts add up. Exact,
    /// except under equality where the copies pass what a `u64` holds: [`Count::AtLeast`].
    pub fn count_in(&self, model: ReadModel, pattern: &QuadPattern) -> Count {
        let pattern = self.read_pattern(model, pattern);
        let pattern = &*pattern;
        if let Some(classes) = self.expanding(model, pattern) {
            return self.expanded_count(&classes, model, pattern, u64::MAX);
        }
        if pattern.graph == GraphSelector::AnyNamed {
            // Every graph minus the default graph: two range counts instead of a scan, both
            // as held. Named graphs are read as held, never expanded, so neither count is an
            // expanded one, which can read as `u64::MAX` (PR #70 F2 round, sol-loop r1 S1).
            let every = QuadPattern {
                graph: GraphSelector::Any,
                ..*pattern
            };
            let default = QuadPattern {
                graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
                ..*pattern
            };
            return Count::Exact(self.held_count(model, &every) - self.held_count(model, &default));
        }
        Count::Exact(self.held_count(model, pattern))
    }

    /// The quads `model`'s stacks hold that match `pattern`: range counts, no expansion.
    fn held_count(&self, model: ReadModel, pattern: &QuadPattern) -> u64 {
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
            return self.expanded_count(&classes, model, pattern, 1).at_least() > 0;
        }
        if pattern.graph == GraphSelector::AnyNamed {
            return self.count_in(model, pattern).at_least() > 0;
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
        let (plan, bound) = self.group_plan(model, &pattern, permutation)?;
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

    /// The plan the group walks of `pattern` (as read) take in `permutation`, and its bound
    /// prefix's length: `None` where a walk can't answer it (equality classes expanded, a
    /// bound graph after the prefix, a range excluding the default graph, an included
    /// stack without the permutation).
    fn group_plan(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<(AccessPlan, usize)> {
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
        supported.then_some((plan, bound))
    }

    /// The groups of [`Self::group_counts_in`] that more than `min` quads match, in id
    /// order: exact, and kept for this version once walked, so that a planner asking for
    /// them per query walks the index once (a counted forest's message budgets,
    /// `nrese-sparql`'s `wcoj`). Charged to the engine's statistics budget
    /// ([`crate::Engine::charge_statistics_to`]) for as long as any holder keeps them.
    /// `None` where `group_counts_in` can't walk them, or the budget can't take them.
    pub fn heavy_groups_in(
        &self,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
        min: u64,
    ) -> Option<HeavyGroups> {
        let read = *self.read_pattern(model, pattern);
        // Which reads the walk answers is decided before the list kept: a stored read of
        // this version keeps one under the same key.
        let (plan, bound) = self.group_plan(model, &read, permutation)?;
        self.statistics
            .heavy_groups(self.data(model), model, read, permutation, min, |budget| {
                self.heavy_groups_walked(model, &plan, bound, min, budget)
            })
    }

    /// The groups past `min` of `plan` grouped at `bound`, charged to `budget` as they are
    /// found: the candidates of each run past `min / r` for `r` runs in all (a value past
    /// `min` in all of them is one), each counted again in all of them unless one run
    /// holds the range. No light group is held.
    fn heavy_groups_walked(
        &self,
        model: ReadModel,
        plan: &AccessPlan,
        bound: usize,
        min: u64,
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<nrese_exec::ChargedVec<(TermId, u64)>, nrese_exec::BudgetExceeded> {
        let stacks = || {
            Stack::ALL
                .into_iter()
                .filter(move |&stack| model.includes(stack))
        };
        let runs: usize = stacks()
            .map(|stack| self.data(model).stack(stack).runs().len())
            .sum();
        let above = (min / runs.max(1) as u64) as i64;
        let mut candidates = nrese_exec::ChargedVec::new(Arc::clone(budget));
        for stack in stacks() {
            self.data(model)
                .stack(stack)
                .group_candidates(plan, bound, above, &mut candidates)?;
        }
        let mut heavy = nrese_exec::ChargedVec::new(Arc::clone(budget));
        if runs <= 1 {
            // One run: its counts are the values' own.
            for &(value, count) in candidates.iter() {
                if count > min as i64 {
                    heavy.push((TermId::from_raw(value), count as u64))?;
                }
            }
            return Ok(heavy);
        }
        candidates.sort_unstable_by_key(|&(value, _)| value);
        let mut last = None;
        for &(value, _) in candidates.iter() {
            // Each value once, though several runs may hold it past `above`.
            if last.replace(value) == Some(value) {
                continue;
            }
            let mut exact = *plan;
            exact.low[bound] = value;
            exact.high[bound] = value;
            let count: u64 = stacks()
                .map(|stack| self.data(model).stack(stack).count_plan(&exact))
                .sum();
            if count > min {
                heavy.push((TermId::from_raw(value), count))?;
            }
        }
        Ok(heavy)
    }

    /// The values of `components` (subject 0, predicate 1, object 2, graph 3) of the quads
    /// matching `pattern` in `model`, as columns in `permutation`'s order: what
    /// [`scan_sorted_in`](Self::scan_sorted_in) yields, decoded a block and a column at a
    /// time instead of a quad at a time. `None` where that can't answer: the matches lie
    /// in several runs or a run with deletions, which must be merged quad by quad, or in
    /// both stacks with columns that can't order them ([`Self::columns_of_stacks`]); or
    /// the plan needs a post-filter.
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
    /// decoded so (several runs, deletions), or where both have matches and the columns
    /// can't order them: rows compare on the columns alone, so every column the plan lets
    /// vary must be among them up to the last of them in `permutation`'s order.
    fn columns_of_stacks(
        &self,
        model: ReadModel,
        plan: &AccessPlan,
        permutation: Permutation,
        components: &[usize],
    ) -> Option<Vec<Vec<u64>>> {
        let matching = Stack::ALL
            .map(|stack| model.includes(stack) && self.data(model).stack(stack).any_plan(plan));
        let order = permutation.order();
        let projected = |position: usize| components.contains(&order[position]);
        let last = (0..4).rev().find(|&position| projected(position));
        let ordered = last.is_none_or(|last| {
            (0..last)
                .all(|position| plan.low[position] == plan.high[position] || projected(position))
        });
        if matching.iter().all(|&matches| matches) && !ordered {
            return None;
        }
        let mut parts: Vec<Vec<Vec<u64>>> = Vec::new();
        for (stack, matches) in Stack::ALL.into_iter().zip(matching) {
            if !matches {
                continue;
            }
            let index = self.data(model).stack(stack);
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
        self.vocabulary
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
        self.vocabulary.lookup(term, self.version.dictionary_len)
    }

    pub fn lookup_quad(&self, quad: QuadRef<'_>) -> Option<EncodedQuad> {
        self.vocabulary
            .lookup_quad(quad, self.version.dictionary_len)
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
        self.vocabulary
            .triple_term([subject, predicate, object], self.version.dictionary_len)
    }

    /// The subject, predicate and object ids of the triple term `id`, nothing decoded.
    /// `None` for another kind of term, one this snapshot doesn't know, and a triple term
    /// the first RDF 1.2 version keyed by its text ([`crate::Dictionary::components`]).
    pub fn components(&self, id: TermId) -> Option<[TermId; 3]> {
        self.vocabulary.components(id, self.version.dictionary_len)
    }

    pub fn decode(&self, id: TermId) -> Option<Term> {
        self.vocabulary.decode(id)
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
        self.vocabulary
            .decode_charged(id, self.version.dictionary_len, charge)
    }

    /// An upper bound of the bytes [`Self::decode`] allocates for `id`, for the dictionary
    /// as stored now: a checkpoint may store the term compressed after, so a charge for a
    /// decode goes through [`Self::decode_charged`]. `None` for inline ids, triple terms and
    /// ids this snapshot doesn't know.
    pub fn decode_bound(&self, id: TermId) -> Option<usize> {
        self.vocabulary
            .decode_bound(id, self.version.dictionary_len)
    }

    /// Calls `f` with a borrowed view of the dictionary term `id` (see
    /// [`Dictionary::with_view`](crate::Dictionary::with_view)); `None` for inline ids and
    /// ids this snapshot doesn't know.
    pub fn with_view<R>(&self, id: TermId, f: impl FnOnce(crate::TermView<'_>) -> R) -> Option<R> {
        self.vocabulary
            .with_view(id, self.version.dictionary_len, f)
    }

    /// Calls `f` with a lookup of the views of this snapshot's dictionary terms, under one
    /// read lock ([`Self::with_view`] for many terms).
    pub fn with_views<R>(
        &self,
        f: impl for<'v> FnOnce(&'v dyn Fn(TermId) -> Option<crate::TermView<'v>>) -> R,
    ) -> R {
        self.vocabulary.with_views(self.version.dictionary_len, f)
    }

    pub fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.vocabulary.decode_quad(quad)
    }

    /// The dictionary terms this snapshot knows whose text passes `test`, sorted (whether
    /// statements still use them is the caller's to check). One pass over the dictionary:
    /// worth it where a pattern has many more rows than the dictionary has bytes per row
    /// of a random read.
    pub fn matching_strings(&self, test: &crate::StringTest<'_>) -> Vec<TermId> {
        self.vocabulary
            .matching_strings(test, self.version.dictionary_len)
    }

    /// [`Self::matching_strings`], and what its scan did ([`crate::StringScan`]).
    pub fn matching_strings_scan(
        &self,
        test: &crate::StringTest<'_>,
    ) -> (Vec<TermId>, crate::StringScan) {
        self.vocabulary
            .matching_strings_scan(test, self.version.dictionary_len)
    }

    /// Whether a prefix test ([`crate::Placement::Start`]) is answered from the text order
    /// by binary search, rather than by a pass over the dictionary.
    pub fn text_order_ready(&self) -> bool {
        self.underlying().text_order_ready()
    }

    /// The size of this snapshot's dictionary text in bytes (the arena: what
    /// [`matching_strings`](Self::matching_strings) reads).
    pub fn dictionary_bytes(&self) -> u64 {
        self.underlying().stats().arena_bytes
    }

    /// The string literals this snapshot's dictionary holds that match `query`, best first
    /// (whether statements still use them is the caller's to check). A private snapshot's
    /// shared indexes don't cover its own terms, and extending them would change state every
    /// snapshot shares: it searches what it can see instead, as a reader of every graph in the
    /// materialised model ([`Self::text_search_in`]; V3, DICT2 9 Nov 04:02 Q2).
    pub fn text_search(
        &self,
        query: &crate::TextQuery,
    ) -> Result<Vec<crate::TextMatch>, SearchRefused> {
        if self.is_private() {
            return self.text_search_in(ReadModel::Materialised, &TextScope::EVERY, query, None);
        }
        let known = self.version.dictionary_len;
        let mut matches = self.underlying().text_search(query);
        matches.retain(|m| TermId::from_raw(m.id).payload() < known);
        Ok(matches)
    }

    /// The vector literals this snapshot's dictionary holds that are nearest to
    /// `query.vector`, among those `accept` takes, nearest first, with how the search went
    /// (whether statements still use them is the caller's to check, in `accept`). What the
    /// search holds, the hits included, is charged to `budget` before it is allocated: a
    /// search the budget has no room for is refused, with nothing left charged. A private
    /// snapshot searches what it can see, as [`Self::text_search`] does
    /// ([`Self::vector_search_in`]): run within what `budget` has left, held on it meanwhile.
    pub fn vector_search<'b>(
        &self,
        query: &crate::VectorQuery,
        accept: &(dyn Fn(TermId) -> bool + Sync),
        budget: &'b nrese_exec::Budget,
    ) -> Result<VectorHits<'b>, SearchRefused> {
        if self.is_private() {
            // A snapshot reading within this very budget (a query's envelope): the reader
            // path charges it through its own `Arc`, as its reads do (sol-loop r1 B3: a
            // bridge holding the budget's room would refuse those reads).
            let reading = self.reads.as_ref().and_then(|reads| reads.0.as_ref());
            if let Some(reads) = reading.filter(|reads| std::ptr::eq(&*reads.budget, budget)) {
                let (hits, report) = self.vector_search_in(
                    ReadModel::Materialised,
                    &TextScope::EVERY,
                    query,
                    accept,
                    &reads.budget,
                )?;
                // Handed over on `budget`: charged there before they are copied.
                let found =
                    nrese_exec::ChargedVec::collect(budget, hits.len(), hits.iter().copied());
                drop(hits);
                return Ok((found?, report));
            }
            // Otherwise an `Arc`'d budget of what `budget` has left, that room held on
            // `budget` until the search is done, then the hits charged there. The snapshot's
            // reads, where it has any, charge their own budget, not this one. (A reader's
            // budget beside `budget` under one parent would find the parent's room held:
            // no caller passes one; SPARQL passes the reader's, the others unbounded ones.)
            let (inner, held) = match budget.is_bounded() {
                true => {
                    let room = budget.remaining();
                    let mut held = nrese_exec::Reservation::new(budget);
                    held.charge(room)?;
                    (nrese_exec::Budget::new(room), Some(held))
                }
                false => (nrese_exec::Budget::unlimited(), None),
            };
            let inner = Arc::new(inner);
            let (hits, report) = self.vector_search_in(
                ReadModel::Materialised,
                &TextScope::EVERY,
                query,
                accept,
                &inner,
            )?;
            // Of the held room, what the copy takes is let go for it, the rest held until the
            // inner hits are freed (sol-loop r2 B3: no gap where neither is counted).
            let held = held.map(|mut held| {
                held.release(hits.len() * std::mem::size_of::<(TermId, f32)>());
                held
            });
            let found = nrese_exec::ChargedVec::collect(budget, hits.len(), hits.iter().copied());
            drop(hits);
            drop(held);
            return Ok((found?, report));
        }
        let (hits, report) =
            self.underlying()
                .vector_search(query, self.version.dictionary_len, accept, budget)?;
        let len = hits.len();
        let (hits, held) = hits.into_parts();
        let found = nrese_exec::ChargedVec::collect(
            budget,
            len,
            hits.into_iter()
                .map(|hit| (TermId::from_raw(hit.id), hit.distance)),
        );
        drop(held);
        Ok((found?, report))
    }

    /// Builds the graph searches like `query` use, covering every vector of its
    /// dimension, before returning: a warm-up (searches build it themselves, a large one
    /// on a thread of its own while they scan exactly). Nothing for a private snapshot,
    /// whose searches scan exactly: a build is shared state.
    pub fn prepare_vector_graph(&self, query: &crate::VectorQuery) {
        if !self.is_private() {
            self.underlying().prepare_vector_graph(query);
        }
    }

    /// The IRIs this snapshot's dictionary holds whose local names match `query`, best
    /// first (whether statements still use them is the caller's to check). A private
    /// snapshot searches what it can see, as [`Self::text_search`] does.
    pub fn iri_search(
        &self,
        query: &crate::TextQuery,
    ) -> Result<Vec<crate::TextMatch>, SearchRefused> {
        if self.is_private() {
            return self.iri_search_in(ReadModel::Materialised, &TextScope::EVERY, query, None);
        }
        let known = self.version.dictionary_len;
        let mut matches = self.underlying().iri_search(query);
        matches.retain(|m| TermId::from_raw(m.id).payload() < known);
        Ok(matches)
    }
}

/// The capability to admit into state other operations reach (the lifecycle contract's S3):
/// held only for a published snapshot ([`Snapshot::shared_state`]), and carrying it, so an
/// owner that takes one works on its snapshot and no other. Proves eligibility only: it
/// replaces none of a cache's key dimensions or reader filters.
///
/// ```
/// let engine = nrese_engine::Engine::new(Default::default()).unwrap();
/// let snapshot = engine.snapshot();
/// let state = snapshot.shared_state().expect("a published snapshot");
/// assert!(std::ptr::eq(state.snapshot(), &snapshot));
/// assert!(engine.speculative().pending_snapshot().shared_state().is_none());
/// ```
///
/// No code outside the engine can make one:
///
/// ```compile_fail
/// fn forge(snapshot: &nrese_engine::Snapshot) -> nrese_engine::SharedState<'_> {
///     nrese_engine::SharedState { snapshot }
/// }
/// ```
#[derive(Clone, Copy)]
pub struct SharedState<'s> {
    snapshot: &'s Snapshot,
}

impl<'s> SharedState<'s> {
    /// The published snapshot it was taken from.
    pub fn snapshot(&self) -> &'s Snapshot {
        self.snapshot
    }
}

/// What a restricted view's inferred stack is to its source's ([`Snapshot::restricted`]):
/// a restriction keeps the source's publication, an addition never has one.
#[derive(Clone, Copy)]
enum ViewOf {
    /// Fewer of the source's inferred statements: a subset, `Only` or a mask.
    Restriction,
    /// Statements the source doesn't have, never published.
    Addition,
}

/// Why a [`Snapshot`]'s search gave no answer: a refusal, never an empty answer.
#[derive(Debug, Clone, Copy)]
pub enum SearchRefused {
    /// The search's budget had no room ([`nrese_exec::BudgetExceeded`]).
    Memory(nrese_exec::BudgetExceeded),
    /// Not returned since V3: a private snapshot ([`Snapshot::is_private`]) searches what it
    /// can see. Kept while callers still match it (`nrese-sparql`'s results).
    Private,
}

impl From<nrese_exec::BudgetExceeded> for SearchRefused {
    fn from(exceeded: nrese_exec::BudgetExceeded) -> Self {
        Self::Memory(exceeded)
    }
}

impl std::fmt::Display for SearchRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(exceeded) => exceeded.fmt(f),
            Self::Private => f.write_str("a private snapshot can't be searched"),
        }
    }
}

impl std::error::Error for SearchRefused {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Memory(exceeded) => Some(exceeded),
            Self::Private => None,
        }
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

/// A vector search's hits, nearest first, charged to the budget it was given, and how the
/// search went ([`Snapshot::vector_search`]).
pub type VectorHits<'b> = (
    nrese_exec::ChargedVec<(TermId, f32), &'b nrese_exec::Budget>,
    crate::VectorSearchReport,
);

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
