//! The corpora of reader-scoped searches ([`Snapshot::text_search_in`](super::Snapshot::text_search_in),
//! `SearchScope::Reader`), kept, and derived from one another across commits.
//!
//! The store exists only once a reader-scoped search ran ([`super::statistics::Statistics`]
//! holds it in a `OnceLock`): an engine whose searches are all global keeps none, records no
//! lineage and takes none of its locks.
//!
//! A corpus is exact for its statements' identity ([`Content`]), the index's documents, the
//! read model, the graphs (the scope, normalised so that equal graph sets share one) and
//! how the reader reads equality classes (`owl:sameAs` expanded or canonical, or as
//! stored): its members are the terms the reader's own scans show. Building one walks the
//! scope's statements; a version that a commit made from another instead takes the corpus
//! of its nearest kept ancestor and looks again at the terms the steps between them touched:
//! each is a member if a statement the reader sees in the scope still has it, a probe. So
//! a search after a small commit costs probes for what the commit touched, not a walk,
//! and deletes stay exact (a literal stays while any visible statement has it). Where a
//! step changed a `sameAs` statement the reader expands, the classes changed and the
//! corpus is walked again; so is an IRIs' corpus of a reader of equality classes. A
//! version made without a recorded step (a bulk load, a reopen, a store made before the
//! first reader search) has no lineage: walked.
//!
//! The lineage (content → its parent, the terms and predicates touched) is kept only
//! while a corpus is kept, back to the oldest kept one, as far as the budget holds it.
//! Where more terms were touched since a kept ancestor than a probe each is worth
//! ([`worth_deriving`], [`CorpusTuning::documents_per_probe`]), or the lineage doesn't
//! reach one, the corpus is walked again.
//!
//! Memory is the memory-accounting owner's ([`nrese_exec::ChargedVec`],
//! [`nrese_exec::Reservation`]), charged before it is allocated. What the store keeps is
//! charged to its budget ([`crate::Engine::set_text_corpus_budget`]; at first
//! [`DEFAULT_READER_CORPUS_BYTES`]): a kept corpus's members (built on it, so nothing moves
//! between budgets), each entry's key and each lineage step; a kept corpus an evicted
//! entry's search still holds stays charged until it lets go. What doesn't fit after the
//! least recently used are let go isn't kept: the corpus is then built on the searching
//! request's budget and serves that search alone, as everything a search builds besides
//! (the bits, the walk's counts, the probes' list). The budget is the only bound on how
//! many corpora and steps are kept.
//!
//! Only a published snapshot, or a restriction of one, changes the store (the dictionary
//! lifecycle's S3: its [`super::snapshot::CorpusAdmission`], from its `SharedState`): it makes it,
//! claims, evicts, keeps, abandons; only a successful publication records a step. Any other
//! snapshot (a writer's pending state, a view that adds facts) builds for its search alone,
//! derived from a kept corpus where that corpus's runs are its own runs' prefix
//! ([`CorpusStore::derive_unadmitted`], reading the store), else walked.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
use parking_lot::Mutex;

use super::snapshot::{CorpusAdmission, Documents, TextScope};
use super::{Content, ReadModel, Stack, Version};
use crate::term::text::Corpus;
use crate::term::{TermId, TermKind};

/// Where an engine's store of corpora is made: a cell shared by the engine's statistics and
/// its views' ([`super::statistics::Statistics::for_view`]), empty until the first
/// reader-scoped search. Statistics of their own (a test's) have none: their searches build
/// their corpora and keep none. The default allocates nothing, so a view of an engine
/// whose searches are all global costs no more than before.
#[derive(Default, Clone)]
pub(crate) struct Corpora(Option<Arc<std::sync::OnceLock<CorpusStore>>>);

impl Corpora {
    /// An engine's cell: empty.
    pub(crate) fn engine() -> Self {
        Self(Some(Arc::default()))
    }

    /// The store, if a reader-scoped search made it.
    pub(crate) fn get(&self) -> Option<&CorpusStore> {
        self.0.as_ref()?.get()
    }

    /// The store, made now if no search made it yet; `None` without a cell.
    pub(crate) fn get_or_init(&self) -> Option<&CorpusStore> {
        Some(self.0.as_ref()?.get_or_init(CorpusStore::default))
    }
}

/// What the corpora of reader-scoped searches an engine keeps, and their lineage, may take
/// unless set otherwise (the store's `store.reader_corpus_bytes`;
/// [`crate::Engine::set_text_corpus_budget`]): 256 MiB. A 1 M-document corpus takes
/// 250,336 bytes (fleet `syn24-lineage-probe-8faeb458`), so about a thousand such corpora fit;
/// the least recently used go first.
pub const DEFAULT_READER_CORPUS_BYTES: usize = 256 << 20;

/// How reader-scoped searches build their corpora: settings, each with its default and the
/// measured reason for it ([`crate::Engine::set_corpus_tuning`]; the store's
/// `store.reader_walk_slice`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusTuning {
    /// Words of bits (64 terms each) one slice of a corpus walk covers at most; each slice
    /// also holds a group count of 16 bytes a term.
    pub walk_slice_words: usize,
    /// Documents a walk visits for the work of one probe of a touched term: a corpus is
    /// derived from a kept ancestor's while the terms touched since are at most its
    /// documents over this, else walked again. At least 1.
    pub documents_per_probe: u64,
}

