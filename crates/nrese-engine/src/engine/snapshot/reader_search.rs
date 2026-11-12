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
use crate::term::text::{Bits, Corpus, word_length};
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

/// The words an own term (an overlay entry: `id`, `key`) has as a document of `documents`,
/// as the shared index counts them: a literal's value counted in place, an IRI's local-name
/// text ([`crate::term::text::local_name_text`]) made on `scratch`'s charge and let go; 0
/// for a term of another kind, and for vectors (whose corpus counts each member once).
fn own_length(
    documents: Documents,
    id: TermId,
    key: &[u8],
    scratch: &mut nrese_exec::Reservation,
) -> Result<u16, nrese_exec::BudgetExceeded> {
    use crate::term::TermView;
    if !documents.holds(id.kind()) {
        return Ok(0);
    }
    Ok(match (documents, crate::term::dictionary::view_key(key)) {
        (Documents::Literals, TermView::String(value) | TermView::LangString { value, .. }) => {
            word_length(value)
        }
        (Documents::Subjects, TermView::Iri(iri)) => {
            let bytes = crate::term::text::local_name_text_bytes(iri.len());
            scratch.charge(bytes)?;
            let length = crate::term::text::local_name_text(iri).map_or(0, |t| word_length(&t));
            scratch.release(bytes);
            length
        }
        _ => 0,
    })
}

/// What an unpublished version's readers built, kept in that version's own slot (3.0c's
/// `OwnState`; the 3.2 review's R32-N2: every search of one retained pending snapshot
/// derived again): its corpora by full kind and snapshot identity, and a private snapshot's
/// own-document indexes by their corpus and stemming language. Views of one version share
/// the slot (equality modes, stored, canonical, reading budgets), so the key holds the full
/// identity: two of them never share an entry they shouldn't. Each value keeps the charge of
/// the request that built it until the version's last holder drops it; each entry (its node,
/// its value's shared allocation, its key's copies) is charged there too, before it is made
/// ([`entry_bytes`]; sol-loop r3 B5). The locks are leaves, held only to look up or to link
/// an entry in, never while a corpus is built; a losing entry drops after the lock. The
/// first value kept for a key wins.
#[derive(Default)]
pub(crate) struct OwnCorpora {
    corpora: parking_lot::Mutex<Links<OwnCorpus>>,
    locals: parking_lot::Mutex<Links<OwnLocal>>,
}

/// Entries, newest first, each in an allocation of exactly its node: no backing vector whose
/// growth would hold more than was charged.
struct Links<T>(Option<Box<Link<T>>>);

struct Link<T> {
    entry: T,
    next: Option<Box<Link<T>>>,
}

impl<T> Default for Links<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> Links<T> {
    fn iter(&self) -> impl Iterator<Item = &T> {
        std::iter::successors(self.0.as_deref(), |link| link.next.as_deref())
            .map(|link| &link.entry)
    }

    fn push(&mut self, mut link: Box<Link<T>>) {
        link.next = self.0.take();
        self.0 = Some(link);
    }
}

impl<T> Drop for Links<T> {
    /// One node at a time: no recursion as deep as the list.
    fn drop(&mut self) {
        let mut next = self.0.take();
        while let Some(mut link) = next {
            next = link.next.take();
        }
    }
}

/// The bytes an entry of `T` holding a value `V` allocates: its node, and its value's shared
/// allocation (an `Arc`'s two counts and the value).
const fn entry_bytes<T, V>() -> usize {
    size_of::<Link<T>>() + 2 * size_of::<usize>() + size_of::<V>()
}

struct OwnCorpus {
    kind: crate::engine::corpora::CorpusKind,
    identity: super::SnapshotIdentity,
    corpus: Arc<Corpus>,
    _key: nrese_exec::Reservation,
}

struct OwnLocal {
    corpus: Arc<Corpus>,
    language: Option<String>,
    local: Arc<crate::term::text::Local>,
    _key: nrese_exec::Reservation,
}

impl OwnCorpora {
    /// The corpus kept for `kind` read as `identity`.
    fn corpus(&self, kind: KindRef<'_>, identity: super::SnapshotIdentity) -> Option<Arc<Corpus>> {
        let corpora = self.corpora.lock();
        corpora
            .iter()
            .find(|own| own.identity == identity && kind.matches(&own.kind))
            .map(|own| Arc::clone(&own.corpus))
    }

