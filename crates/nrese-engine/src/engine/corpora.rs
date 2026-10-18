//! The corpora of full-text searches ([`Snapshot::text_search_in`](super::Snapshot::text_search_in)),
//! kept, and derived from one another across commits.
//!
//! A corpus is exact for its statements' identity ([`Content`]), the index's documents, the
//! read model and the graphs (the scope, normalised so that equal graph sets share one).
//! Building one walks the scope's index ranges; a version that a commit made from another
//! (or a pending state, or a reader's view made from the reader's earlier one) instead
//! takes the corpus of its nearest kept ancestor and looks again at the terms the steps
//! between them touched: each is a member if a statement in the scope still uses it, an
//! index probe. So a search after a small commit costs probes for what the commit touched,
//! not a walk, and deletes stay exact (a literal stays while any statement uses it).
//!
//! The lineage (content → its parent and the terms touched) is kept only while a corpus is
//! kept, back to the oldest kept one, for at most [`STEPS`] steps and [`STEP_TERMS`] terms:
//! a store nobody searches keeps none. Beyond, or where more terms were touched than a
//! probe each is worth ([`worth_deriving`]), the corpus is walked again.
//!
//! Kept corpora are bounded: at most [`ENTRIES`] and the engine's byte budget
//! ([`crate::EngineConfig::text_corpus_bytes`]), least recently used first out; a corpus of a
//! version no snapshot holds goes once a newer one of its kind is kept. What they hold is
//! in [`crate::EngineStats::text_corpora`].

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

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

/// What a corpus is of, without the version: the documents, the model and the graphs.
pub(crate) type CorpusKind = (Documents, ReadModel, TextScope);

/// Counts of the corpora (for [`crate::EngineStats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextCorpusStats {
    /// Corpora kept, and their bytes.
    pub kept: u64,
    pub bytes: u64,
    /// The byte budget they are kept within.
    pub budget: u64,
    /// Corpora built by a walk, and derived from an ancestor's, since the engine started.
    pub walked: u64,
    pub derived: u64,
}

/// A kept corpus: of which statements, of what, and the corpus once its first search
/// built it (`None` meanwhile: other searches build their own rather than wait, since the
/// build runs on every core and a waiting thread may be one of its parts').
struct Kept {
    content: Content,
    kind: CorpusKind,
    /// The latest version seen with this content (compaction gives the same content new
    /// runs): while it lives, a snapshot may still ask.
    version: Weak<Version>,
    corpus: Option<Arc<Corpus>>,
}

/// One step of the lineage: the content it was made from and the terms it touched
/// (string literals as objects, IRIs as subjects; raw ids, sorted).
struct Step {
    parent: Content,
    touched: Arc<[u64]>,
}

#[derive(Default)]
struct State {
    /// Least recently used first.
    kept: Vec<Kept>,
    steps: HashMap<Content, Step>,
    order: VecDeque<Content>,
    step_terms: usize,
}

/// The kept corpora and the lineage, shared by the snapshots of an engine and the views
/// made from them.
pub(crate) struct CorpusStore {
    budget: u64,
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
/// where another search is building it).
pub(crate) enum Found {
    Kept(Arc<Corpus>),
    Build {
        from: Option<(Arc<Corpus>, Vec<u64>)>,
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
        Self {
            budget: budget as u64,
            state: Mutex::default(),
            walked: AtomicU64::new(0),
            derived: AtomicU64::new(0),
        }
    }

    /// Records that `child` was made from `parent`, adding or removing statements with
    /// these subjects and objects.
    pub(crate) fn record(
        &self,
        parent: Content,
        child: Content,
        statements: impl IntoIterator<Item = (TermId, TermId)>,
    ) {
        if self.state.lock().kept.is_empty() {
            return;
        }
        let mut touched: Vec<u64> = statements
            .into_iter()
            .flat_map(|(subject, object)| terms(subject, object))
            .collect();
        touched.sort_unstable();
        touched.dedup();
        self.record_terms(parent, child, touched);
    }