impl CorpusTuning {
    /// The defaults, each measured.
    ///
    /// - `walk_slice_words` 1024: a walk of 1 M documents in slices of 256, 1,024 and
    ///   4,096 words does 111.7 M, 110.4 M and 111.9 M instructions (fleet
    ///   `syn24-work-probe-2-06961a4e`) and takes 20.8, 9.1 and 9.4 ms on 12 threads
    ///   (`syn24-tuning-probe-f4b4dd86`); 1,024 holds no more scratch than 4,096.
    /// - `documents_per_probe` 10, from work, not wall time, so it holds whatever the core
    ///   count. At 1 M documents a probe does about 1,100 instructions and a walk about
    ///   110 M: they cost alike at about 100,000 terms, 10 documents a probe
    ///   (`syn24-work-probe-2-06961a4e`). A walk has no fixed cost in work (1,000
    ///   documents: 157–203 k instructions), so there is no floor. On 12 threads a walk's
    ///   wall time matches 90–190 documents a probe (`syn24-tuning-probe-f4b4dd86`): just
    ///   below the crossover, a serial derivation may take up to about 10× the parallel
    ///   walk's time. A latency-first store with one tenant can raise it.
    pub const DEFAULT: Self = Self {
        walk_slice_words: 1024,
        documents_per_probe: 10,
    };
}

impl Default for CorpusTuning {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What a corpus is of, without the version: the documents, the model, the graphs, and
/// the reader's equality (`owl:sameAs` when it expands classes) and whether it reads them
/// canonically.
pub(crate) type CorpusKind = (Documents, ReadModel, TextScope, Option<TermId>, bool);

/// A corpus's kind as a search asks for it: borrowed, so that a lookup copies nothing; owned
/// ([`CorpusKind`]) only when an entry is made, after its bytes are charged.
#[derive(Clone, Copy)]
pub(crate) struct KindRef<'a> {
    pub(crate) documents: Documents,
    pub(crate) model: ReadModel,
    pub(crate) scope: &'a TextScope,
    pub(crate) equality: Option<TermId>,
    pub(crate) canonical: bool,
}

impl KindRef<'_> {
    pub(crate) fn matches(&self, kind: &CorpusKind) -> bool {
        kind.0 == self.documents
            && kind.1 == self.model
            && kind.2 == *self.scope
            && kind.3 == self.equality
            && kind.4 == self.canonical
    }

    /// The touched terms whose probes can change this kind's members, of a step's or a
    /// delta's `touched` (sorted raw ids). Blank nodes are never documents: they matter only
    /// to a reader of equality classes of IRIs (the IRIs' corpora, in a model with inferred
    /// statements), whose classes may hold them; to any other kind they are left out. Ids
    /// sort by kind first (its tag is the high bits), so the blank nodes are one range.
    fn probed<'t>(&self, touched: &'t [u64]) -> [&'t [u64]; 2] {
        if self.reads_classes() {
            return [touched, &[]];
        }
        let start = touched.partition_point(|&raw| tag(raw) < BLANK);
        let end = touched.partition_point(|&raw| tag(raw) <= BLANK);
        [&touched[..start], &touched[end..]]
    }

    /// Whether this kind's reader may expand equality classes of IRIs ([`reads_classes`]).
    fn reads_classes(&self) -> bool {
        reads_classes(self.documents, self.model, self.equality)
    }

    /// How many of `touched` [`Self::probed`] keeps.
    fn probed_len(&self, touched: &[u64]) -> usize {
        self.probed(touched).iter().map(|part| part.len()).sum()
    }

    pub(crate) fn to_owned(self) -> CorpusKind {
        (
            self.documents,
            self.model,
            self.scope.clone(),
            self.equality,
            self.canonical,
        )
    }
}

/// Counts of the corpora (for tests and the engine's statistics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextCorpusStats {
    /// Whether the store exists: a reader-scoped search ran on this engine.
    pub started: bool,
    /// Corpora kept.
    pub kept: u64,
    /// Bytes charged to the corpora's budget: kept corpora (and evicted ones a search
    /// still holds), entries' keys, the lineage.
    pub bytes: u64,
    /// The budget's limit.
    pub budget: u64,
    /// Lineage steps recorded since the engine started (publications made while a corpus
    /// was kept).
    pub steps: u64,
    /// Corpora built by a walk, and derived from an ancestor's, since the engine started.
    pub walked: u64,
    pub derived: u64,
}

/// A corpus a search uses: one the store keeps, shared, or the search's own.
pub(crate) enum CorpusRef {
    Kept(Arc<Corpus>),
    Own(Corpus),
}

impl std::ops::Deref for CorpusRef {
    type Target = Corpus;

    fn deref(&self) -> &Corpus {
        match self {
            Self::Kept(corpus) => corpus,
            Self::Own(corpus) => corpus,
        }
    }
}

/// Bytes the shared allocation of a kept corpus takes (an `Arc`'s: two counts and it).
const SHARED: usize = 2 * std::mem::size_of::<usize>() + std::mem::size_of::<Corpus>();

