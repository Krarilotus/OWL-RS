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

use super::{
    Corpus, Indexed, Match, Scope, Source, TextIndex, Whole, local_name_text,
    local_name_text_bytes, word_count,
};
use crate::term::dictionary::view_key;
use crate::term::{TermId, TermKind, TermView};

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

/// What [`Local::build`] charges before it allocates (sol-loop r1 B1), as upper bounds of
/// what [`TextIndex`] and the build allocate, kept until `Local` drops (so transient and kept
/// allocations both count):
/// - before anything: the index's shard maps and stem tables (256 maps of 24 bytes, rounded
///   up);
/// - a document, before its text is made: its number, kind and text pointer with their
///   vectors' doubling, and its length slot ([`PER_DOCUMENT`]); its text, as a copy or as
///   an IRI's local-name text ([`local_name_text_bytes`]);
/// - then, before it is indexed, each byte of the text ([`PER_BYTE`]: its lower-cased words,
///   up to three bytes a byte, once while tokenised and once as keys, and their stem table
///   copies), and each word ([`PER_WORD`]: a tokenising map slot with its `String` (88), a
///   B-tree entry with its node's share at the minimum fill of five (170), a posting buffer
///   at its doubling (34), a stem table slot with its `String` and vector (128); 420, rounded
///   up).
const UPFRONT: usize = 8 << 10;
const PER_DOCUMENT: usize = 128;
const PER_BYTE: usize = 12;
const PER_WORD: usize = 512;

impl Local {
    /// The index of the overlay's own entries `own` (id and key, ascending) that `member`
    /// takes, as documents of literals (`iris` false: their values) or of IRI names (their
    /// local-name text), stems prepared for `language`; each charged to `budget` before it
    /// is made ([`PER_WORD`] and the others). Entries of other kinds, or with no word, are
    /// left out.
    pub(crate) fn build<'k>(
        own: impl IntoIterator<Item = (TermId, &'k [u8])>,
        member: impl Fn(u64) -> bool,
        iris: bool,
        language: Option<&str>,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let mut charge = Reservation::new(Arc::clone(budget));
        charge.charge(UPFRONT)?;
        let mut index = TextIndex::default();
        let (mut members, mut texts, mut kinds) = (Vec::new(), Vec::new(), Vec::new());
        for (id, key) in own {
            if !member(id.payload()) {
                continue;
            }
            let text = match (iris, id.kind(), view_key(key)) {
                (false, _, TermView::String(value) | TermView::LangString { value, .. }) => {
                    if word_count(value) == 0 {
                        continue;
                    }
                    charge.charge(PER_DOCUMENT + value.len())?;
                    value.to_owned()
                }
                (true, TermKind::Iri, TermView::Iri(iri)) => {
                    charge.charge(PER_DOCUMENT + local_name_text_bytes(iri.len()))?;
                    match local_name_text(iri) {
                        Some(text) if word_count(&text) > 0 => text,
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let words = word_count(&text);
            charge.charge(PER_BYTE * text.len() + PER_WORD * words)?;
            index.add(members.len() as u64, &text);
            members.push(id.payload());
            texts.push(text.into_boxed_str());
            kinds.push(id.kind());
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