    /// Keeps `corpus` for `kind` read as `identity`, its entry charged to `request` first
    /// (its node and shared allocation, the scope's graphs); the one kept first for that key
    /// where another search kept one meanwhile, this one dropped after the lock.
    fn keep(
        &self,
        kind: KindRef<'_>,
        identity: super::SnapshotIdentity,
        corpus: Corpus,
        request: &Arc<nrese_exec::Budget>,
    ) -> Result<Arc<Corpus>, nrese_exec::BudgetExceeded> {
        let mut key = nrese_exec::Reservation::new(Arc::clone(request));
        key.charge(entry_bytes::<OwnCorpus, Corpus>() + scope_bytes(kind.scope))?;
        let corpus = Arc::new(corpus);
        let link = Box::new(Link {
            entry: OwnCorpus {
                kind: kind.to_owned(),
                identity,
                corpus: Arc::clone(&corpus),
                _key: key,
            },
            next: None,
        });
        let mut corpora = self.corpora.lock();
        let kept = corpora
            .iter()
            .find(|own| own.identity == identity && kind.matches(&own.kind))
            .map(|own| Arc::clone(&own.corpus));
        if let Some(kept) = kept {
            drop(corpora);
            drop(link);
            return Ok(kept);
        }
        corpora.push(link);
        Ok(corpus)
    }

    /// The own-document index kept for `corpus` (by its allocation, which the entry keeps
    /// alive) stemmed for `language`.
    fn local(
        &self,
        corpus: &Arc<Corpus>,
        language: Option<&str>,
    ) -> Option<Arc<crate::term::text::Local>> {
        let locals = self.locals.lock();
        locals
            .iter()
            .find(|own| Arc::ptr_eq(&own.corpus, corpus) && own.language.as_deref() == language)
            .map(|own| Arc::clone(&own.local))
    }

    /// Keeps `local` for `corpus` stemmed for `language`, its entry charged to `request`
    /// first (its node and shared allocation, the language's copy); the one kept first
    /// where another search kept one meanwhile, this one dropped after the lock.
    fn keep_local(
        &self,
        corpus: &Arc<Corpus>,
        language: Option<&str>,
        local: crate::term::text::Local,
        request: &Arc<nrese_exec::Budget>,
    ) -> Result<Arc<crate::term::text::Local>, nrese_exec::BudgetExceeded> {
        let mut key = nrese_exec::Reservation::new(Arc::clone(request));
        key.charge(
            entry_bytes::<OwnLocal, crate::term::text::Local>() + language.map_or(0, str::len),
        )?;
        let local = Arc::new(local);
        let link = Box::new(Link {
            entry: OwnLocal {
                corpus: Arc::clone(corpus),
                language: language.map(str::to_owned),
                local: Arc::clone(&local),
                _key: key,
            },
            next: None,
        });
        let mut locals = self.locals.lock();
        let kept = locals
            .iter()
            .find(|own| Arc::ptr_eq(&own.corpus, corpus) && own.language.as_deref() == language)
            .map(|own| Arc::clone(&own.local));
        if let Some(kept) = kept {
            drop(locals);
            drop(link);
            return Ok(kept);
        }
        locals.push(link);
        Ok(local)
    }
}

/// Bytes a scope's copy holds beyond itself: its graphs where it lists them.
fn scope_bytes(scope: &TextScope) -> usize {
    match scope {
        TextScope::Merge(graphs) => graphs.len() * size_of::<TermId>(),
        TextScope::Graphs(_) => 0,
    }
}

/// The lexical form of an own term (an overlay entry: `id`, `key`) if it is a vector
/// literal among `corpus`'s members.
fn vector_lexical<'k>(id: TermId, key: &'k [u8], corpus: &Corpus) -> Option<&'k str> {
    if id.kind() != TermKind::TypedLiteral || !corpus.contains(id.payload()) {
        return None;
    }
    match crate::term::dictionary::view_key(key) {
        crate::term::TermView::Typed { value, datatype } if datatype == nrese_vector::DATATYPE => {
            Some(value)
        }
        _ => None,
    }
}

/// A private snapshot's own documents' lengths, by overlay entry from `start`
/// ([`Snapshot::own_lengths`]).
struct OwnLengths {
    start: u64,
    lengths: nrese_exec::ChargedVec<u16>,
}

impl OwnLengths {
    /// Words of `document`: the shared `index`'s below `start`, its own text's past it (0:
    /// not a document).
    #[inline]
    fn length(&self, index: &crate::term::text::TextIndex, document: u64) -> u16 {
        match document.checked_sub(self.start) {
            None => index.length_of(document),
            Some(own) => self.lengths.get(own as usize).copied().unwrap_or(0),
        }
    }
}

/// Words of the bits each slice of a walk over `words` covers ([`Snapshot::mark_values`]):
/// four slices a thread of the pool, at most `most`
/// ([`crate::CorpusTuning::walk_slice_words`]; a slice holds its group counts, 16 bytes a
/// term, besides the bits).
fn slice_words(words: usize, most: usize) -> usize {
    words
        .div_ceil(rayon::current_num_threads() * 4)
        .clamp(1, most.max(1))
}

/// The request's budget, or an unlimited one where the caller brought none.
fn request_budget(budget: Option<&Arc<nrese_exec::Budget>>) -> Arc<nrese_exec::Budget> {
    match budget {
        Some(budget) => Arc::clone(budget),
        None => Arc::new(nrese_exec::Budget::unlimited()),
    }
}

