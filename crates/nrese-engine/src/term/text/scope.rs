//! Reader-scoped full-text search (`SearchScope::Reader`): BM25 over a [`Corpus`], the
//! documents a reader can see, instead of over every indexed document. The corpus gives
//! the document count, the mean length and, through its membership, each word's document
//! frequency; postings outside it are skipped, so nothing outside it changes a score, an
//! order or the best match the relevance is divided by.
//!
//! Beside [`TextIndex::search`], not through it: the dictionary-wide search keeps its own
//! body and cost (the scope is the caller's choice, made before either is called).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

use super::{Match, Stems, TextIndex, TextMatch, TextQuery, stem_key, stemmer, words};

/// The numbers whose bits `bits` sets, ascending.
fn ones(bits: &[u64]) -> impl Iterator<Item = u64> + '_ {
    bits.iter().enumerate().flat_map(|(w, &word)| {
        let mut rest = word;
        std::iter::from_fn(move || {
            (rest != 0).then(|| {
                let bit = rest.trailing_zeros();
                rest &= rest - 1;
                w as u64 * 64 + u64::from(bit)
            })
        })
    })
}

/// The documents a search ranks among (by number: dictionary entries), with the
/// statistics BM25 takes from them: built by the caller from what a reader sees
/// ([`Corpus::finish`]). Its members are held on the budget they were charged to before
/// they were allocated, until it drops.
#[derive(Debug)]
pub struct Corpus {
    members: Members,
    documents: u64,
    total_words: u64,
    _charge: Reservation,
}

/// A corpus being built: a bit per dictionary entry, charged to the building search's
/// budget, with the count of its documents and their words.
pub(crate) struct Bits {
    pub(crate) bits: ChargedVec<u64>,
    pub(crate) documents: u64,
    pub(crate) total_words: u64,
}

impl Bits {
    /// The documents whose bits `bits` sets (a bit per number below its span), with their
    /// count and words: `length` gives a number's words (0: not a document, left out).
    pub(crate) fn counted(mut bits: ChargedVec<u64>, length: impl Fn(u64) -> u16) -> Self {
        let (mut documents, mut total_words) = (0u64, 0u64);
        for (w, word) in bits.iter_mut().enumerate() {
            let mut rest = *word;
            while rest != 0 {
                let bit = rest.trailing_zeros();
                rest &= rest - 1;
                match length(w as u64 * 64 + u64::from(bit)) {
                    0 => *word &= !(1 << bit),
                    length => {
                        documents += 1;
                        total_words += u64::from(length);
                    }
                }
            }
        }
        Self {
            bits,
            documents,
            total_words,
        }
    }

    /// `base` with each document of `changes` a member or not (numbers below `span`;
    /// numbers `length` gives no words stay out): its counts adjusted, not counted again.
    pub(crate) fn derived(
        base: &Corpus,
        changes: &[(u64, bool)],
        span: u64,
        budget: &Arc<Budget>,
        length: impl Fn(u64) -> u16,
    ) -> Result<Self, BudgetExceeded> {
        let mut bits = base.bits(span, budget)?;
        let (mut documents, mut total_words) = (base.documents, base.total_words);
        for &(document, member) in changes {
            let (word, bit) = ((document / 64) as usize, document % 64);
            let was = bits[word] >> bit & 1 == 1;
            let length = length(document);
            if member && !was && length > 0 {
                bits[word] |= 1 << bit;
                documents += 1;
                total_words += u64::from(length);
            } else if !member && was {
                bits[word] &= !(1 << bit);
                documents -= 1;
                total_words -= u64::from(length);
            }
        }
        Ok(Self {
            bits,
            documents,
            total_words,
        })
    }
}

/// A corpus's documents: a sorted list where they are few against the numbers they span,
/// else a bit per number.
#[derive(Debug)]
enum Members {
    Sorted(Vec<u64>),
    Bits(Vec<u64>),
}

impl Corpus {
    /// Its documents (the indexed ones: literals with a word; for vectors, every member).
    pub fn documents(&self) -> u64 {
        self.documents
    }

    /// Its words, counted with repetition.
    pub fn total_words(&self) -> u64 {
        self.total_words
    }

    /// Whether `document` is one of its documents.
    #[inline]
    pub fn contains(&self, document: u64) -> bool {
        match &self.members {
            Members::Bits(bits) => {
                let word = (document / 64) as usize;
                word < bits.len() && bits[word] >> (document % 64) & 1 == 1
            }
            Members::Sorted(sorted) => sorted.binary_search(&document).is_ok(),
        }
    }

    /// Its documents, ascending.
    pub fn members(&self) -> Vec<u64> {
        match &self.members {
            Members::Sorted(sorted) => sorted.clone(),
            Members::Bits(bits) => ones(bits).collect(),
        }
    }

