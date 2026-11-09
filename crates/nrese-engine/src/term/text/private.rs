//! Search over a private snapshot (dictionary lifecycle V3; the contract's §7): its shared
//! documents through the shared index, its own (the overlay's terms its statements use)
//! through an index of their own, under one search body ([`super::search_with`]).
//!
//! - **Coordinates:** the overlay's ids sit past the shared index's documents and may share
//!   numbers with entries published after the overlay opened, so the shared side answers for
//!   documents below the overlay's `start` alone, and the own documents are numbered `0..k`
//!   in their own index ([`Local`]), translated to their overlay ids as postings are read
//!   (every one above every shared document, so the two lists concatenate in order).
//! - **Statistics:** the corpus's, which counts both sides ([`super::Corpus`], built by the
//!   snapshot with each own document's length from its text, [`super::word_length`]).
//! - **Stems:** prepared on each side, or on neither.

use std::borrow::Cow;
use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, Reservation};
use rust_stemmers::Stemmer;

use super::{Corpus, Indexed, Match, Scope, Source, TextIndex, Whole};
use crate::term::TermKind;

/// A private search's own documents: an index over them numbered `0..k`, local document `i`
/// being overlay entry `members[i]` (ascending), with each one's text and kind. Built for one
/// search over the corpus's own members, its bytes charged to the search's budget first.
pub(crate) struct Local {
    index: TextIndex,
    members: Vec<u64>,
    texts: Vec<Box<str>>,
    kinds: Vec<TermKind>,
    _charge: Reservation,
}

impl Local {
    /// The bytes [`Self::build`] charges for documents of `text_bytes` bytes in all: an upper
    /// estimate of their copies, the index's words and postings, and their numbers, kinds and
    /// lengths (three bytes a text byte, 64 a document).
    fn charged(documents: usize, text_bytes: usize) -> usize {
        text_bytes.saturating_mul(3) + documents.saturating_mul(64)
    }

    /// The index of `documents` (overlay entry, kind, text; entries ascending, each with a
    /// word), stems prepared for `language`, each document charged to `budget` before it is
    /// kept.
    pub(crate) fn build(
        documents: impl IntoIterator<Item = (u64, TermKind, String)>,
        language: Option<&str>,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let mut charge = Reservation::new(Arc::clone(budget));
        let mut index = TextIndex::default();
        let (mut members, mut texts, mut kinds) = (Vec::new(), Vec::new(), Vec::new());
        for (entry, kind, text) in documents {
            charge.charge(Self::charged(1, text.len()))?;
            index.add(members.len() as u64, &text);
            members.push(entry);
            texts.push(text.into_boxed_str());
            kinds.push(kind);
        }
        if let Some(language) = language {
            index.prepare_stems(language);
        }
        Ok(Self {
            index,
            members,
            texts,
            kinds,
            _charge: charge,
        })
    }

    fn position(&self, entry: u64) -> Option<usize> {
        self.members.binary_search(&entry).ok()
    }

    /// The text of own document `entry` (an overlay id's payload).
    pub(crate) fn text(&self, entry: u64) -> Option<&str> {
        self.position(entry).map(|at| &*self.texts[at])
    }

    /// The kind of own document `entry`.
    pub(crate) fn kind(&self, entry: u64) -> Option<TermKind> {
        self.position(entry).map(|at| self.kinds[at])
    }
}

/// The shared index's postings among the corpus's documents below `start`: none of the
/// overlay's, nor an entry published after it opened under one of their numbers.
struct Below<'a> {
    corpus: &'a Corpus,
    start: u64,
}

impl Scope for Below<'_> {
    fn documents(&self) -> u64 {
        self.corpus.documents()
    }

    fn total_words(&self) -> u64 {
        self.corpus.total_words()
    }

    fn postings<'l>(&self, list: Cow<'l, [(u64, u16)]>) -> Cow<'l, [(u64, u16)]> {
        Cow::Owned(
            list.iter()
                .filter(|&&(document, _)| document < self.start && self.corpus.contains(document))
                .copied()
                .collect(),
        )
    }
}

/// A private snapshot's search source: the shared index below `start` and its own
/// documents' index, with the corpus's statistics (both sides).
pub(crate) struct Private<'a> {
    shared: Indexed<'a, Below<'a>>,
    own: Indexed<'a, Whole<'a>>,
    local: &'a Local,
    corpus: &'a Corpus,
    start: u64,
}

impl<'a> Private<'a> {
    /// Over `shared` (prepared for `language` by its owner) and `local`, for `corpus`'s
    /// documents, the overlay's own from `start`. Stemming where both sides have the stems.
    pub(crate) fn new(
        shared: &'a TextIndex,
        corpus: &'a Corpus,
        start: u64,
        local: &'a Local,
        language: Option<&str>,
    ) -> Self {
        let mut shared = Indexed::new(shared, Below { corpus, start }, language);
        let mut own = Indexed::new(&local.index, Whole(&local.index), language);
        if shared.stems.is_some() != own.stems.is_some() {
            shared.stems = None;
            own.stems = None;
        }
        Self {
            shared,
            own,
            local,
            corpus,
            start,
        }
    }
}

impl Source for Private<'_> {
    fn documents(&self) -> u64 {
        self.corpus.documents()
    }

    fn total_words(&self) -> u64 {
        self.corpus.total_words()
    }

    fn postings(&self, word: &str, matching: Match) -> Cow<'_, [(u64, u16)]> {
        let shared = self.shared.postings(word, matching);
        let own = self.own.postings(word, matching);
        if own.is_empty() {
            return shared;
        }
        let mut both = shared.into_owned();
        both.extend(
            own.iter()
                .map(|&(local, count)| (self.local.members[local as usize], count)),
        );
        Cow::Owned(both)
    }

    fn excluding(&self, word: &str) -> Vec<u64> {
        let mut out = self.shared.excluding(word);
        out.retain(|&document| document < self.start);
        out.extend(
            self.own
                .excluding(word)
                .into_iter()
                .map(|local| self.local.members[local as usize]),
        );
        out
    }

    fn length(&self, document: u64) -> f64 {
        match document < self.start {
            true => self.shared.length(document),
            false => self
                .local
                .position(document)
                .map_or(1.0, |at| self.own.length(at as u64)),
        }
    }

    fn stemmer(&self) -> Option<&Stemmer> {
        self.shared.stemmer()
    }
}
