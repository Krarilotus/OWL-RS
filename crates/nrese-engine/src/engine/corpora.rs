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
//! Memory, by [`nrese_exec::Reservation`]s on the engine's budget for corpora
//! ([`crate::EngineConfig::text_corpus_bytes`]): a kept entry's key from when it is
//! entered, a kept corpus's members from when it is kept until the last search holding it
//! lets go (an evicted corpus a search still uses stays charged), and each lineage step.
//! What doesn't fit, after the least recently used are let go, isn't kept: a corpus
//! larger than the budget serves its search and goes. At most [`ENTRIES`] are kept. A
//! corpus being built, and one not kept, is the search's: charged to its request's budget
//! ([`super::Snapshot::text_search_in`]). The budget's use is
//! [`crate::EngineStats::text_corpora`].

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use nrese_exec::{Budget, Reservation};
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

/// Counts of the corpora (for [`crate::EngineStats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextCorpusStats {
    /// Corpora kept.
    pub kept: u64,
    /// Bytes charged to the corpora's budget: kept entries' keys, kept corpora (and evicted
    /// ones a search still holds), the lineage.
    pub bytes: u64,
    /// The budget.
    pub budget: u64,
    /// Corpora built by a walk, and derived from an ancestor's, since the engine started.
    pub walked: u64,
    pub derived: u64,
}

/// A corpus as searches share it, with the charge of its bytes to the corpora's budget
/// when it is kept (released when the last search holding it lets go).
pub(crate) struct Held {
    corpus: Corpus,
    _charge: Option<Reservation>,
}

impl Held {
    /// A corpus not kept: the search that built it holds it, on its request's budget.
    pub(crate) fn transient(corpus: Corpus) -> Arc<Self> {
        Arc::new(Self {
            corpus,
            _charge: None,
        })
    }
}

impl Held {
    /// Whether the corpora's budget holds its bytes (it is kept).
    pub(crate) fn charged(&self) -> bool {
        self._charge.is_some()
    }
}

impl std::ops::Deref for Held {
    type Target = Corpus;

    fn deref(&self) -> &Corpus {
        &self.corpus
    }
}

/// Bytes a held corpus takes: its members, itself, the `Arc`'s counts.
fn held_bytes(corpus: &Corpus) -> usize {
    corpus.memory_bytes() + std::mem::size_of::<Held>() + 16
}

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
    corpus: Option<Arc<Held>>,
}

/// Bytes a kept entry takes besides its corpus: itself and its graphs.
fn key_bytes(kind: &CorpusKind) -> usize {
    let graphs = match &kind.2 {
        TextScope::Merge(graphs) => graphs.capacity() * std::mem::size_of::<TermId>(),
        TextScope::Graphs(_) => 0,
    };
    std::mem::size_of::<Kept>() + graphs
}

/// One step of the lineage: the content it was made from, the terms it touched (string
/// literals as objects, IRIs as subjects; raw ids, sorted), the predicates of the
/// statements it added or removed (raw ids, sorted), and its charge.
struct Step {
    parent: Content,
    touched: Arc<[u64]>,
    predicates: Box<[u64]>,
    bytes: usize,
}

/// Bytes a step takes: itself, its key in the map and the order, its terms and predicates.
fn step_bytes(touched: usize, predicates: usize) -> usize {
    std::mem::size_of::<Step>()
        + 3 * std::mem::size_of::<Content>()
        + 16
        + (touched + predicates) * std::mem::size_of::<u64>()
}

struct State {
    /// Least recently used first.
    kept: Vec<Kept>,
    steps: HashMap<Content, Step>,
    order: VecDeque<Content>,
    step_terms: usize,
    /// The steps' charge.
    lineage: Reservation,
}

/// The kept corpora and the lineage, shared by the snapshots of an engine and the views
/// made from them.
pub(crate) struct CorpusStore {
    budget: Arc<Budget>,
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
/// (with the terms touched since) or to walk; and whether to keep what it builds (not
/// where another search is building it, or its key didn't fit the budget).
pub(crate) enum Found {
    Kept(Arc<Held>),
    Build {
        from: Option<(Arc<Held>, Vec<u64>)>,
        keep: bool,
    },
}

/// The terms of a statement a corpus may hold: its object if a string literal, its
/// subject if an IRI (raw ids).
fn terms(subject: TermId, object: TermId) -> impl Iterator<Item = u64> {
    let object =
        matches!(object.kind(), TermKind::String | TermKind::LangString).then_some(object.raw());
    let subject = (subject.kind() == TermKind::Iri).then_some(subject.raw());
    object.into_iter().chain(subject)
}

/// Whether probing `touched` terms beats walking a corpus of `documents`: a probe costs
/// about what a walk spends on sixteen documents.
pub(crate) fn worth_deriving(touched: usize, documents: u64) -> bool {
    touched as u64 <= (documents / 16).max(1024)
}

impl CorpusStore {
    pub(crate) fn new(budget: usize) -> Self {
        let budget = Arc::new(Budget::new(budget));
        Self {
            state: Mutex::new(State {
                kept: Vec::new(),
                steps: HashMap::new(),
                order: VecDeque::new(),
                step_terms: 0,
                lineage: Reservation::new(Arc::clone(&budget)),
            }),
            budget,
            walked: AtomicU64::new(0),
            derived: AtomicU64::new(0),
        }
    }

