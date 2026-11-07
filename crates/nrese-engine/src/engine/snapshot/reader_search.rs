//! Reader-scoped searches (`SearchScope::Reader`): full-text, IRI and vector searches over
//! the documents this snapshot's own scans show in a scope's graphs, its corpus
//! ([`super::super::corpora`]), so that nothing outside them changes a score, a rank or a
//! selection. The global searches ([`Snapshot::text_search`], [`Snapshot::vector_search`],
//! [`Snapshot::iri_search`]) are beside these, unchanged: the caller chooses which.

use std::sync::Arc;

use super::Snapshot;
use crate::engine::ReadModel;
use crate::engine::corpora::{CorpusRef, CorpusStore, Found, KindRef, TextCorpusStats};
use crate::engine::equality::Classes as EqualityClasses;
use crate::quad::{AccessPlan, GraphSelector, Permutation, QuadPattern};
use crate::term::text::{Bits, Corpus};
use crate::term::{TermId, TermKind};

/// The graphs whose statements make the corpus of a reader-scoped search
/// ([`Snapshot::text_search_in`]): the literals that are their objects (the IRIs that are
/// their subjects, for [`Snapshot::iri_search_in`]; the vectors that are their objects, for
/// [`Snapshot::vector_search_in`]).
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

/// Words of the bits a slice of a corpus walk covers at most ([`Snapshot::mark_values`]):
/// its group counts (16 bytes a term) are what the slice holds besides the bits.
const SLICE_WORDS: usize = 1024;

/// Words of the bits each slice of a walk over `words` covers: four slices a thread of the
/// pool, at most [`SLICE_WORDS`].
fn slice_words(words: usize) -> usize {
    words
        .div_ceil(rayon::current_num_threads() * 4)
        .clamp(1, SLICE_WORDS)
}

/// The request's budget, or an unlimited one where the caller brought none.
fn request_budget(budget: Option<&Arc<nrese_exec::Budget>>) -> Arc<nrese_exec::Budget> {
    match budget {
        Some(budget) => Arc::clone(budget),
        None => Arc::new(nrese_exec::Budget::unlimited()),
    }
}

impl Snapshot {
    /// The engine's store of corpora, made at the first reader-scoped search; `None` for
    /// statistics without an engine's cell (a search then keeps nothing).
    fn corpora(&self) -> Option<&CorpusStore> {
        self.statistics.corpora.get_or_init()
    }

    /// The string literals that are objects of statements this snapshot shows in
    /// `scope`'s graphs in `model` (as its own scans do, equality classes expanded or
    /// canonical as it reads them) and match `query`, best first, ranked among those
    /// literals alone: no literal outside them (another version's, an open transaction's,
    /// a graph's outside the scope, one no visible statement has) changes a score. What
    /// building the corpus takes is charged to `budget` while the search holds it (the
    /// request's; `None`: unbounded); a kept corpus is the engine's. A budget that has no
    /// room returns its refusal, as does a read the reader's budget refused
    /// ([`Self::read_refused`]): what it read may be incomplete.
    pub fn text_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::TextQuery,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<Vec<crate::TextMatch>, nrese_exec::BudgetExceeded> {
        let request = request_budget(budget);
        let corpus = self.text_corpus(Documents::Literals, model, scope, &request)?;
        let known = self.version.dictionary_len;
        let mut matches = self.dictionary.text_search_in(query, &corpus);
        matches.retain(|m| TermId::from_raw(m.id).payload() < known);
        Ok(matches)
    }

    /// The IRIs that are subjects of statements this snapshot shows in `scope`'s graphs
    /// in `model` and whose local names match `query`, best first, ranked among those IRIs
    /// alone (as [`Self::text_search_in`] ranks literals, `budget` as there). Read
    /// canonically, a class is its representative, named by every identity: each
    /// identity's local name is a document while the representative is a subject, and a
    /// match is answered as the representative, each once, at its best identity's
    /// relevance.
    pub fn iri_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::TextQuery,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<Vec<crate::TextMatch>, nrese_exec::BudgetExceeded> {
        let request = request_budget(budget);
        let corpus = self.text_corpus(Documents::Subjects, model, scope, &request)?;
        let known = self.version.dictionary_len;
        let mut found = self.dictionary.iri_search_in(query, &corpus);
        found.retain(|m| TermId::from_raw(m.id).payload() < known);
        if let Some((classes, true)) = self.corpus_classes(Documents::Subjects, model) {
            // Best first: the first of each class is its best identity's.
            let mut seen = std::collections::HashSet::new();
            found.retain_mut(|m| {
                m.id = classes.representative(TermId::from_raw(m.id)).raw();
                seen.insert(m.id)
            });
        }
        Ok(found)
    }