    /// Its documents as a bit per number below `span` (at least its own), charged to
    /// `budget` before they are allocated.
    fn bits(&self, span: u64, budget: &Arc<Budget>) -> Result<ChargedVec<u64>, BudgetExceeded> {
        let words = match &self.members {
            Members::Sorted(_) => span.div_ceil(64) as usize,
            Members::Bits(bits) => (span.div_ceil(64) as usize).max(bits.len()),
        };
        let mut out = ChargedVec::with_capacity(Arc::clone(budget), words)?;
        match &self.members {
            Members::Sorted(sorted) => {
                out.resize(words, 0)?;
                for &document in sorted {
                    out[(document / 64) as usize] |= 1 << (document % 64);
                }
            }
            Members::Bits(bits) => {
                out.extend_from_slice(bits)?;
                out.resize(words, 0)?;
            }
        }
        Ok(out)
    }

    /// The corpus of `bits` (a bit per number below `span`, every one a document), its
    /// members charged to `budget` before they are allocated: a sorted list where that is
    /// smaller than a bit per number.
    pub(crate) fn finish(
        bits: &Bits,
        span: u64,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let (members, charge) = match Self::finished_bytes(bits.documents, span) {
            (true, _) => {
                let mut out = ChargedVec::with_capacity(Arc::clone(budget), bits.bits.len())?;
                out.extend_from_slice(&bits.bits)?;
                let (members, charge) = out.into_parts();
                (Members::Bits(members), charge)
            }
            (false, _) => {
                let count = bits.documents as usize;
                let out = ChargedVec::collect(Arc::clone(budget), count, ones(&bits.bits))?;
                let (members, charge) = out.into_parts();
                (Members::Sorted(members), charge)
            }
        };
        Ok(Self {
            members,
            documents: bits.documents,
            total_words: bits.total_words,
            _charge: charge,
        })
    }

    /// Whether a corpus of `documents` below `span` is kept as bits, and the bytes its
    /// members take: what [`Self::finish`] charges.
    pub(crate) fn finished_bytes(documents: u64, span: u64) -> (bool, usize) {
        match documents * 64 > span {
            true => (true, span.div_ceil(64) as usize * 8),
            false => (false, documents as usize * 8),
        }
    }
}

impl TextIndex {
    /// Words of `document` (0: not indexed).
    pub(crate) fn length_of(&self, document: u64) -> u16 {
        self.lengths.get(document as usize).copied().unwrap_or(0)
    }

    /// The corpus of the documents whose bits `bits` sets (a bit per number below `span`;
    /// numbers this index has no words for are left out), with their count and words. One
    /// thread: it runs under the index's lock.
    pub(crate) fn corpus(&self, bits: ChargedVec<u64>) -> Bits {
        Bits::counted(bits, |document| self.length_of(document))
    }

    /// `base` with each document of `changes` a member or not (numbers below `span`;
    /// numbers this index has no words for stay out): its counts adjusted, not counted
    /// again.
    pub(crate) fn derive(
        &self,
        base: &Corpus,
        changes: &[(u64, bool)],
        span: u64,
        budget: &Arc<Budget>,
    ) -> Result<Bits, BudgetExceeded> {
        Bits::derived(base, changes, span, budget, |document| {
            self.length_of(document)
        })
    }

    /// [`Self::postings_of`] restricted to `corpus`'s documents.
    fn postings_in(
        &self,
        word: &str,
        matching: Match,
        stems: Option<(&rust_stemmers::Stemmer, &Stems)>,
        corpus: &Corpus,
    ) -> Vec<(u64, u16)> {
        let mut list = match self.postings_of(word, matching, stems) {
            Cow::Borrowed(list) => list.to_vec(),
            Cow::Owned(list) => list,
        };
        list.retain(|&(document, _)| corpus.contains(document));
        list
    }