    /// Records that `child` was made from `parent`, adding or removing these statements
    /// (subject, predicate, object).
    pub(crate) fn record(
        &self,
        parent: Content,
        child: Content,
        statements: impl IntoIterator<Item = (TermId, TermId, TermId)>,
    ) {
        if self.state.lock().kept.is_empty() {
            return;
        }
        let (mut touched, mut predicates) = (Vec::new(), Vec::new());
        for (subject, predicate, object) in statements {
            touched.extend(terms(subject, object));
            predicates.push(predicate.raw());
        }
        self.record_terms(parent, child, touched, predicates);
    }

    /// Records that the view `child` of the version `base` was made from the view
    /// `earlier` of `earlier_base` (a reader's view at a later commit), touching what the
    /// steps from `earlier_base` to `base` touched and the statements of `changed` (those
    /// the view shows or hides now and didn't before). Nothing where the lineage lacks
    /// those steps.
    pub(crate) fn record_view<'q>(
        &self,
        earlier: Content,
        earlier_base: Content,
        child: Content,
        base: Content,
        changed: impl IntoIterator<Item = &'q EncodedQuad>,
    ) {
        let Some((mut touched, mut predicates)) = self.touched_between(earlier_base, base) else {
            return;
        };
        for quad in changed {
            touched.extend(terms(quad.subject, quad.object));
            predicates.push(quad.predicate.raw());
        }
        self.record_terms(earlier, child, touched, predicates);
    }

    /// Records that `child` was made from `parent` touching `touched` and statements of
    /// `predicates` (raw ids): charged to the budget, the oldest steps let go for it; not
    /// recorded where it doesn't fit (a search there walks).
    fn record_terms(
        &self,
        parent: Content,
        child: Content,
        mut touched: Vec<u64>,
        mut predicates: Vec<u64>,
    ) {
        if parent == child {
            return;
        }
        touched.sort_unstable();
        touched.dedup();
        predicates.sort_unstable();
        predicates.dedup();
        let mut state = self.state.lock();
        // Steps lead from a version to a kept corpus: with none kept, none are.
        if state.kept.is_empty() {
            Self::forget_lineage(&mut state);
            return;
        }
        let bytes = step_bytes(touched.len(), predicates.len());
        while state.lineage.charge(bytes).is_err() {
            if !Self::drop_oldest_step(&mut state) {
                return;
            }
        }
        state.step_terms += touched.len();
        state.steps.insert(
            child,
            Step {
                parent,
                touched: touched.into(),
                predicates: predicates.into(),
                bytes,
            },
        );
        state.order.push_back(child);
        Self::prune(&mut state);
    }

    fn forget_lineage(state: &mut State) {
        state.steps.clear();
        state.order.clear();
        state.step_terms = 0;
        let held = state.lineage.bytes();
        state.lineage.release(held);
    }

    /// Lets the oldest step go; `false` if there is none.
    fn drop_oldest_step(state: &mut State) -> bool {
        let Some(oldest) = state.order.pop_front() else {
            return false;
        };
        if let Some(step) = state.steps.remove(&oldest) {
            state.step_terms -= step.touched.len();
            state.lineage.release(step.bytes);
        }
        true
    }

    /// Drops the oldest steps beyond the bounds, and those no path to a kept corpus takes:
    /// contents are numbered as made, so a path from a version down to a kept corpus only
    /// passes steps made after that corpus's version.
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

    /// The terms and predicates the steps from `ancestor` to `content` touched, if the
    /// lineage holds them all.
    fn touched_between(&self, ancestor: Content, content: Content) -> Option<(Vec<u64>, Vec<u64>)> {
        let state = self.state.lock();
        let (mut terms, mut predicates) = (Vec::new(), Vec::new());
        let mut at = content;
        for _ in 0..STEPS {
            if at == ancestor {
                return Some((terms, predicates));
            }
            let step = state.steps.get(&at)?;
            terms.extend_from_slice(&step.touched);
            predicates.extend_from_slice(&step.predicates);
            at = step.parent;
        }
        None
    }

