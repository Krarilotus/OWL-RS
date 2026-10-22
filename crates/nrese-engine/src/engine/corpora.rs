//! The corpora of full-text searches ([`Snapshot::text_search_in`](super::Snapshot::text_search_in)),
//! kept, and derived from one another across commits.
//!
//! A corpus is exact for its statements' identity ([`Content`]), the index's documents, the
//! read model, the graphs (the scope, normalised so that equal graph sets share one) and
//! how the reader reads equality classes (`owl:sameAs` expanded or canonical, or as
//! stored): its members are the terms the reader's own scans show. Building one walks the
//! scope's statements; a version that a commit made from another (or a pending state, or
//! a reader's view made from the reader's earlier one) instead takes the corpus of its
//! nearest kept ancestor and looks again at the terms the steps between them touched:
//! each is a member if a statement the reader sees in the scope still has it, a probe. So
//! a search after a small commit costs probes for what the commit touched, not a walk,
//! and deletes stay exact (a literal stays while any visible statement has it). Where a
//! step changed a `sameAs` statement the reader expands, the classes changed and the
//! corpus is walked again; so is an IRIs' corpus of a reader of equality classes.
//!
//! The lineage (content → its parent, the terms and predicates touched) is kept only
//! while a corpus is kept, back to the oldest kept one, for at most [`STEPS`] steps and
//! [`STEP_TERMS`] terms: a store nobody searches keeps none. Beyond, or where more terms
//! were touched than a probe each is worth ([`worth_deriving`]), the corpus is walked again.
//!
//! Memory is the memory-accounting owner's ([`nrese_exec::ChargedVec`],
//! [`nrese_exec::Reservation`]), charged before it is allocated. What the store keeps is
//! charged to its budget ([`crate::Engine::set_text_corpus_budget`]; at first one of
//! [`crate::EngineConfig::text_corpus_bytes`]): a kept corpus's members (built on it, so
//! nothing moves between budgets), each entry's key and each lineage step; a kept corpus
//! an evicted entry's search still holds stays charged until it lets go. What doesn't fit
//! after the least recently used are let go isn't kept: the corpus is then built on the
//! searching request's budget and serves that search alone, as everything a search builds
//! besides (the bits, the walk's counts, the probes' list). At most [`ENTRIES`] are kept;
//! the budget's use is [`crate::EngineStats::text_corpora`].

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
use parking_lot::Mutex;

use super::snapshot::{Documents, TextScope};
use super::{Content, ReadModel, Version};
use crate::quad::EncodedQuad;
use crate::term::text::Corpus;
use crate::term::{TermId, TermKind};

/// Corpora kept at most.
pub(crate) const ENTRIES: usize = 64;

/// Lineage steps kept, and the terms they may hold together.
const STEPS: usize = 4096;
const STEP_TERMS: usize = 1 << 18;

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
    fn matches(&self, kind: &CorpusKind) -> bool {
        kind.0 == self.documents
            && kind.1 == self.model
            && kind.2 == *self.scope
            && kind.3 == self.equality
            && kind.4 == self.canonical
    }

    fn to_owned(self) -> CorpusKind {
        (
            self.documents,
            self.model,
            self.scope.clone(),
            self.equality,
            self.canonical,
        )
    }
}

/// Counts of the corpora (for [`crate::EngineStats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextCorpusStats {
    /// Corpora kept.
    pub kept: u64,
    /// Bytes charged to the corpora's budget: kept corpora (and evicted ones a search
    /// still holds), entries' keys, the lineage.
    pub bytes: u64,
    /// The budget's limit.
    pub budget: u64,
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

/// One step of the lineage: the content it was made from, the terms it touched (string
/// literals as objects, IRIs as subjects; raw ids, sorted), the predicates of the
/// statements it added or removed (raw ids, sorted), each on the store's budget, and the
/// charge of the step's own entry in the map and the order.
struct Step {
    parent: Content,
    touched: Vec<u64>,
    predicates: Vec<u64>,
    _charges: [Reservation; 3],
}

/// Bytes a step's entry takes in the map and the order.
const STEP_ENTRY: usize =
    std::mem::size_of::<(Content, Step)>() + 2 * std::mem::size_of::<Content>() + 16;

struct State {
    /// Least recently used first.
    kept: Vec<Kept>,
    /// Kept corpora let go while searches may still hold them: each with the charge of its
    /// shared allocation, released once the last search let go (when it is freed).
    retired: Vec<(Weak<Corpus>, Reservation)>,
    steps: HashMap<Content, Step>,
    order: VecDeque<Content>,
    step_terms: usize,
}