/// A kept corpus: of which statements, of what, the charge of its key, and the corpus once
/// its first search built it (`None` meanwhile: other searches build their own rather
/// than wait, since the build runs on every core and a waiting thread may be one of its
/// parts').
struct Kept {
    content: Content,
    kind: CorpusKind,
    /// The latest version seen with this content (compaction gives the same content new
    /// runs): while it lives, a snapshot may still ask.
    version: Weak<Version>,
    _key: Reservation,
    /// The corpus, and the charge of its shared allocation.
    corpus: Option<(Arc<Corpus>, Reservation)>,
}

/// Bytes an entry takes besides its corpus: itself and its graphs (a clone, as long as
/// it is: the capacity a vector's clone has).
fn key_bytes(kind: KindRef<'_>) -> usize {
    let graphs = match kind.scope {
        TextScope::Merge(graphs) => std::mem::size_of_val(graphs.as_slice()),
        TextScope::Graphs(_) => 0,
    };
    std::mem::size_of::<Kept>() + graphs
}

/// Whether a corpus of `documents` in `model`, read with `equality`, may expand equality
/// classes of IRIs (as `Snapshot::corpus_classes` does where a class of two or more
/// exists): then a statement about a blank node or literal of a class changes the
/// membership of the class's IRIs (sol-loop r1 R1-S1).
fn reads_classes(documents: Documents, model: ReadModel, equality: Option<TermId>) -> bool {
    documents == Documents::Subjects && model != ReadModel::Asserted && equality.is_some()
}

/// One step of the lineage: the content it was made from, the terms it touched (string
/// and typed literals as objects, IRIs as subjects, and every other subject where
/// `subjects`; raw ids, sorted) and the predicates of the statements it added or removed
/// (raw ids, sorted), each on the store's budget. Its slots in the map and the order are
/// charged with their storage ([`State::slots`]).
struct Step {
    parent: Content,
    touched: Vec<u64>,
    predicates: Vec<u64>,
    /// Whether `touched` holds every subject, not only the IRIs: recorded while a corpus
    /// that reads equality classes is kept, and needed by one to derive across this step
    /// (sol-loop r2 P1, `dictlc-p32-count4`: holding them always cost a commit of blank-node
    /// statements 5 % more).
    subjects: bool,
    _charges: [Reservation; 2],
}

/// Bytes the lineage's map and order hold when made for `capacity` steps
/// (`HashMap::with_capacity`, `VecDeque::with_capacity`): the map's buckets (as `std`'s
/// table sizes them: 4 or 8 below 8 steps, else the power of two of at least
/// capacity × 8 / 7; an entry and a control byte each, a group of control bytes and its
/// alignment) and one slot of the order each.
fn slot_bytes(capacity: usize) -> usize {
    let buckets = match capacity {
        0..4 => 4,
        4..8 => 8,
        _ => (capacity * 8 / 7).next_power_of_two(),
    };
    buckets * (std::mem::size_of::<(Content, Step)>() + 1)
        + 32
        + capacity * std::mem::size_of::<Content>()
}

struct State {
    /// Least recently used first.
    kept: Vec<Kept>,
    /// Kept corpora let go while searches may still hold them: each with the charge of its
    /// shared allocation, released once the last search let go (when it is freed).
    retired: Vec<(Weak<Corpus>, Reservation)>,
    steps: HashMap<Content, Step>,
    order: VecDeque<Content>,
    /// The charge of the storage of `steps` and `order` on the store's budget: grown only
    /// after its bytes are charged ([`CorpusStore::room_for_step`]), freed before its
    /// charge goes. Held, never read.
    _slots: Reservation,
}

/// The kept corpora and the lineage, shared by the snapshots of an engine and the views
/// made from them.
pub(crate) struct CorpusStore {
    budget: Mutex<Arc<Budget>>,
    tuning: Mutex<CorpusTuning>,
    state: Mutex<State>,
    steps: AtomicU64,
    walked: AtomicU64,
    derived: AtomicU64,
}

/// What a search finds for a corpus it needs: kept, or to derive from an ancestor's
/// (with the terms touched since, on the search's budget) or to walk; and whether to keep
/// what it builds (not where another search is building it, or its key didn't fit).
pub(crate) enum Found {
    Kept(Arc<Corpus>),
    Build {
        from: Option<(Arc<Corpus>, ChargedVec<u64>)>,
        keep: bool,
    },
}

/// A raw id's kind tag, and a blank node's.
fn tag(raw: u64) -> u64 {
    raw >> crate::term::PAYLOAD_BITS
}
const BLANK: u64 = TermKind::BlankNode as u64;

/// The terms of a statement whose membership it may change (raw ids): its object if a
/// string literal or a typed one (a vector, for the corpora of vector searches), and its
/// subject if an IRI, or of any kind with `subjects`. A blank node's or literal's class can
/// hold IRIs, which a reader of equality classes shows as subjects of the statement: the
/// derivation expands each term to its class and keeps the identities the corpus's
/// documents hold (sol-loop r1 R1-S1). No other corpus holds them.
fn terms(
    subject: TermId,
    object: TermId,
    subjects: bool,
) -> std::iter::Chain<std::option::IntoIter<u64>, std::option::IntoIter<u64>> {
    let object = matches!(
        object.kind(),
        TermKind::String | TermKind::LangString | TermKind::TypedLiteral
    )
    .then_some(object.raw());
    let subject = (subjects || subject.kind() == TermKind::Iri).then_some(subject.raw());
    object.into_iter().chain(subject)
}