    /// Records that the view `child` of the version `base` was made from the view
    /// `earlier` of `earlier_base` (a reader's view at a later commit), touching what the
    /// steps from `earlier_base` to `base` touched and the terms of `changed` (the
    /// statements the view shows or hides now and didn't before). Nothing where the
    /// lineage lacks those steps.
    pub(crate) fn record_view<'q>(
        &self,
        earlier: Content,
        earlier_base: Content,
        child: Content,
        base: Content,
        changed: impl IntoIterator<Item = &'q EncodedQuad>,
    ) {
        let Some(mut touched) = self.touched_between(earlier_base, base) else {
            return;
        };
        touched.extend(
            changed
                .into_iter()
                .flat_map(|quad| terms(quad.subject, quad.object)),
        );
        touched.sort_unstable();
        touched.dedup();
        self.record_terms(earlier, child, touched);
    }

    /// Records that `child` was made from `parent` touching `touched` (raw ids, sorted).
    fn record_terms(&self, parent: Content, child: Content, touched: Vec<u64>) {
        if parent == child {
            return;
        }
        let mut state = self.state.lock();
        // Steps lead from a version to a kept corpus: with none kept, none are.
        if state.kept.is_empty() {
            state.steps.clear();
            state.order.clear();
            state.step_terms = 0;
            return;
        }
        state.step_terms += touched.len();
        state.steps.insert(
            child,
            Step {
                parent,
                touched: touched.into(),
            },
        );
        state.order.push_back(child);
        Self::prune(&mut state);
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
            state.order.pop_front();
            if let Some(step) = state.steps.remove(&oldest) {
                state.step_terms -= step.touched.len();
            }
        }
    }

    /// The terms the steps from `ancestor` to `content` touched, if the lineage holds
    /// them all.
    fn touched_between(&self, ancestor: Content, content: Content) -> Option<Vec<u64>> {
        let state = self.state.lock();
        let mut terms = Vec::new();
        let mut at = content;
        for _ in 0..STEPS {
            if at == ancestor {
                terms.sort_unstable();
                terms.dedup();
                return Some(terms);
            }
            let step = state.steps.get(&at)?;
            terms.extend_from_slice(&step.touched);
            at = step.parent;
        }
        None
    }

    /// The corpus `kind` of `version`: kept, or to build, with the nearest kept ancestor's
    /// corpus and the terms touched since where the lineage has one.
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
        let mut from = None;
        let mut terms: Vec<u64> = Vec::new();
        let mut at = content;
        for _ in 0..STEPS {
            let Some(step) = state.steps.get(&at) else {
                break;
            };
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
        state.kept.push(Kept {
            content,
            kind: kind.clone(),
            version: Arc::downgrade(version),
            corpus: None,
        });
        self.evict(&mut state);
        Found::Build { from, keep: true }
    }

    /// Keeps `corpus`, built for `kind` of `version` by the search [`Self::find`] told to,
    /// and keeps within the bounds.
    pub(crate) fn keep(&self, version: &Arc<Version>, kind: &CorpusKind, corpus: Arc<Corpus>) {
        let mut state = self.state.lock();
        let content = version.content;
        match state
            .kept
            .iter_mut()
            .find(|k| k.content == content && k.kind == *kind)
        {
            Some(kept) => kept.corpus = Some(corpus),
            // Evicted while it was built: kept again, as the most recently used.
            None => state.kept.push(Kept {
                content,
                kind: kind.clone(),
                version: Arc::downgrade(version),
                corpus: Some(corpus),
            }),
        }
        self.evict(&mut state);
    }

    /// Counts a corpus built, by a walk or derived.
    pub(crate) fn built(&self, derived: bool) {
        match derived {
            true => self.derived.fetch_add(1, Ordering::Relaxed),
            false => self.walked.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// Least recently used first out, until at most [`ENTRIES`] are kept and their bytes
    /// fit the budget; the most recent stays, whatever its size (a search needs it).
    fn evict(&self, state: &mut State) {
        let bytes = |kept: &[Kept]| -> u64 {
            kept.iter()
                .filter_map(|k| k.corpus.as_ref())
                .map(|corpus| corpus.memory_bytes() as u64)
                .sum()
        };
        while state.kept.len() > 1
            && (state.kept.len() > ENTRIES || bytes(&state.kept) > self.budget)
        {
            state.kept.remove(0);
        }
        Self::prune(state);
    }

    pub(crate) fn stats(&self) -> TextCorpusStats {
        let state = self.state.lock();
        TextCorpusStats {
            kept: state.kept.len() as u64,
            bytes: state
                .kept
                .iter()
                .filter_map(|k| k.corpus.as_ref())
                .map(|corpus| corpus.memory_bytes() as u64)
                .sum(),
            budget: self.budget,
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
                )
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