    /// The vector literals that are objects of statements this snapshot shows in `scope`'s
    /// graphs in `model` (its corpus of vectors), among those `accept` takes, nearest to
    /// `query.vector` first (ties by id), with how the search went. Every one of them is
    /// compared (exact, whatever `query.strategy` asks): no vector outside them (another
    /// version's, an open transaction's, a deleted one, one in a graph outside the scope)
    /// changes the answer or the report. Building the corpus is charged to `budget` as
    /// [`Self::text_search_in`]'s is.
    pub fn vector_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::VectorQuery,
        accept: &(dyn Fn(TermId) -> bool + Sync),
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<(Vec<(TermId, f32)>, crate::VectorSearchReport), nrese_exec::BudgetExceeded> {
        // The space first: no vector of the query's dimension, nothing found and nothing
        // reported, before a corpus is admitted (attack list item 7; sol-loop r1 S3).
        let Some(input) = self.dictionary.vector_input(query) else {
            let none = crate::VectorSearchReport {
                space: 0,
                graph: false,
                scanned: 0,
            };
            return Ok((Vec::new(), none));
        };
        let request = request_budget(budget);
        let corpus = self.text_corpus(Documents::Vectors, model, scope, &request)?;
        // The corpus decides what the search is over and reports; `accept` narrows its hits.
        let known = self.version.dictionary_len;
        let eligible = |raw: u64| {
            let id = TermId::from_raw(raw);
            id.payload() < known && corpus.contains(id.payload())
        };
        let accept = |raw: u64| accept(TermId::from_raw(raw));
        let (hits, report) =
            crate::term::vectors::search_eligible(Some(input), query, &eligible, &accept);
        let hits = hits
            .into_iter()
            .map(|hit| (TermId::from_raw(hit.id), hit.distance))
            .collect();
        Ok((hits, report))
    }

    /// The corpora this snapshot's engine keeps, and how many were built by a walk or
    /// derived from an ancestor's ([`super::super::corpora`]); all zero, `started` false,
    /// where no reader-scoped search ran on the engine.
    pub fn text_corpus_stats(&self) -> TextCorpusStats {
        self.statistics
            .corpora
            .get()
            .map(CorpusStore::stats)
            .unwrap_or_default()
    }