/// The most touched terms worth probing to derive a corpus of `documents` rather than
/// walking it: a probe does the work a walk does on `documents_per_probe` documents.
pub(crate) fn derive_bound(documents: u64, documents_per_probe: u64) -> u64 {
    documents / documents_per_probe.max(1)
}

/// Whether probing `touched` terms beats walking a corpus of `documents`.
pub(crate) fn worth_deriving(touched: usize, documents: u64, documents_per_probe: u64) -> bool {
    touched as u64 <= derive_bound(documents, documents_per_probe)
}

/// `items` sorted, without repeats (the vector keeps its capacity and charge).
fn sort_dedup(items: &mut ChargedVec<u64>) {
    items.sort_unstable();
    let mut len = 0;
    for i in 0..items.len() {
        if len == 0 || items[i] != items[len - 1] {
            items[len] = items[i];
            len += 1;
        }
    }
    items.truncate(len);
}

/// The runs `base` holds, where they are the first runs of `version` in both stacks (the
/// same `Arc`s, in the same layout); `None` otherwise. Both versions hold the runs while
/// they are compared, so no address is reused meanwhile.
fn run_prefix(base: &Version, version: &Version) -> Option<usize> {
    Stack::ALL.into_iter().try_fold(0, |held, stack| {
        let (base, version) = (base.stack(stack), version.stack(stack));
        let (had, has) = (base.runs(), version.runs());
        let prefix = base.layout() == version.layout()
            && had.len() <= has.len()
            && had.iter().zip(has).all(|(a, b)| Arc::ptr_eq(a, b));
        prefix.then_some(held + had.len())
    })
}

/// The steps from `from` up its lineage, nearest first, while `steps` holds them.
fn up(steps: &HashMap<Content, Step>, from: Content) -> impl Iterator<Item = &Step> + Clone {
    std::iter::successors(steps.get(&from), |step| steps.get(&step.parent))
}

impl Default for CorpusStore {
    fn default() -> Self {
        Self::on(Arc::new(Budget::new(DEFAULT_READER_CORPUS_BYTES)))
    }
}

impl CorpusStore {
    fn on(budget: Arc<Budget>) -> Self {
        Self {
            state: Mutex::new(State {
                kept: Vec::new(),
                retired: Vec::new(),
                steps: HashMap::new(),
                order: VecDeque::new(),
                _slots: Reservation::new(Arc::clone(&budget)),
            }),
            budget: Mutex::new(budget),
            tuning: Mutex::new(CorpusTuning::DEFAULT),
            steps: AtomicU64::new(0),
            walked: AtomicU64::new(0),
            derived: AtomicU64::new(0),
        }
    }

    /// The budget what the store keeps is charged to from now on: kept corpora and steps
    /// let go (they stay charged to the old one while searches hold them).
    pub(crate) fn set_budget(&self, budget: Arc<Budget>) {
        let mut state = self.state.lock();
        for kept in std::mem::take(&mut state.kept) {
            Self::retire(&mut state, kept);
        }
        Self::forget_lineage(&mut state, &budget);
        *self.budget.lock() = budget;
    }

    /// The budget what the store keeps is charged to.
    pub(crate) fn budget(&self) -> Arc<Budget> {
        Arc::clone(&self.budget.lock())
    }

    /// How corpora are built from now on; kept corpora stay (they are exact either way).
    pub(crate) fn set_tuning(&self, tuning: CorpusTuning) {
        *self.tuning.lock() = tuning;
    }

    /// How corpora are built.
    pub(crate) fn tuning(&self) -> CorpusTuning {
        *self.tuning.lock()
    }

    /// Records that `child` was made from `parent`, adding or removing these statements
    /// (subject, predicate, object).
    pub(crate) fn record(
        &self,
        parent: Content,
        child: Content,
        statements: impl Iterator<Item = (TermId, TermId, TermId)> + Clone,
    ) {
        if self.state.lock().kept.is_empty() {
            return;
        }
        let n = statements.clone().count();
        let touched = |subjects: bool| {
            statements
                .clone()
                .flat_map(move |(subject, _, object)| terms(subject, object, subjects))
        };
        let predicates = statements.clone().map(|(_, predicate, _)| predicate.raw());
        self.record_terms(parent, child, 2 * n, touched, n, predicates);
    }

    /// A step's terms and predicates, sorted, on `budget`.
    fn charged_step(
        budget: &Arc<Budget>,
        bound: usize,
        terms: impl IntoIterator<Item = u64>,
        predicate_bound: usize,
        predicates: impl IntoIterator<Item = u64>,
    ) -> Result<(ChargedVec<u64>, ChargedVec<u64>), BudgetExceeded> {
        let mut touched = ChargedVec::collect(Arc::clone(budget), bound, terms)?;
        sort_dedup(&mut touched);
        let mut predicates = ChargedVec::collect(Arc::clone(budget), predicate_bound, predicates)?;
        sort_dedup(&mut predicates);
        Ok((touched, predicates))
    }