/// What the corpus store admits by (contract S3): the version of a snapshot with the
/// shared-state capability, made only from its token ([`CorpusAdmission::of`]), so that an
/// admission works on the token's own snapshot and never on one passed beside it. (Named
/// apart from 3.0c's [`super::Admission`], the capability it is made from.)
pub(crate) struct CorpusAdmission<'s> {
    version: &'s Arc<crate::engine::Version>,
}

impl<'s> CorpusAdmission<'s> {
    /// The admission of the snapshot `token` was taken from.
    fn of(token: &super::SharedState<'s>) -> Self {
        Self {
            version: &token.snapshot().version,
        }
    }

    /// The version admitted.
    pub(crate) fn version(&self) -> &'s Arc<crate::engine::Version> {
        self.version
    }

    /// A version admitted without a snapshot: the corpus store's own unit tests.
    #[cfg(test)]
    pub(crate) fn for_tests(version: &'s Arc<crate::engine::Version>) -> Self {
        Self { version }
    }
}

impl Snapshot {
    /// The engine's store of corpora, made at the first reader-scoped search of a published
    /// snapshot (`admitted`: a snapshot without the capability makes none, contract S3);
    /// `None` for statistics without an engine's cell, and where none was made yet for a
    /// search without the capability (a search then keeps nothing).
    fn corpora(&self, admitted: bool) -> Option<&CorpusStore> {
        match admitted {
            true => self.statistics.corpora.get_or_init(),
            false => self.statistics.corpora.get(),
        }
    }

    /// The most words of bits a slice of a corpus walk covers: the engine's setting, or the
    /// default where there is no store.
    fn walk_slice_words(&self) -> usize {
        self.statistics
            .corpora
            .get()
            .map_or(crate::CorpusTuning::DEFAULT, CorpusStore::tuning)
            .walk_slice_words
    }

    /// The string literals that are objects of statements this snapshot shows in
    /// `scope`'s graphs in `model` (as its own scans do, equality classes expanded or
    /// canonical as it reads them) and match `query`, best first, ranked among those
    /// literals alone: no literal outside them (another version's, an open transaction's,
    /// a graph's outside the scope, one no visible statement has) changes a score. What
    /// building the corpus takes is charged to `budget` while the search holds it (the
    /// request's; `None`: unbounded); a kept corpus is the engine's. A budget that has no
    /// room returns its refusal, as does a read the reader's budget refused
    /// ([`Self::read_refused`]): what it read may be incomplete. A private snapshot searches
    /// what it can see the same way, its own literals among them (V3; the lifecycle
    /// contract's §7), and leaves the store of corpora as it found it.
    pub fn text_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::TextQuery,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<Vec<crate::TextMatch>, super::SearchRefused> {
        let request = self.search_budget(budget);
        let corpus = self.text_corpus(Documents::Literals, model, scope, &request)?;
        let known = self.version.dictionary_len;
        let mut matches = self.search_corpus(Documents::Literals, query, &corpus, &request)?;
        matches.retain(|m| TermId::from_raw(m.id).payload() < known);
        Ok(matches)
    }

    /// The literals or IRI names (`documents`) of `corpus` matching `query`: through the
    /// shared index, and for a private snapshot through the index of its own documents too,
    /// built for this search on `request` ([`crate::term::text::Private`]).
    fn search_corpus(
        &self,
        documents: Documents,
        query: &crate::TextQuery,
        corpus: &CorpusRef,
        request: &Arc<nrese_exec::Budget>,
    ) -> Result<Vec<crate::TextMatch>, nrese_exec::BudgetExceeded> {
        let iris = documents == Documents::Subjects;
        let Some(view) = self.own_view() else {
            return Ok(match iris {
                false => self.underlying().text_search_in(query, Some(corpus)),
                true => self.underlying().iri_search_in(query, Some(corpus)),
            });
        };
        let language = query.stem.as_deref();
        // A corpus kept in the version's own slot keeps its own-document index beside it
        // (R32-N2); one built for this search alone builds its index for it alone.
        let (kept_local, fresh);
        let own: &crate::term::text::Local = match (corpus, self.own_corpora()) {
            (CorpusRef::Kept(kept), Some(slot)) => {
                kept_local = match slot.local(kept, language) {
                    Some(local) => local,
                    None => {
                        let local =
                            self.own_documents(view, documents, corpus, language, request)?;
                        slot.keep_local(kept, language, local, request)?
                    }
                };
                &kept_local
            }
            _ => {
                fresh = self.own_documents(view, documents, corpus, language, request)?;
                &fresh
            }
        };
        Ok(self
            .underlying()
            .private_search(iris, query, corpus, view.start(), own))
    }

