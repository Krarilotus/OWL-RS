//! The dictionary's searches scoped to a reader's corpus (`SearchScope::Reader`): the same
//! indexes and lock order as the global searches beside them, ranked or selected among the
//! corpus's documents alone ([`crate::term::text::Corpus`]).

use super::{Dictionary, TermId, TermKind, TermView, view_key};
use crate::term::text::{Corpus, TextIndex, TextMatch, TextQuery};

impl Dictionary {
    /// Calls `f` with the text index of the string literals (with `iris`, of the IRIs'
    /// local names) under its read lock, without `inner`'s: for counting a corpus, on the
    /// caller's thread.
    pub(crate) fn with_text_index<R>(&self, iris: bool, f: impl FnOnce(&TextIndex) -> R) -> R {
        let slot = match iris {
            false => &self.text,
            true => &self.iri_text,
        };
        f(&slot.read())
    }

    /// The string literals of `corpus` matching `query`, best first, ranked among the
    /// corpus's documents alone ([`TextIndex::search_in`]). The index takes in the terms
    /// interned since first, as [`Self::text_search`]'s does.
    pub(crate) fn text_search_in(&self, query: &TextQuery, corpus: &Corpus) -> Vec<TextMatch> {
        self.prepare_text(false, query.stem.as_deref());
        // The index's lock before `inner`'s (the lock order).
        let text = self.text.read();
        let inner = self.inner.read();
        let text_of = |index: u64| match view_key(&inner.key(index)) {
            TermView::String(value) | TermView::LangString { value, .. } => Some(value.to_owned()),
            _ => None,
        };
        let mut matches = text.search_in(query, &text_of, corpus);
        for found in &mut matches {
            let kind = match view_key(&inner.key(found.id)) {
                TermView::LangString { .. } => TermKind::LangString,
                _ => TermKind::String,
            };
            found.id = TermId::new(kind, found.id).raw();
        }
        matches
    }

    /// The IRIs of `corpus` whose local name's words match `query`, best first, ranked among
    /// the corpus's documents alone.
    pub(crate) fn iri_search_in(&self, query: &TextQuery, corpus: &Corpus) -> Vec<TextMatch> {
        self.prepare_text(true, query.stem.as_deref());
        let text = self.iri_text.read();
        let inner = self.inner.read();
        let text_of = |index: u64| match view_key(&inner.key(index)) {
            TermView::Iri(iri) => crate::term::text::local_name_text(iri),
            _ => None,
        };
        let mut matches = text.search_in(query, &text_of, corpus);
        for found in &mut matches {
            found.id = TermId::new(TermKind::Iri, found.id).raw();
        }
        matches
    }

    /// What a reader's vector search like `query` reads: the space of its dimension (taken
    /// under the index's lock, read without it: [`crate::term::vectors::SearchInput`]), the
    /// vector literals interned since taken in first; `None` where no vector has that
    /// dimension (the search then finds nothing, before it builds a corpus). No graph is
    /// built or read: [`crate::term::vectors::search_eligible`] compares every eligible one.
    pub(crate) fn vector_input(
        &self,
        query: &crate::VectorQuery,
    ) -> Option<crate::term::vectors::SearchInput> {
        self.cover_vectors();
        self.vectors.read().search_input(query)
    }
}