    /// The members of the corpus of [`Self::text_search_in`] for `model` and `scope` (with
    /// `iris`, of [`Self::iri_search_in`]'s), dictionary entries ascending, with their
    /// number and words, as a search finds them (kept, derived or walked): for checks
    /// against the statements this snapshot's scans show.
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
            .text_corpus(documents, model, scope, &request_budget(None))
            .expect("no budget to exceed");
        (corpus.members(), corpus.documents(), corpus.total_words())
    }

    /// The members of [`Self::vector_search_in`]'s corpus for `model` and `scope`: the
    /// typed literals that are objects of statements this snapshot shows there (the scan
    /// reads the vectors among them).
    #[doc(hidden)]
    pub fn vector_corpus_members(&self, model: ReadModel, scope: &TextScope) -> Vec<u64> {
        self.text_corpus(Documents::Vectors, model, scope, &request_budget(None))
            .expect("no budget to exceed")
            .members()
    }

    /// Whether this snapshot's reads in `model` expand equality classes that hold literals
    /// (or typed literals): then a literal's membership can change with a statement about an
    /// IRI of its class.
    fn classes_relate_literals(&self, model: ReadModel) -> bool {
        if model == ReadModel::Asserted {
            return false;
        }
        self.equality.is_some_and(|same_as| {
            self.version
                .equality
                .classes(&self.version, same_as)
                .relate_literals()
        })
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

    /// The corpus of `documents` this snapshot shows in `scope` in `model`, with its
    /// statistics from the index (which covers them: the index is extended first); exact,
    /// kept by the statements' identity, model, graphs and how this snapshot reads
    /// equality. Membership is what this snapshot's own scans show (equality classes
    /// expanded or canonical, the visibility of their copies), never the statements as
    /// stored. Derived from the nearest kept ancestor's where the lineage has one: each
    /// term the steps since touched is probed, a member while a visible statement in the
    /// scope has it (an IRI of a class: every identity of it); else walked
    /// ([`Self::walk_corpus`]), and after a change of a `sameAs` statement (the classes
    /// change). The probes and the walk hold no lock of the dictionary (the walk runs on
    /// every core); only the counting does, on this thread. Nobody waits for another's
    /// build: a search that finds one under way builds its own and doesn't keep it. What
    /// the build takes, and a corpus not kept, is charged to `request`.
    fn text_corpus(
        &self,
        documents: Documents,
        model: ReadModel,
        scope: &TextScope,
        request: &Arc<nrese_exec::Budget>,
    ) -> Result<CorpusRef, nrese_exec::BudgetExceeded> {
        // Copied only where it isn't written the one way yet, charged then; the charge
        // bound first, as bindings drop in reverse: the copy goes before its charge.
        let (_scope_charge, scope) = scope.normalised(request)?;
        let scope = &*scope;
        // What the build reads (equality expansion's copies) is charged to the search's budget
        // where the snapshot brought no reader's budget of its own (a store's autocompletion
        // reads a plain snapshot): a refused read stops it, as a reader's would.
        let budgeted;
        let reader = match self.reads {
            Some(_) => self,
            None => {
                budgeted = self.reading_within(Arc::clone(request))?;
                &budgeted
            }
        };
        let store = self.corpora();
        let kind = KindRef {
            documents,
            model,
            scope,
            equality: self.equality,
            canonical: self.canonical,
        };
        let found = match store {
            Some(store) => store.find(&self.version, kind, request)?,
            None => Found::Build {
                from: None,
                keep: false,
            },
        };
        let (from, keep) = match found {
            Found::Kept(corpus) => {
                self.cover(documents);
                return Ok(CorpusRef::Kept(corpus));
            }
            Found::Build { from, keep } => (from, keep),
        };
        let span = self.version.dictionary_len;
        let built = match reader.build_corpus(documents, model, scope, from, store, request) {
            Ok(built) => built,
            Err(refused) => {
                // The entry `find` made for it is let go: nothing will fill it.
                if let Some(store) = store.filter(|_| keep) {
                    store.abandon(&self.version, kind);
                }
                return Err(refused);
            }
        };
        // Kept: built on the store's budget. Else the search's, on its own.
        let (_, bytes) = Corpus::finished_bytes(built.documents, span);
        let kept = store.filter(|_| keep).and_then(|store| {
            store.keep_built(&self.version, kind, bytes, |budget| {
                Corpus::finish(&built, span, budget)
            })
        });
        Ok(match kept {
            Some(corpus) => CorpusRef::Kept(corpus),
            None => CorpusRef::Own(Corpus::finish(&built, span, request)?),
        })
    }

    /// The members of the corpus of `documents` in `scope` in `model`: derived from `from`
    /// (an ancestor's corpus and the terms touched since) or walked. Refused where `request`
    /// has no room, or where a read the reader's budget refused stopped early (what it
    /// built may miss members).
    fn build_corpus(
        &self,
        documents: Documents,
        model: ReadModel,
        scope: &TextScope,
        from: Option<(Arc<Corpus>, nrese_exec::ChargedVec<u64>)>,
        store: Option<&CorpusStore>,
        request: &Arc<nrese_exec::Budget>,
    ) -> Result<Bits, nrese_exec::BudgetExceeded> {
        let built_by = |derived: bool| {
            if let Some(store) = store {
                store.built(derived);
            }
        };
        let iris = documents == Documents::Subjects;
        let vectors = documents == Documents::Vectors;
        // A touched IRI of a class stands for every identity of it (literals belong to no
        // class): each is probed.
        let classes = self.corpus_classes(documents, model);
        let span = self.version.dictionary_len;
        // Where the reader's equality classes hold literals (`r owl:sameAs "alpha"`), a
        // statement about an IRI can show a literal its commit never touched: those corpora
        // are walked, never derived (sol-loop r1 S1).
        let from = from.filter(|_| iris || !self.classes_relate_literals(model));
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
                built_by(true);
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
                built_by(false);
                self.walk_corpus(documents, model, scope, request)?
            }
        };
        // A read the reader's budget refused stopped early: what it built may miss members,
        // so it is neither kept nor used.
        if let Some(refused) = self.read_refused() {
            return Err(refused);
        }
        Ok(built)
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
    ) -> Result<Bits, nrese_exec::BudgetExceeded> {
        let span = self.version.dictionary_len;
        let words = span.div_ceil(64) as usize;
        let mut bits = nrese_exec::ChargedVec::with_capacity(Arc::clone(budget), words)?;
        bits.resize(words, 0)?;
        let mut scratch = nrese_exec::Reservation::new(Arc::clone(budget));
        let (running, bytes) = self.walk_scratch(words, budget.remaining());
        scratch.charge(bytes)?;
        // Per graph, each kind's walk; a scan (where the index can't walk them) marks every
        // kind at once. Only the count needs the index: on a pool thread it is covered
        // while the statements are walked; elsewhere first.
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
            Documents::Vectors => Bits::counted(bits, |_| 1),
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
            Documents::Literals => self.dictionary.prepare_text(false, None),
            Documents::Subjects => self.dictionary.prepare_text(true, None),
            // The vector search covers its index itself, under its own lock.
            Documents::Vectors => {}
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
    /// every term is held. Where the index can't walk them (a reader of equality classes),
    /// one scan of the statements marks the terms of every kind of `documents`: then `true`.
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
                if let Some(word) = bits.get_mut((at / 64) as usize) {
                    *word |= 1 << (at % 64);
                }
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
        // As read (canonically: by representatives); `None` where reads expand classes.
        let plan = self.range_plan(model, &pattern, permutation, low, high)?;
        self.group_counts_of(model, &plan)
    }

    /// [`Self::group_counts_in`] for a plan of a range: per distinct value of its first
    /// unbound component, the matches in `model`, in id order. `None` where a stack can't
    /// answer it.
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
        let supported = crate::engine::Stack::ALL.into_iter().all(|stack| {
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
        let mut counts = Vec::new();
        let mut sources = 0;
        for stack in crate::engine::Stack::ALL {
            if model.includes(stack) {
                let before = counts.len();
                self.data(model)
                    .stack(stack)
                    .group_counts(plan, bound, &mut counts);
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
}