    /// Room in the lineage's map and order for one more step. Where either is full, both
    /// are made anew for twice the steps (at least 16): the new storage charged first (old
    /// and new are held together while the steps move), the old freed, then its charge
    /// released. Growth is never left to the map itself, whose tombstones could make it
    /// grow past what was charged. `false` where the budget refuses it.
    fn room_for_step(state: &mut State, budget: &Arc<Budget>) -> bool {
        let len = state.steps.len();
        if len < state.steps.capacity() && state.order.len() < state.order.capacity() {
            return true;
        }
        let target = (len * 2).max(16);
        let mut charge = Reservation::new(Arc::clone(budget));
        if charge.charge(slot_bytes(target)).is_err() {
            return false;
        }
        let mut steps = HashMap::with_capacity(target);
        steps.extend(state.steps.drain());
        let mut order = VecDeque::with_capacity(target);
        order.extend(state.order.drain(..));
        // The old storage is freed by these assignments, then its charge by the last.
        state.steps = steps;
        state.order = order;
        state._slots = charge;
        true
    }

    fn insert_step(
        &self,
        state: &mut State,
        parent: Content,
        child: Content,
        (touched, predicates): (ChargedVec<u64>, ChargedVec<u64>),
        subjects: bool,
    ) {
        let (touched, touched_charge) = touched.into_parts();
        let (predicates, predicates_charge) = predicates.into_parts();
        let replaced = state.steps.insert(
            child,
            Step {
                parent,
                touched,
                predicates,
                subjects,
                _charges: [touched_charge, predicates_charge],
            },
        );
        // A content is made once; a second step for it would leave its first in the order.
        if replaced.is_none() {
            state.order.push_back(child);
        }
        self.steps.fetch_add(1, Ordering::Relaxed);
        Self::prune(state);
    }

    /// Records that `child` was made from `parent` touching `terms(subjects)` (at most
    /// `bound`) and statements of `predicates` (at most `predicate_bound`), raw ids: charged
    /// to the budget before they are allocated, the oldest steps let go for room; not
    /// recorded where they don't fit (a search there walks). `subjects`, whether every
    /// subject is among the terms, holds where a kept corpus reads equality classes, read
    /// under the state's lock with the step's insertion: a corpus kept later finds the step
    /// without them, and walks past it ([`Self::find`]).
    fn record_terms<T: IntoIterator<Item = u64>>(
        &self,
        parent: Content,
        child: Content,
        bound: usize,
        terms: impl Fn(bool) -> T,
        predicate_bound: usize,
        predicates: impl IntoIterator<Item = u64> + Clone,
    ) {
        if parent == child {
            return;
        }
        let mut state = self.state.lock();
        // Under the state's lock (as `set_budget` sets it): a step is charged to the budget
        // the lineage it joins is on.
        let budget = self.budget();
        // Steps lead from a version to a kept corpus: with none kept, none are.
        if state.kept.is_empty() {
            Self::forget_lineage(&mut state, &budget);
            return;
        }
        let subjects = state
            .kept
            .iter()
            .any(|k| reads_classes(k.kind.0, k.kind.1, k.kind.3));
        // A slot for it first; the oldest steps let go for room (they free slots).
        while !Self::room_for_step(&mut state, &budget) {
            if !Self::drop_oldest_step(&mut state) {
                return;
            }
        }
        let built = loop {
            let attempt = Self::charged_step(
                &budget,
                bound,
                terms(subjects),
                predicate_bound,
                predicates.clone(),
            );
            match attempt {
                Ok(built) => break built,
                Err(_) if Self::drop_oldest_step(&mut state) => {}
                Err(_) => return,
            }
        };
        self.insert_step(&mut state, parent, child, built, subjects);
    }

    /// Lets the lineage go, its storage freed before its charge is released; the next
    /// storage is charged to `budget`.
    fn forget_lineage(state: &mut State, budget: &Arc<Budget>) {
        state.steps = HashMap::new();
        state.order = VecDeque::new();
        state._slots = Reservation::new(Arc::clone(budget));
    }

    /// Lets the oldest step go; `false` if there is none.
    fn drop_oldest_step(state: &mut State) -> bool {
        let Some(oldest) = state.order.pop_front() else {
            return false;
        };
        state.steps.remove(&oldest);
        true
    }

    /// Lets an entry go: its corpus's shared allocation stays charged until the searches
    /// holding it let go ([`State::retired`]).
    fn retire(state: &mut State, kept: Kept) {
        if let Some((corpus, charge)) = kept.corpus {
            state.retired.push((Arc::downgrade(&corpus), charge));
        }
        Self::sweep(state);
    }

    /// Frees the retired corpora no search holds, then releases their charges (in that
    /// order: a tuple drops its fields in order).
    fn sweep(state: &mut State) {
        state
            .retired
            .retain(|(corpus, _)| corpus.strong_count() > 0);
    }

    /// Drops the oldest steps no path to a kept corpus takes: contents are numbered as
    /// made, so a path from a version down to a kept corpus only passes steps made after
    /// that corpus's version. (The budget bounds the rest: [`Self::record_terms`].)
    fn prune(state: &mut State) {
        let oldest_kept = state.kept.iter().map(|k| k.content.raw()).min();
        while let Some(&oldest) = state.order.front() {
            if oldest_kept.is_some_and(|kept| oldest.raw() > kept) {
                break;
            }
            Self::drop_oldest_step(state);
        }
    }

