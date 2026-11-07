//! What a reader's searches (`SearchScope::Reader`) read from the dictionary beside the
//! searches themselves ([`Dictionary::text_search_in`], [`Dictionary::iri_search_in`]): the
//! text index for counting a corpus, and a vector search's input.

use super::Dictionary;
use crate::term::text::TextIndex;

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