    /// Lets the least recently used entry other than `(content, kind)` go; `false` if
    /// there is none.
    fn evict_one(state: &mut State, keep: Option<(Content, &CorpusKind)>) -> bool {
        let at = state.kept.iter().position(|k| {
            keep.is_none_or(|(content, kind)| k.content != content || k.kind != *kind)
        });
        match at {
            Some(at) => {
                state.kept.remove(at);
                true
            }
            None => false,
        }
    }

    /// The corpus `kind` of `version`: kept, or to build, with the nearest kept ancestor's
    /// corpus and the terms touched since where the lineage has one (none past a step that
    /// changed a `sameAs` statement the reader expands).
    pub(crate) fn find(&self, version: &Arc<Version>, kind: &CorpusKind) -> Found {
        let content = version.content;
        let mut state = self.state.lock();
        let at = state
            .kept
            .iter()
            .position(|k| k.content == content && k.kind == *kind);
        let building = match at {
            Some(at) => {
                // The most recently used goes last.
                let mut kept = state.kept.remove(at);
                kept.version = Arc::downgrade(version);
                let corpus = kept.corpus.clone();
                state.kept.push(kept);
                if let Some(corpus) = corpus {
                    return Found::Kept(corpus);
                }
                true
            }
            None => false,
        };
        // The nearest ancestor with a kept, built corpus of this kind.
        let same_as = kind.3.map(TermId::raw);
        let mut from = None;
        let mut terms: Vec<u64> = Vec::new();
        let mut at = content;
        for _ in 0..STEPS {
            let Some(step) = state.steps.get(&at) else {
                break;
            };
            if same_as.is_some_and(|same_as| step.predicates.binary_search(&same_as).is_ok()) {
                break;
            }
            terms.extend_from_slice(&step.touched);
            at = step.parent;
            let ancestor = state
                .kept
                .iter()
                .find(|k| k.content == at && k.kind == *kind)
                .and_then(|k| k.corpus.as_ref());
            if let Some(corpus) = ancestor {
                if worth_deriving(terms.len(), corpus.documents()) {
                    terms.sort_unstable();
                    terms.dedup();
                    from = Some((Arc::clone(corpus), std::mem::take(&mut terms)));
                }
                break;
            }
            if terms.len() > STEP_TERMS {
                break;
            }
        }
        if building {
            return Found::Build { from, keep: false };
        }
        // A newer corpus of this kind: the kept ones of versions no snapshot holds go.
        state
            .kept
            .retain(|k| k.kind != *kind || k.version.strong_count() > 0);
        while state.kept.len() >= ENTRIES {
            Self::evict_one(&mut state, None);
        }
        let mut key = Reservation::new(Arc::clone(&self.budget));
        while key.charge(key_bytes(kind)).is_err() {
            if !Self::evict_one(&mut state, None) {
                Self::prune(&mut state);
                return Found::Build { from, keep: false };
            }
        }
        state.kept.push(Kept {
            content,
            kind: kind.clone(),
            version: Arc::downgrade(version),
            _key: key,
            corpus: None,
        });
        Self::prune(&mut state);
        Found::Build { from, keep: true }
    }

    /// Keeps `corpus`, built for `kind` of `version` by the search [`Self::find`] told to,
    /// if its bytes fit the budget once the least recently used others are let go; else
    /// (or if its entry went meanwhile) it serves its search alone.
    pub(crate) fn keep(
        &self,
        version: &Arc<Version>,
        kind: &CorpusKind,
        corpus: Corpus,
    ) -> Arc<Held> {
        let content = version.content;
        let mut state = self.state.lock();
        let entry = |state: &State| {
            state
                .kept
                .iter()
                .position(|k| k.content == content && k.kind == *kind)
        };
        if entry(&state).is_none() {
            return Held::transient(corpus);
        }
        let mut charge = Reservation::new(Arc::clone(&self.budget));
        while charge.charge(held_bytes(&corpus)).is_err() {
            if !Self::evict_one(&mut state, Some((content, kind))) {
                // Larger than the budget: not kept, nor its entry.
                if let Some(at) = entry(&state) {
                    state.kept.remove(at);
                }
                Self::prune(&mut state);
                return Held::transient(corpus);
            }
        }
        let held = Arc::new(Held {
            corpus,
            _charge: Some(charge),
        });
        let at = entry(&state).expect("not evicted: kept above");
        let mut kept = state.kept.remove(at);
        kept.corpus = Some(Arc::clone(&held));
        state.kept.push(kept);
        Self::prune(&mut state);
        held
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
        TextCorpusStats {
            kept,
            bytes: self.budget.used() as u64,
            budget: self.budget.limit() as u64,
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
            let found = searching.text_search(&query).len();
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