    /// Lets the least recently used entry other than `(content, kind)` go; `false` if
    /// there is none.
    fn evict_one(state: &mut State, keep: Option<(Content, KindRef<'_>)>) -> bool {
        let at = state.kept.iter().position(|k| {
            keep.is_none_or(|(content, kind)| k.content != content || !kind.matches(&k.kind))
        });
        match at {
            Some(at) => {
                let kept = state.kept.remove(at);
                Self::retire(state, kept);
                true
            }
            None => false,
        }
    }

    /// The corpus `kind` of the admitted snapshot's version: kept, or to build, with the
    /// nearest kept ancestor's corpus and the terms touched since (collected on `request`,
    /// the searching request's budget) where the lineage has one; none past a step that
    /// changed a `sameAs` statement the reader expands. Only a published snapshot (or a
    /// restriction of one) is admitted (contract S3): it may claim an entry and evict.
    pub(crate) fn find(
        &self,
        admitted: &CorpusAdmission<'_>,
        kind: KindRef<'_>,
        request: &Arc<Budget>,
    ) -> Result<Found, BudgetExceeded> {
        let version = admitted.version();
        let content = version.content;
        let per_probe = self.tuning().documents_per_probe;
        let mut state = self.state.lock();
        let at = state
            .kept
            .iter()
            .position(|k| k.content == content && kind.matches(&k.kind));
        let building = match at {
            Some(at) => {
                // The most recently used goes last.
                let mut kept = state.kept.remove(at);
                kept.version = Arc::downgrade(version);
                let corpus = kept.corpus.as_ref().map(|(corpus, _)| Arc::clone(corpus));
                state.kept.push(kept);
                if let Some(corpus) = corpus {
                    return Ok(Found::Kept(corpus));
                }
                true
            }
            None => false,
        };
        // The nearest ancestor with a kept, built corpus of this kind, and the steps there:
        // looked for no further back than the most terms any kept corpus of this kind is
        // worth deriving over.
        let reach = state
            .kept
            .iter()
            .filter(|k| kind.matches(&k.kind))
            .filter_map(|k| k.corpus.as_ref())
            .map(|(corpus, _)| derive_bound(corpus.documents(), per_probe))
            .max();
        let same_as = kind.equality.map(TermId::raw);
        let mut ancestor = None;
        let (mut terms, mut count) = (0, 0);
        for step in up(&state.steps, content) {
            if same_as.is_some_and(|same_as| step.predicates.binary_search(&same_as).is_ok()) {
                break;
            }
            // A step recorded while no corpus read classes lacks the other subjects such
            // a reader's members change by: walked.
            if kind.reads_classes() && !step.subjects {
                break;
            }
            terms += kind.probed_len(&step.touched);
            count += 1;
            let at = step.parent;
            if let Some(corpus) = state
                .kept
                .iter()
                .find(|k| k.content == at && kind.matches(&k.kind))
                .and_then(|k| k.corpus.as_ref().map(|(corpus, _)| corpus))
            {
                ancestor = Some(Arc::clone(corpus));
                break;
            }
            if reach.is_none_or(|reach| terms as u64 > reach) {
                break;
            }
        }
        let from = match ancestor {
            Some(base) if worth_deriving(terms, base.documents(), per_probe) => {
                let all = up(&state.steps, content)
                    .take(count)
                    .flat_map(|step| kind.probed(&step.touched).into_iter().flatten().copied());
                let mut touched = ChargedVec::collect(Arc::clone(request), terms, all)?;
                sort_dedup(&mut touched);
                Some((base, touched))
            }
            _ => None,
        };
        if building {
            return Ok(Found::Build { from, keep: false });
        }
        // A newer corpus of this kind: the kept ones of versions no snapshot holds go.
        while let Some(at) = state
            .kept
            .iter()
            .position(|k| kind.matches(&k.kind) && k.version.strong_count() == 0)
        {
            let kept = state.kept.remove(at);
            Self::retire(&mut state, kept);
        }
        let mut key = Reservation::new(self.budget());
        while key.charge(key_bytes(kind)).is_err() {
            if !Self::evict_one(&mut state, None) {
                Self::prune(&mut state);
                return Ok(Found::Build { from, keep: false });
            }
        }
        state.kept.push(Kept {
            content,
            kind: kind.to_owned(),
            version: Arc::downgrade(version),
            _key: key,
            corpus: None,
        });
        Self::prune(&mut state);
        Ok(Found::Build { from, keep: true })
    }

    /// Builds the corpus `kind` of `version` on the store's budget with `build`, if its
    /// entry is still there and, once the least recently used others are let go, the
    /// budget has room for `bytes`; keeps it. `None` where it isn't kept (a search then
    /// builds it on its own budget). The build runs without the store's lock.
    pub(crate) fn keep_built(
        &self,
        admitted: &CorpusAdmission<'_>,
        kind: KindRef<'_>,
        bytes: usize,
        build: impl FnOnce(&Arc<Budget>) -> Result<Corpus, BudgetExceeded>,
    ) -> Option<Arc<Corpus>> {
        let content = admitted.version().content;
        let budget = self.budget();
        let entry = |state: &State| {
            state
                .kept
                .iter()
                .position(|k| k.content == content && kind.matches(&k.kind))
        };
        {
            let mut state = self.state.lock();
            entry(&state)?;
            while budget.remaining() < bytes {
                if !Self::evict_one(&mut state, Some((content, kind))) {
                    break;
                }
            }
        }
        let built = build(&budget).ok().and_then(|corpus| {
            let mut charge = Reservation::new(Arc::clone(&budget));
            charge.charge(SHARED).ok()?;
            Some((Arc::new(corpus), charge))
        });
        let mut state = self.state.lock();
        // A budget set while it was built (`set_budget` let every entry go, and an entry
        // made since is the new budget's): what was charged to the old one serves its
        // search alone, as where its entry went.
        let current = Arc::ptr_eq(&budget, &self.budget.lock());
        match (built, entry(&state).filter(|_| current)) {
            (Some((corpus, charge)), Some(at)) => {
                let mut kept = state.kept.remove(at);
                let shared = Arc::clone(&corpus);
                kept.corpus = Some((corpus, charge));
                state.kept.push(kept);
                Self::prune(&mut state);
                return Some(shared);
            }
            // Doesn't fit: nor its entry.
            (None, Some(at)) => {
                let kept = state.kept.remove(at);
                Self::retire(&mut state, kept);
            }
            // Its entry went meanwhile: shared until its search lets go.
            (Some((corpus, charge)), None) => {
                let shared = Arc::clone(&corpus);
                state.retired.push((Arc::downgrade(&corpus), charge));
                drop(corpus);
                Self::prune(&mut state);
                return Some(shared);
            }
            (None, None) => {}
        }
        Self::prune(&mut state);
        None
    }

    /// Lets go of the entry [`Self::find`] made for the corpus `kind` of `version` while it
    /// has no corpus yet: its build failed (a budget refused it, a read stopped early), so no
    /// corpus will fill it, and a later search builds and keeps its own.
    pub(crate) fn abandon(&self, admitted: &CorpusAdmission<'_>, kind: KindRef<'_>) {
        let content = admitted.version().content;
        let mut state = self.state.lock();
        if let Some(at) = state
            .kept
            .iter()
            .position(|k| k.content == content && kind.matches(&k.kind) && k.corpus.is_none())
        {
            let kept = state.kept.remove(at);
            Self::retire(&mut state, kept);
        }
        Self::prune(&mut state);
    }

    /// What a snapshot without the shared-state capability (a writer's pending state, a
    /// view that adds facts) derives its corpus `kind` from: a kept corpus of the same kind
    /// whose version's runs are a prefix of `version`'s in both stacks (as `with_delta` and
    /// `with_run` build a pending state: the base's runs and one more), the longest such,
    /// and the terms of every run past the prefix (inserts and tombstones), collected on
    /// `request`. `None`: walk. Reads the store only; nothing is claimed, moved, charged to
    /// the store or evicted (contract S3). The caller's derivation probes each touched term
    /// in its own snapshot, so the result is exact whatever the runs hold.
    ///
    /// A `private` version (a private transaction's, V3) has an equality cell of its own
    /// whatever it changes, so it is not asked to share the base's: its classes are the
    /// base's where its extra runs hold no `sameAs` statement, checked below in both stacks
    /// (the classes read the default graph's `sameAs` statements of both; equality.rs), and
    /// otherwise it walks. Every other version keeps the cell check (condition c: a
    /// restricted or adding view has a fresh cell and a fresh content; DICT2 9 Nov 04:02).
    pub(crate) fn derive_unadmitted(
        &self,
        version: &Arc<Version>,
        kind: KindRef<'_>,
        private: bool,
        request: &Arc<Budget>,
    ) -> Option<(Arc<Corpus>, ChargedVec<u64>)> {
        let per_probe = self.tuning().documents_per_probe;
        // The longest prefix among the kept corpora of this kind, under one equality cell:
        // a pending state that changes `owl:sameAs` has a new one, and walks.
        let (base, corpus) = {
            let state = self.state.lock();
            let mut best: Option<(usize, Arc<Version>, Arc<Corpus>)> = None;
            for kept in state.kept.iter().filter(|k| kind.matches(&k.kind)) {
                let Some((corpus, _)) = &kept.corpus else {
                    continue;
                };
                let Some(base) = kept.version.upgrade() else {
                    continue;
                };
                if !private && !Arc::ptr_eq(&base.equality, &version.equality) {
                    continue;
                }
                let Some(prefix) = run_prefix(&base, version) else {
                    continue;
                };
                if best.as_ref().is_none_or(|(longest, ..)| prefix > *longest) {
                    best = Some((prefix, base, Arc::clone(corpus)));
                }
            }
            let (_, base, corpus) = best?;
            (base, corpus)
        };
        let same_as = kind.equality.map(TermId::raw);
        let extra: Vec<&Arc<crate::index::run::Run>> = Stack::ALL
            .into_iter()
            .flat_map(|stack| {
                let had = base.stack(stack).runs().len();
                &version.stack(stack).runs()[had..]
            })
            .collect();
        // A step that changes a `sameAs` statement the reader expands changes its classes:
        // walked, as a lineage step there would be.
        if same_as.is_some_and(|same_as| {
            extra
                .iter()
                .any(|run| run.statements().any(|[_, p, _, _]| p == same_as))
        }) {
            return None;
        }
        let bound: usize = extra.iter().map(|run| 2 * run.entries() as usize).sum();
        // Every subject only for a reader of classes: no other corpus holds a blank node.
        let subjects = kind.reads_classes();
        let all = extra.iter().flat_map(|run| {
            run.statements()
                .flat_map(|[s, _, o, _]| terms(TermId::from_raw(s), TermId::from_raw(o), subjects))
        });
        // Where the request has no room for the touched terms, the walk is the way left (it
        // charges what it needs itself).
        let mut touched = ChargedVec::collect(Arc::clone(request), bound, all).ok()?;
        sort_dedup(&mut touched);
        worth_deriving(touched.len(), corpus.documents(), per_probe).then_some((corpus, touched))
    }

    /// Counts a corpus built, by a walk or derived.
    pub(crate) fn built(&self, derived: bool) {
        match derived {
            true => self.derived.fetch_add(1, Ordering::Relaxed),
            false => self.walked.fetch_add(1, Ordering::Relaxed),
        };
    }

    pub(crate) fn stats(&self) -> TextCorpusStats {
        let kept = self.state.lock().kept.len() as u64;
        let budget = self.budget();
        TextCorpusStats {
            started: true,
            kept,
            bytes: budget.used() as u64,
            budget: budget.limit() as u64,
            steps: self.steps.load(Ordering::Relaxed),
            walked: self.walked.load(Ordering::Relaxed),
            derived: self.derived.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::text::Bits;

    fn kind(scope: &TextScope) -> KindRef<'_> {
        KindRef {
            documents: Documents::Literals,
            model: ReadModel::Materialised,
            scope,
            equality: None,
            canonical: false,
        }
    }

    /// A corpus of one document, on `budget`.
    fn corpus(budget: &Arc<Budget>) -> Result<Corpus, BudgetExceeded> {
        let mut bits = ChargedVec::with_capacity(Arc::clone(budget), 1)?;
        bits.resize(1, 1)?;
        let bits = Bits {
            bits,
            documents: 1,
            total_words: 1,
        };
        Corpus::finish(&bits, 64, budget)
    }

    /// Sol-loop r2 R2-S1: a search builds its corpus on the store's budget; meanwhile the
    /// budget is set anew (every entry let go) and another search makes the same entry on
    /// the new one. What the first built is charged to the old budget: it serves that
    /// search, and the new entry stays for a corpus on the new budget.
    #[test]
    fn a_corpus_built_across_a_budget_change_is_not_kept_under_the_new_one() {
        let store = CorpusStore::default();
        let version = Arc::new(Version::empty());
        let scope = TextScope::EVERY;
        let request = Arc::new(Budget::unlimited());
        let fresh = Arc::new(Budget::new(1 << 20));
        let admitted = CorpusAdmission::for_tests(&version);
        let made = store.find(&admitted, kind(&scope), &request);
        assert!(matches!(made, Ok(Found::Build { keep: true, .. })));
        let served = store.keep_built(&admitted, kind(&scope), 64, |old| {
            store.set_budget(Arc::clone(&fresh));
            let again = store.find(&admitted, kind(&scope), &request);
            assert!(matches!(again, Ok(Found::Build { keep: true, .. })));
            corpus(old)
        });
        assert!(served.is_some(), "the search gets its corpus");
        let state = store.state.lock();
        assert_eq!(state.kept.len(), 1);
        assert!(
            state.kept.iter().all(|k| k.corpus.is_none()),
            "a corpus on the old budget kept under the new one"
        );
    }

    /// Sol-loop r2 R2-S2: the lineage's map and order keep their storage when steps go;
    /// that storage stays charged while it is held. 1,000 steps from a kept corpus, then a
    /// corpus kept at the last version: every step leads nowhere and goes, the storage
    /// stays. Oracle: the storage `std` reports (capacity times entry size, a lower bound).
    #[test]
    fn the_lineage_storage_stays_charged_while_it_is_held() {
        let budget = Arc::new(Budget::new(64 << 20));
        let store = CorpusStore::on(Arc::clone(&budget));
        let scope = TextScope::EVERY;
        let request = Arc::new(Budget::unlimited());
        let first = Arc::new(Version::empty());
        let admitted = CorpusAdmission::for_tests(&first);
        store.find(&admitted, kind(&scope), &request).unwrap();
        store
            .keep_built(&admitted, kind(&scope), 64, corpus)
            .unwrap();
        let iri = TermId::new(TermKind::Iri, 1);
        let mut parent = first.content;
        for _ in 0..1000 {
            let child = Content::fresh();
            store.record(parent, child, std::iter::once((iri, iri, iri)));
            parent = child;
        }
        drop(first);
        let last = Arc::new(Version {
            content: parent,
            ..Version::empty()
        });
        let admitted = CorpusAdmission::for_tests(&last);
        store.find(&admitted, kind(&scope), &request).unwrap();
        store
            .keep_built(&admitted, kind(&scope), 64, corpus)
            .unwrap();
        let state = store.state.lock();
        assert!(state.steps.is_empty(), "{} steps left", state.steps.len());
        let held = state.steps.capacity() * std::mem::size_of::<(Content, Step)>()
            + state.order.capacity() * std::mem::size_of::<Content>();
        assert!(held > 0);
        assert!(
            budget.used() >= held,
            "storage of {held} bytes held, {} charged",
            budget.used()
        );
    }
}