    /// The IRIs that are subjects of statements this snapshot shows in `scope`'s graphs
    /// in `model` and whose local names match `query`, best first, ranked among those IRIs
    /// alone (as [`Self::text_search_in`] ranks literals, `budget` as there). Read
    /// canonically, a class is its representative, named by every identity: each
    /// identity's local name is a document while the representative is a subject, and a
    /// match is answered as the representative, each once, at its best identity's
    /// relevance. A private snapshot searches its own IRIs too, as [`Self::text_search_in`].
    pub fn iri_search_in(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::TextQuery,
        budget: Option<&Arc<nrese_exec::Budget>>,
    ) -> Result<Vec<crate::TextMatch>, super::SearchRefused> {
        let request = self.search_budget(budget);
        let corpus = self.text_corpus(Documents::Subjects, model, scope, &request)?;
        let known = self.version.dictionary_len;
        let mut found = self.search_corpus(Documents::Subjects, query, &corpus, &request)?;
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
    /// changes the answer or the report. Building the corpus, the scan and the hits are
    /// charged to `budget` before they are allocated, as [`Snapshot::vector_search`]'s are.
    /// A private snapshot searches its own vectors too ([`Self::private_vector_search`]).
    pub fn vector_search_in<'b>(
        &self,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::VectorQuery,
        accept: &(dyn Fn(TermId) -> bool + Sync),
        budget: &'b Arc<nrese_exec::Budget>,
    ) -> Result<super::VectorHits<'b>, super::SearchRefused> {
        if let Some(view) = self.own_view() {
            return self.private_vector_search(view, model, scope, query, accept, budget);
        }
        // The space first: no vector of the query's dimension, nothing found and nothing
        // reported, before a corpus is admitted (attack list item 7; sol-loop r1 S3).
        let Some(input) = self.underlying().vector_input(query) else {
            let none = crate::VectorSearchReport {
                space: 0,
                graph: false,
                scanned: 0,
            };
            return Ok((nrese_exec::ChargedVec::new(&**budget), none));
        };
        let corpus = self.text_corpus(Documents::Vectors, model, scope, budget)?;
        // The corpus decides what the search is over and reports; `accept` narrows its hits.
        let known = self.version.dictionary_len;
        let eligible = |raw: u64| {
            let id = TermId::from_raw(raw);
            id.payload() < known && corpus.contains(id.payload())
        };
        let accept = |raw: u64| accept(TermId::from_raw(raw));
        let (hits, report) =
            crate::term::vectors::search_eligible(Some(input), query, &eligible, &accept, budget)?;
        // As the global search hands them over: the ids typed, the raw hits freed first.
        let len = hits.len();
        let (hits, held) = hits.into_parts();
        let found = nrese_exec::ChargedVec::collect(
            &**budget,
            len,
            hits.into_iter()
                .map(|hit| (TermId::from_raw(hit.id), hit.distance)),
        );
        drop(held);
        Ok((found?, report))
    }

    /// [`Self::vector_search_in`] of a private snapshot (V3; the lifecycle contract's §7):
    /// the shared vectors of its corpus below the overlay's `start`, compared exactly as a
    /// reader's are ([`crate::term::vectors::search_eligible`]), and its own (the overlay's
    /// vector literals of the query's dimension in its corpus) by an exact scan of their own,
    /// each `eligible` before `accept`; the two lists merged by distance, then id
    /// ([`nrese_vector::merge`]), the shared hits offered first: where `k` cuts a group of
    /// equal distances, the shared ones of it stay (`merge` keeps the first it is offered).
    /// The report's space and scan are exactly both sides' eligible vectors, which a caller
    /// widens `k` by. Own vectors never enter the shared index or its graph.
    fn private_vector_search<'b>(
        &self,
        view: &crate::term::overlay::OverlayView,
        model: ReadModel,
        scope: &TextScope,
        query: &crate::VectorQuery,
        accept: &(dyn Fn(TermId) -> bool + Sync),
        budget: &'b Arc<nrese_exec::Budget>,
    ) -> Result<super::VectorHits<'b>, super::SearchRefused> {
        let input = self.underlying().vector_input(query);
        let corpus = self.text_corpus(Documents::Vectors, model, scope, budget)?;
        let start = view.start();
        // The own vectors of the dimension among the corpus's members (sol-loop r1 B2): each
        // parse charged first (`parse_bytes`) and let go; counted, their storage charged as
        // nrese-vector counts it (`bytes_for`, whole chunks past the first), then filled.
        let dimension = query.vector.len();
        let mut scratch = nrese_exec::Reservation::new(Arc::clone(budget));
        let mut count = 0;
        for (id, key) in view.own_entries() {
            let Some(value) = vector_lexical(id, key, &corpus) else {
                continue;
            };
            let bytes = nrese_vector::parse_bytes(value.len());
            scratch.charge(bytes)?;
            let vector = nrese_vector::parse(value).ok();
            count += usize::from(vector.is_some_and(|vector| vector.len() == dimension));
            scratch.release(bytes);
        }
        let chunks = count.div_ceil(nrese_vector::CHUNK);
        let room = match chunks {
            0 | 1 => count,
            _ => chunks * nrese_vector::CHUNK,
        };
        // The chunks' shared allocations and the list of them, beside their values.
        let mut charge = nrese_exec::Reservation::new(Arc::clone(budget));
        charge.charge(nrese_vector::Vectors::bytes_for(dimension, room) + 128 * chunks + 64)?;
        let mut own = nrese_vector::Vectors::with_capacity(dimension, count);
        for (id, key) in view.own_entries() {
            let Some(value) = vector_lexical(id, key, &corpus) else {
                continue;
            };
            let bytes = nrese_vector::parse_bytes(value.len());
            scratch.charge(bytes)?;
            match nrese_vector::parse(value) {
                Ok(vector) if vector.len() == dimension => {
                    own.push(id.raw(), &vector);
                }
                _ => {}
            }
            scratch.release(bytes);
        }
        if input.is_none() && own.is_empty() {
            let none = crate::VectorSearchReport {
                space: 0,
                graph: false,
                scanned: 0,
            };
            return Ok((nrese_exec::ChargedVec::new(&**budget), none));
        }
        let shared = |raw: u64| {
            let id = TermId::from_raw(raw);
            id.payload() < start && corpus.contains(id.payload())
        };
        let accept = |raw: u64| accept(TermId::from_raw(raw));
        let (shared_hits, shared_report) =
            crate::term::vectors::search_eligible(input, query, &shared, &accept, budget)?;
        let own_hits =
            nrese_vector::exact(&own, &query.vector, query.k, query.metric, &accept, budget)?;
        let hits = nrese_vector::merge(
            shared_hits.iter().chain(own_hits.iter()).copied(),
            query.k,
            budget,
        )?;
        drop((shared_hits, own_hits));
        let report = crate::VectorSearchReport {
            space: shared_report.space + own.len(),
            graph: false,
            scanned: shared_report.scanned + own.len(),
        };
        let len = hits.len();
        let (hits, held) = hits.into_parts();
        let found = nrese_exec::ChargedVec::collect(
            &**budget,
            len,
            hits.into_iter()
                .map(|hit| (TermId::from_raw(hit.id), hit.distance)),
        );
        drop(held);
        drop(charge);
        Ok((found?, report))
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
    /// against the statements this snapshot's scans show (a private snapshot's own ones
    /// among them, past its overlay's start).
    #[doc(hidden)]
    pub fn text_corpus_members(
        &self,
        model: ReadModel,
        scope: &TextScope,
        iris: bool,
    ) -> Result<(Vec<u64>, u64, u64), super::SearchRefused> {
        let documents = match iris {
            false => Documents::Literals,
            true => Documents::Subjects,
        };
        let corpus = self
            .text_corpus(documents, model, scope, &request_budget(None))
            .expect("no budget to exceed");
        Ok((corpus.members(), corpus.documents(), corpus.total_words()))
    }

    /// The members of [`Self::vector_search_in`]'s corpus for `model` and `scope`: the
    /// typed literals that are objects of statements this snapshot shows there (the scan
    /// reads the vectors among them), a private snapshot's own ones among them.
    #[doc(hidden)]
    pub fn vector_corpus_members(
        &self,
        model: ReadModel,
        scope: &TextScope,
    ) -> Result<Vec<u64>, super::SearchRefused> {
        Ok(self
            .text_corpus(Documents::Vectors, model, scope, &request_budget(None))
            .expect("no budget to exceed")
            .members())
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
        // Only a published snapshot, or a restriction of one, admits into the store; any
        // other derives what it can from a kept corpus, reading it, and keeps nothing.
        let token = self.shared_state();
        let admitted = token.as_ref().map(CorpusAdmission::of);
        let store = self.corpora(admitted.is_some());
        let kind = KindRef {
            documents,
            model,
            scope,
            equality: self.equality,
            canonical: self.canonical,
        };
        // An unpublished snapshot keeps what it builds in its own version, for that
        // version's readers (3.0c's slot; R32-N2): found again by the kind and its identity.
        let own = self.own_corpora();
        let identity = self.identity();
        if let Some(corpus) = own.as_ref().and_then(|own| own.corpus(kind, identity)) {
            self.cover(documents);
            return Ok(CorpusRef::Kept(corpus));
        }
        let found = match (store, &admitted) {
            (Some(store), Some(admitted)) => store.find(admitted, kind, request)?,
            (Some(store), None) => Found::Build {
                from: store.derive_unadmitted(&self.version, kind, self.is_private(), request),
                keep: false,
            },
            (None, _) => Found::Build {
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
                if let (Some(store), Some(admitted)) = (store.filter(|_| keep), &admitted) {
                    store.abandon(admitted, kind);
                }
                return Err(refused);
            }
        };
        // Kept: built on the store's budget. Else the search's, on its own.
        let (_, bytes) = Corpus::finished_bytes(built.documents, span);
        let kept = match (store.filter(|_| keep), &admitted) {
            (Some(store), Some(admitted)) => store.keep_built(admitted, kind, bytes, |budget| {
                Corpus::finish(&built, span, budget)
            }),
            _ => None,
        };
        Ok(match (kept, own) {
            (Some(corpus), _) => CorpusRef::Kept(corpus),
            // Kept in the version's own slot, still on the search's budget: charged to the
            // request that built it until the version's last holder drops it.
            (None, Some(own)) => {
                let corpus = Corpus::finish(&built, span, request)?;
                CorpusRef::Kept(own.keep(kind, identity, corpus, request)?)
            }
            (None, None) => CorpusRef::Own(Corpus::finish(&built, span, request)?),
        })
    }

    /// This snapshot's version's own corpora ([`OwnCorpora`]), where the snapshot is
    /// unpublished (3.0c's [`super::Admission::Own`]); `None` for one with the
    /// shared-state capability, whose corpora the engine's store keeps.
    fn own_corpora(&self) -> Option<Arc<OwnCorpora>> {
        match self.admission() {
            super::Admission::Own(own) => Some(own.get_or_insert_with(OwnCorpora::default)),
            super::Admission::Shared(_) => None,
        }
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
                let own = self.own_lengths(documents, request)?;
                match (vectors, own) {
                    // Every typed literal is a member (of one word): the scan reads the
                    // vectors among them.
                    (true, _) => Bits::derived(&base, &changes, span, request, |_| 1)?,
                    (false, None) => self.underlying().with_text_index(iris, |index| {
                        index.derive(&base, &changes, span, request)
                    })?,
                    (false, Some(own)) => self.underlying().with_text_index(iris, |index| {
                        Bits::derived(&base, &changes, span, request, |document| {
                            own.length(index, document)
                        })
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
        // Read once: the slices charged are the slices walked.
        let per = slice_words(words, self.walk_slice_words());
        let (running, bytes) = self.walk_scratch(words, per, budget.remaining());
        scratch.charge(bytes)?;
        // Per graph, each kind's walk; a scan (where the index can't walk them) marks every
        // kind at once. Only the count needs the index: on a pool thread it is covered
        // while the statements are walked; elsewhere first.
        let marks: &mut [u64] = &mut bits;
        let mut walk = move || {
            let mut mark = |graph: GraphSelector| {
                for &kind in documents.kinds() {
                    if self.mark_values(model, graph, documents, kind, marks, per, running) {
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
        let own = self.own_lengths(documents, budget)?;
        Ok(match (documents, own) {
            (Documents::Vectors, _) => Bits::counted(bits, |_| 1),
            (Documents::Literals | Documents::Subjects, None) => self
                .underlying()
                .with_text_index(documents == Documents::Subjects, |index| index.corpus(bits)),
            (Documents::Literals | Documents::Subjects, Some(own)) => self
                .underlying()
                .with_text_index(documents == Documents::Subjects, |index| {
                    Bits::counted(bits, |document| own.length(index, document))
                }),
        })
    }

    /// A private snapshot's own documents' lengths for a corpus of `documents`, by overlay
    /// entry from its `start` (V3): each own term's words as the shared index would count
    /// them ([`crate::term::text::word_length`]; 0 for a term of another kind), charged to
    /// `budget`. `None` for a published snapshot, whose documents the shared index counts.
    fn own_lengths(
        &self,
        documents: Documents,
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<Option<OwnLengths>, nrese_exec::BudgetExceeded> {
        let Some(view) = self.own_view() else {
            return Ok(None);
        };
        let start = view.start();
        let count = (view.end() - start) as usize;
        let mut lengths = nrese_exec::ChargedVec::with_capacity(Arc::clone(budget), count)?;
        // An IRI's local-name text is made to be counted: its bytes charged first, let go
        // after (sol-loop r1 B4). Literals are counted in place.
        let mut scratch = nrese_exec::Reservation::new(Arc::clone(budget));
        for (id, key) in view.own_entries() {
            lengths.push(own_length(documents, id, key, &mut scratch)?)?;
        }
        Ok(Some(OwnLengths { start, lengths }))
    }

    /// The budget a text or IRI search's work is charged to: the request's where its caller
    /// brings one. For a private snapshot otherwise its reader's (the budget a query
    /// attached with [`Self::reading_within`]), else its private transaction's (the
    /// overlay's, which its view holds as long as it lives); each kept value holds its charge
    /// there (V3 review r1 B1: the global entry points brought none, and the work went
    /// uncharged). A published snapshot's caller that brings none is unbounded, as before.
    fn search_budget(&self, budget: Option<&Arc<nrese_exec::Budget>>) -> Arc<nrese_exec::Budget> {
        if let Some(budget) = budget {
            return Arc::clone(budget);
        }
        let Some(view) = self.own_view() else {
            return request_budget(None);
        };
        match self.reads.as_ref().and_then(|reads| reads.0.as_ref()) {
            Some(reads) => Arc::clone(&reads.budget),
            None => Arc::clone(view.budget()),
        }
    }

    /// The overlay view of a private snapshot (its own terms); `None` for a published one.
    fn own_view(&self) -> Option<&crate::term::overlay::OverlayView> {
        match &self.vocabulary {
            crate::term::resolve::Vocabulary::Private(view) => Some(view),
            crate::term::resolve::Vocabulary::Shared(_) => None,
        }
    }

    /// The own documents of `corpus` (a private snapshot's, of `documents`: literals or IRI
    /// names), indexed for one search with stems for `language`, charged to `budget` (V3).
    fn own_documents(
        &self,
        view: &crate::term::overlay::OverlayView,
        documents: Documents,
        corpus: &Corpus,
        language: Option<&str>,
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<crate::term::text::Local, nrese_exec::BudgetExceeded> {
        crate::term::text::Local::build(
            view.own_entries(),
            |entry| corpus.contains(entry),
            documents == Documents::Subjects,
            language,
            budget,
        )
    }

    /// Extends the index of `documents` (the vectors, for vector literals) over the
    /// dictionary's entries.
    fn cover(&self, documents: Documents) {
        match documents {
            Documents::Literals => self.underlying().prepare_text(false, None),
            Documents::Subjects => self.underlying().prepare_text(true, None),
            // The vector search covers its index itself, under its own lock.
            Documents::Vectors => {}
        }
    }

    /// How many slices of a corpus walk over `words` words of bits run at once, and the most
    /// they hold besides the bits: per slice its group counts (16 bytes a term, 64 terms a
    /// word of the slice) from each run, and as many again where they are merged or
    /// compared; and the parts a large range's grouping is cut into
    /// ([`crate::index::GROUP_PART`] entries each: a cut, a vector and at least four counts;
    /// in a run large enough to be walked in parts, the part's block of the walk's slab,
    /// [`crate::index::PART_BLOCK`] counts, instead of the four). A smaller run is always
    /// walked serially and holds no slab.
    /// The slices running at once cover disjoint ranges, so their parts are at most the
    /// runs' entries over a part, and two more a run and slice. As many at once as `room`
    /// holds, at least one, no more than the pool's threads nor than there are slices.
    fn walk_scratch(&self, words: usize, per: usize, room: usize) -> (usize, usize) {
        use crate::index::{GROUP_PART, PART_BLOCK};
        let runs = self.version.runs().count().max(1);
        let slice = per * 64 * 16 * runs * 2;
        let most = words.div_ceil(per).min(rayon::current_num_threads()).max(1);
        let running = (room / slice).clamp(1, most);
        let entries: u64 = self.version.runs().map(|run| run.entries()).sum();
        let parts = entries as usize / GROUP_PART + 2 * runs * running;
        let count = std::mem::size_of::<(u64, i64)>();
        let part = std::mem::size_of::<usize>() + std::mem::size_of::<Vec<(u64, i64)>>();
        let slabbed: usize = self
            .version
            .runs()
            .map(|run| run.entries() as usize)
            .filter(|&entries| entries >= 2 * GROUP_PART)
            .map(|entries| entries / GROUP_PART + 2 * running)
            .sum();
        let blocks = slabbed * (PART_BLOCK - 4) * count;
        (
            running,
            running * slice + parts * (part + 4 * count) + blocks,
        )
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
    /// named graph) in `model`. The entries in slices of `per` whole words, `running` at
    /// once on the pool: each a walk over the index's groups in its range of ids, so no list
    /// of every term is held. Where the index can't walk them (a reader of equality
    /// classes), one scan of the statements marks the terms of every kind of `documents`:
    /// then `true`.
    #[allow(clippy::too_many_arguments)]
    fn mark_values(
        &self,
        model: ReadModel,
        graphs: GraphSelector,
        documents: Documents,
        kind: TermKind,
        bits: &mut [u64],
        per: usize,
        running: usize,
    ) -> bool {
        use rayon::prelude::*;
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
        // Uncharged here, as before: no reader's budget reaches this walk (the snapshot read
        // owner's follow-up, D16-039 F7).
        let unlimited = nrese_exec::Budget::unlimited();
        let mut counts = nrese_exec::ChargedVec::new(&unlimited);
        let mut sources = 0;
        for stack in crate::engine::Stack::ALL {
            if model.includes(stack) {
                let before = counts.len();
                self.data(model)
                    .stack(stack)
                    .group_counts(plan, bound, &mut counts, &unlimited)
                    .expect("an unlimited budget");
                sources += usize::from(counts.len() > before);
            }
        }
        let (mut counts, _held) = counts.into_parts();
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

/// The own slot's losing builds (V3 review r1 F1, deterministic): two readers miss one
/// entry before either keeps it, both build, both keep; the second gets the first's value,
/// and its own value and its entry's charge are gone by the time `keep` returns. For a
/// corpus and for a private snapshot's own-document index. No lock is held between the miss
/// and the keep (each call takes and leaves it).
#[cfg(test)]
mod own_corpora_tests {
    use super::*;

    fn corpus(budget: &Arc<nrese_exec::Budget>) -> Corpus {
        let mut bits = nrese_exec::ChargedVec::with_capacity(Arc::clone(budget), 4).unwrap();
        bits.resize(4, 0b1011).unwrap();
        Corpus::finish(&Bits::counted(bits, |_| 1), 256, budget).unwrap()
    }

    #[test]
    fn a_losing_build_is_dropped_with_its_charge() {
        let budget = Arc::new(nrese_exec::Budget::new(1 << 20));
        let slot = OwnCorpora::default();
        let scope = TextScope::EVERY;
        let kind = KindRef {
            documents: Documents::Literals,
            model: ReadModel::Materialised,
            scope: &scope,
            equality: None,
            canonical: false,
        };
        let identity = super::super::SnapshotIdentity {
            revision: 0,
            content: crate::engine::Content::fresh(),
            equality: None,
            canonical: false,
        };
        // Both miss, both build.
        assert!(slot.corpus(kind, identity).is_none());
        assert!(slot.corpus(kind, identity).is_none());
        let none = budget.used();
        let first = corpus(&budget);
        let second = corpus(&budget);
        let one_value = (budget.used() - none) / 2;
        let first = slot.keep(kind, identity, first, &budget).unwrap();
        let one_kept = budget.used();
        let second = slot.keep(kind, identity, second, &budget).unwrap();
        assert!(Arc::ptr_eq(&first, &second), "the first value kept is kept");
        assert_eq!(
            budget.used(),
            one_kept - one_value,
            "the loser's value and its entry's charge are released"
        );
        let found = slot.corpus(kind, identity).unwrap();
        assert!(Arc::ptr_eq(&found, &first));

        // A private snapshot's own-document index beside that corpus, the same way.
        let local = || {
            crate::term::text::Local::build(std::iter::empty(), |_| true, false, None, &budget)
                .unwrap()
        };
        assert!(slot.local(&first, None).is_none());
        let (a, b) = (local(), local());
        let a = slot.keep_local(&first, None, a, &budget).unwrap();
        let before = budget.used();
        let alone = local();
        let one_local = budget.used() - before;
        drop(alone);
        let b = slot.keep_local(&first, None, b, &budget).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the first index kept is kept");
        assert_eq!(
            budget.used(),
            before - one_local,
            "the losing index and its entry's charge are released"
        );
    }
}

#[cfg(test)]
mod corpus_walk_tests {
    use super::*;
    use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

    /// A corpus walk over a run too small to be walked in parts holds no group walk's slab:
    /// its scratch is the serial walk's (package review r1 S2). One literal, so one word of
    /// bits (8 bytes), and one run of one statement: a slice's counts of that run and as many
    /// again (2,048 bytes), and two parts of a cut, a vector and four counts (192). With a
    /// query's earlier holds leaving 3,200 bytes, that fits and the walk is answered, the
    /// literal its one document; nothing is held after but the earlier holds. A slab's block
    /// counted for each of those parts (4,160 bytes of scratch) would refuse it.
    #[test]
    fn a_corpus_walk_over_a_run_too_small_for_parts_fits_the_serial_scratch() {
        let ex = |local: &str| NamedNode::new_unchecked(format!("http://example.com/{local}"));
        let engine = crate::Engine::new(crate::EngineConfig {
            background_maintenance: false,
            ..crate::EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let text: Term = Literal::new_simple_literal("red fox").into();
        let literal = tx.intern(text.as_ref());
        tx.insert(Quad::new(ex("s"), ex("p"), text, GraphName::DefaultGraph).as_ref());
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        // The shape stated: one word of bits; one run, of fewer entries than a part.
        assert_eq!(snapshot.version.dictionary_len.div_ceil(64), 1);
        let runs: Vec<_> = snapshot.version.runs().collect();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].entries() < crate::index::GROUP_PART as u64);

        let limit = 1 << 20;
        let budget = Arc::new(nrese_exec::Budget::new(limit));
        let earlier = limit - 3_200;
        budget.charge(earlier).unwrap();
        let corpus = snapshot
            .walk_corpus(
                Documents::Literals,
                ReadModel::Asserted,
                &TextScope::EVERY,
                &budget,
            )
            .expect("the serial walk's scratch fits the room left");
        assert_eq!(corpus.documents, 1);
        let at = literal.payload();
        assert_eq!((corpus.bits[(at / 64) as usize] >> (at % 64)) & 1, 1);
        drop(corpus);
        assert!(!budget.refused(), "no refusal recorded on the budget");
        assert_eq!(budget.used(), earlier);
        budget.release(earlier);
    }
}