    /// The documents of `corpus` matching `query`, best first (ties by number), scored by
    /// BM25 (k1 = 1.2, b = 0.75) with `corpus`'s statistics alone: its document count and
    /// mean length, each word's frequency among its documents, the best match among them.
    /// The query is read as [`Self::search`] reads it; `text_of` gives a document's text,
    /// to check phrases.
    pub(crate) fn search_in(
        &self,
        query: &TextQuery,
        text_of: &dyn Fn(u64) -> Option<String>,
        corpus: &Corpus,
    ) -> Vec<TextMatch> {
        let documents = corpus.documents() as f64;
        if documents == 0.0 {
            return Vec::new();
        }
        let average = corpus.total_words() as f64 / documents;
        let (k1, b) = (1.2, 0.75);
        let stemming = query
            .stem
            .as_deref()
            .and_then(|language| Some((stemmer(language)?, self.stems.get(&stem_key(language))?)));
        let stems = stemming.as_ref().map(|(stemmer, stems)| (stemmer, *stems));
        let normal = |word: &str| -> String {
            match &stemming {
                Some((stemmer, _)) => stemmer.stem(word).into_owned(),
                None => word.to_owned(),
            }
        };
        let (terms, excluded, phrases) = parse(query);
        if terms.is_empty() && phrases.is_empty() {
            return Vec::new();
        }
        let mut scores: HashMap<u64, (f64, usize)> = HashMap::new();
        let score = |frequency: f64, id: u64, count: u16| {
            let idf = (1.0 + (documents - frequency + 0.5) / (frequency + 0.5)).ln();
            let length = self.length(id);
            let tf = f64::from(count);
            idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * length / average))
        };
        for phrase in &phrases {
            let lists: Vec<Vec<(u64, u16)>> = phrase
                .iter()
                .map(|word| self.postings_in(word, Match::Exact, stems, corpus))
                .collect();
            if lists.iter().any(Vec::is_empty) {
                continue;
            }
            let mut phrase_scores: HashMap<u64, f64> = HashMap::new();
            for list in &lists {
                let frequency = list.len() as f64;
                for &(id, count) in list {
                    *phrase_scores.entry(id).or_default() += score(frequency, id, count);
                }
            }
            let phrase: Vec<String> = phrase.iter().map(|word| normal(word)).collect();
            let rarest = lists
                .iter()
                .min_by_key(|list| list.len())
                .expect("two words");
            for &(id, _) in rarest {
                let in_all = lists
                    .iter()
                    .all(|list| list.binary_search_by_key(&id, |&(i, _)| i).is_ok());
                let in_sequence = in_all
                    && text_of(id).is_some_and(|text| {
                        let text: Vec<String> = words(&text).map(|word| normal(&word)).collect();
                        text.windows(phrase.len())
                            .any(|window| window == phrase.as_slice())
                    });
                if in_sequence {
                    let entry = scores.entry(id).or_default();
                    entry.0 += phrase_scores.get(&id).copied().unwrap_or(0.0);
                    entry.1 += 1;
                }
            }
        }
        for (word, matching) in &terms {
            let found = self.postings_in(word, *matching, stems, corpus);
            let frequency = found.len() as f64;
            for &(id, count) in &found {
                let entry = scores.entry(id).or_default();
                entry.0 += score(frequency, id, count);
                entry.1 += 1;
            }
        }
        let needed = if query.all_words {
            terms.len() + phrases.len()
        } else {
            1
        };
        let excluded: HashSet<u64> = excluded
            .iter()
            .flat_map(|word| {
                self.postings_of(word, Match::Exact, stems)
                    .iter()
                    .map(|&(id, _)| id)
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut matches: Vec<TextMatch> = scores
            .into_iter()
            .filter(|(id, (_, words))| *words >= needed && !excluded.contains(id))
            .map(|(id, (score, _))| TextMatch {
                id,
                relevance: score,
            })
            .collect();
        let best = matches
            .iter()
            .map(|m| m.relevance)
            .fold(f64::MIN_POSITIVE, f64::max);
        for m in &mut matches {
            m.relevance /= best;
        }
        matches.sort_by(|a, b| {
            b.relevance
                .total_cmp(&a.relevance)
                .then_with(|| a.id.cmp(&b.id))
        });
        matches
    }
}

/// The query's words with how each matches, the words to exclude and the phrases, as
/// [`TextIndex::search`] reads them.
#[allow(clippy::type_complexity)]
fn parse(query: &TextQuery) -> (Vec<(String, Match)>, Vec<String>, Vec<Vec<String>>) {
    let mut terms: Vec<(String, Match)> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut phrases: Vec<Vec<String>> = Vec::new();
    for (i, part) in query.text.split('"').enumerate() {
        if i % 2 == 1 {
            let phrase: Vec<String> = words(part).collect();
            match phrase.len() {
                0 => {}
                1 => terms.push((phrase[0].clone(), Match::Exact)),
                _ => phrases.push(phrase),
            }
            continue;
        }
        for raw in part.split_whitespace() {
            if let Some(rest) = raw.strip_prefix('-').filter(|rest| !rest.is_empty()) {
                excluded.extend(words(rest));
                continue;
            }
            let (raw, matching) = match raw.split_once('~') {
                Some((word, edits)) => {
                    let edits = edits.parse::<u8>().unwrap_or(2).min(2);
                    (word, Match::Fuzzy(edits))
                }
                None if query.prefix || raw.ends_with('*') => (raw, Match::Prefix),
                None => (raw, Match::Exact),
            };
            for word in words(raw) {
                if !terms.iter().any(|(w, m)| *w == word && *m == matching) {
                    terms.push((word, matching));
                }
            }
        }
    }
    (terms, excluded, phrases)
}