/// The kept corpora and the lineage, shared by the snapshots of an engine and the views
/// made from them.
pub(crate) struct CorpusStore {
    budget: Mutex<Arc<Budget>>,
    state: Mutex<State>,
    walked: AtomicU64,
    derived: AtomicU64,
}

impl Default for CorpusStore {
    fn default() -> Self {
        Self::new(crate::EngineConfig::default().text_corpus_bytes)
    }
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

/// The terms of a statement a corpus may hold: its object if a string literal or a typed
/// one (a vector, for the corpora of vector searches), its subject if an IRI (raw ids).
fn terms(
    subject: TermId,
    object: TermId,
) -> std::iter::Chain<std::option::IntoIter<u64>, std::option::IntoIter<u64>> {
    let object = matches!(
        object.kind(),
        TermKind::String | TermKind::LangString | TermKind::TypedLiteral
    )
    .then_some(object.raw());
    let subject = (subject.kind() == TermKind::Iri).then_some(subject.raw());
    object.into_iter().chain(subject)
}

/// Whether probing `touched` terms beats walking a corpus of `documents`: a probe costs
/// about what a walk spends on sixteen documents.
pub(crate) fn worth_deriving(touched: usize, documents: u64) -> bool {
    touched as u64 <= (documents / 16).max(1024)
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

/// The steps from `from` up its lineage, nearest first, while `steps` holds them.
fn up(steps: &HashMap<Content, Step>, from: Content) -> impl Iterator<Item = &Step> + Clone {
    std::iter::successors(steps.get(&from), |step| steps.get(&step.parent)).take(STEPS)
}

impl CorpusStore {
    pub(crate) fn new(budget: usize) -> Self {
        Self::on(Arc::new(Budget::new(budget)))
    }

    fn on(budget: Arc<Budget>) -> Self {
        Self {
            budget: Mutex::new(budget),
            state: Mutex::new(State {
                kept: Vec::new(),
                retired: Vec::new(),
                steps: HashMap::new(),
                order: VecDeque::new(),
                step_terms: 0,
            }),
            walked: AtomicU64::new(0),
            derived: AtomicU64::new(0),
        }
    }

    /// The budget what the store keeps from now on is charged to: kept corpora and steps
    /// let go (they stay charged to the old one while searches hold them).
    pub(crate) fn set_budget(&self, budget: Budget) {
        let mut state = self.state.lock();
        for kept in std::mem::take(&mut state.kept) {
            Self::retire(&mut state, kept);
        }
        Self::forget_lineage(&mut state);
        *self.budget.lock() = Arc::new(budget);
    }

    /// The budget what the store keeps is charged to.
    pub(crate) fn budget(&self) -> Arc<Budget> {
        Arc::clone(&self.budget.lock())
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
        let terms = statements
            .clone()
            .flat_map(|(subject, _, object)| terms(subject, object));
        let predicates = statements.map(|(_, predicate, _)| predicate.raw());
        self.record_terms(parent, child, 2 * n, terms, n, predicates);
    }

    /// Records that the view `child` of the version `base` was made from the view
    /// `earlier` of `earlier_base` (a reader's view at a later commit), touching what the
    /// steps from `earlier_base` to `base` touched and the statements of `hide` and `show`
    /// (those the view shows or hides now and didn't before). Nothing where the lineage lacks
    /// those steps, or the budget has no room for the step.
    pub(crate) fn record_view(
        &self,
        earlier: Content,
        earlier_base: Content,
        child: Content,
        base: Content,
        hide: &[EncodedQuad],
        show: &[EncodedQuad],
    ) {
        if earlier == child {
            return;
        }
        let budget = self.budget();
        let mut state = self.state.lock();
        let built = {
            // The steps from `base` back to `earlier_base`: how many, if they lead there.
            let Some(count) = (base == earlier_base).then_some(0).or_else(|| {
                up(&state.steps, base)
                    .position(|step| step.parent == earlier_base)
                    .map(|at| at + 1)
            }) else {
                return;
            };
            let path = up(&state.steps, base).take(count);
            let changed = hide.iter().chain(show);
            let n = hide.len() + show.len();
            let bound = path.clone().map(|s| s.touched.len()).sum::<usize>() + 2 * n;
            let predicate_bound = path.clone().map(|s| s.predicates.len()).sum::<usize>() + n;
            let terms = path.clone().flat_map(|s| s.touched.iter().copied()).chain(
                changed
                    .clone()
                    .flat_map(|quad| terms(quad.subject, quad.object)),
            );
            let predicates = path
                .flat_map(|s| s.predicates.iter().copied())
                .chain(changed.map(|quad| quad.predicate.raw()));
            match Self::charged_step(&budget, bound, terms, predicate_bound, predicates) {
                Ok(built) => built,
                Err(_) => return,
            }
        };
        Self::insert_step(&mut state, earlier, child, built);
    }

    /// A step's entry charge and its terms and predicates, sorted, on `budget`.
    #[allow(clippy::type_complexity)]
    fn charged_step(
        budget: &Arc<Budget>,
        bound: usize,
        terms: impl IntoIterator<Item = u64>,
        predicate_bound: usize,
        predicates: impl IntoIterator<Item = u64>,
    ) -> Result<(Reservation, ChargedVec<u64>, ChargedVec<u64>), BudgetExceeded> {
        let mut entry = Reservation::new(Arc::clone(budget));
        entry.charge(STEP_ENTRY)?;
        let mut touched = ChargedVec::collect(Arc::clone(budget), bound, terms)?;
        sort_dedup(&mut touched);
        let mut predicates = ChargedVec::collect(Arc::clone(budget), predicate_bound, predicates)?;
        sort_dedup(&mut predicates);
        Ok((entry, touched, predicates))
    }

    fn insert_step(
        state: &mut State,
        parent: Content,
        child: Content,
        (entry, touched, predicates): (Reservation, ChargedVec<u64>, ChargedVec<u64>),
    ) {
        let (touched, touched_charge) = touched.into_parts();
        let (predicates, predicates_charge) = predicates.into_parts();
        state.step_terms += touched.len();
        state.steps.insert(
            child,
            Step {
                parent,
                touched,
                predicates,
                _charges: [entry, touched_charge, predicates_charge],
            },
        );
        state.order.push_back(child);
        Self::prune(state);
    }

    /// Records that `child` was made from `parent` touching `terms` (at most `bound`) and
    /// statements of `predicates` (at most `predicate_bound`), raw ids: charged to the
    /// budget before they are allocated, the oldest steps let go for room; not recorded
    /// where they don't fit (a search there walks).
    fn record_terms(
        &self,
        parent: Content,
        child: Content,
        bound: usize,
        terms: impl IntoIterator<Item = u64> + Clone,
        predicate_bound: usize,
        predicates: impl IntoIterator<Item = u64> + Clone,
    ) {
        if parent == child {
            return;
        }
        let budget = self.budget();
        let mut state = self.state.lock();
        // Steps lead from a version to a kept corpus: with none kept, none are.
        if state.kept.is_empty() {
            Self::forget_lineage(&mut state);
            return;
        }
        let built = loop {
            let attempt = Self::charged_step(
                &budget,
                bound,
                terms.clone(),
                predicate_bound,
                predicates.clone(),
            );
            match attempt {
                Ok(built) => break built,
                Err(_) if Self::drop_oldest_step(&mut state) => {}
                Err(_) => return,
            }
        };
        Self::insert_step(&mut state, parent, child, built);
    }

    fn forget_lineage(state: &mut State) {
        state.steps.clear();
        state.order.clear();
        state.step_terms = 0;
    }

    /// Lets the oldest step go; `false` if there is none.
    fn drop_oldest_step(state: &mut State) -> bool {
        let Some(oldest) = state.order.pop_front() else {
            return false;
        };
        if let Some(step) = state.steps.remove(&oldest) {
            state.step_terms -= step.touched.len();
        }
        true
    }

    /// Drops the oldest steps beyond the bounds, and those no path to a kept corpus takes:
    /// contents are numbered as made, so a path from a version down to a kept corpus only
    /// passes steps made after that corpus's version.
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

    fn prune(state: &mut State) {
        let oldest_kept = state.kept.iter().map(|k| k.content.raw()).min();
        while let Some(&oldest) = state.order.front() {
            let useless = oldest_kept.is_none_or(|kept| oldest.raw() <= kept);
            if !useless && state.order.len() <= STEPS && state.step_terms <= STEP_TERMS {
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

    /// The corpus `kind` of `version`: kept, or to build, with the nearest kept ancestor's
    /// corpus and the terms touched since (collected on `request`, the searching request's
    /// budget) where the lineage has one; none past a step that changed a `sameAs`
    /// statement the reader expands.
    pub(crate) fn find(
        &self,
        version: &Arc<Version>,
        kind: KindRef<'_>,
        request: &Arc<Budget>,
    ) -> Result<Found, BudgetExceeded> {
        let content = version.content;
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
        // The nearest ancestor with a kept, built corpus of this kind, and the steps there.
        let same_as = kind.equality.map(TermId::raw);
        let mut ancestor = None;
        let (mut terms, mut count) = (0, 0);
        for step in up(&state.steps, content) {
            if same_as.is_some_and(|same_as| step.predicates.binary_search(&same_as).is_ok()) {
                break;
            }
            terms += step.touched.len();
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
            if terms > STEP_TERMS {
                break;
            }
        }
        let from = match ancestor {
            Some(base) if worth_deriving(terms, base.documents()) => {
                let all = up(&state.steps, content)
                    .take(count)
                    .flat_map(|step| step.touched.iter().copied());
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
        while state.kept.len() >= ENTRIES {
            Self::evict_one(&mut state, None);
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
        version: &Arc<Version>,
        kind: KindRef<'_>,
        bytes: usize,
        build: impl FnOnce(&Arc<Budget>) -> Result<Corpus, BudgetExceeded>,
    ) -> Option<Arc<Corpus>> {
        let content = version.content;
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
        match (&built, entry(&state)) {
            (Some(_), Some(at)) => {
                let mut kept = state.kept.remove(at);
                let (corpus, charge) = built.expect("matched");
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
            (Some(_), None) => {
                let (corpus, charge) = built.expect("matched");
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
            kept,
            bytes: budget.used() as u64,
            budget: budget.limit() as u64,
            walked: self.walked.load(Ordering::Relaxed),
            derived: self.derived.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use nrese_rdf::{GraphName, Literal, LiteralRef, NamedNode, Quad};

    use crate::{Engine, EngineConfig, TextQuery};

    /// A search runs no parallel part under a lock of the dictionary or of the store's
    /// corpora: the index's build, the corpus's walk (a part per slice of the bits) and
    /// the counting under the index's lock alone, on one thread. Each part here takes the
    /// locks a stolen job might want, exclusively, and interns; holding one across the
    /// parts, the search would never end (the guard fails after 30 s).
    #[test]
    fn searches_run_their_parallel_parts_without_the_lock() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let documents = 3 * crate::term::text::SERIAL_DOCUMENTS;
        for i in 0..documents {
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked(format!("http://example.com/s{i}")),
                    NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
                    Literal::new_simple_literal(format!("label word{} {i}", i % 97)),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.commit().unwrap();
        let dictionary = Arc::clone(&engine.inner.shared.dictionary);
        let interned = Arc::new(AtomicU64::new(0));
        let hook: Arc<dyn Fn() + Send + Sync> = {
            let (dictionary, interned) = (Arc::downgrade(&dictionary), Arc::clone(&interned));
            Arc::new(move || {
                let n = interned.fetch_add(1, Ordering::Relaxed);
                let dictionary = dictionary.upgrade().expect("the dictionary");
                dictionary.take_search_locks();
                dictionary.intern(LiteralRef::new_simple_literal(&format!("stolen {n}")).into());
            })
        };
        *dictionary.part_hook.lock() = Some(hook);
        let snapshot = engine.snapshot();
        let query = TextQuery {
            text: "word3".to_owned(),
            all_words: false,
            prefix: false,
            stem: None,
        };
        let (done, finished) = std::sync::mpsc::channel();
        let searching = snapshot.clone();
        let names = TextQuery {
            text: "s1*".to_owned(),
            prefix: true,
            ..query.clone()
        };
        std::thread::spawn(move || {
            let found = searching.text_search(&query).unwrap().len();
            let named = searching
                .iri_search_in(
                    crate::ReadModel::Materialised,
                    &crate::TextScope::EVERY,
                    &names,
                    None,
                )
                .unwrap()
                .len();
            let _ = done.send((found, named));
        });
        let (found, named) = finished
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the search ended: no lock held across its parallel parts");
        *dictionary.part_hook.lock() = None;
        assert!(interned.load(Ordering::Relaxed) >= 2, "parts ran the hook");
        assert_eq!(found as u64, documents.div_ceil(97));
        assert!(named > 0);
    }
}
