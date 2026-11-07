//! A reader's corpus (`SearchScope::Reader`): the documents a reader can see, with the
//! statistics BM25 takes from them. [`TextIndex::search_over`] ranks among them as it ranks
//! among every indexed document ([`super::Whole`]): one search body, the scope a type
//! ([`super::Scope`]), so nothing outside the corpus changes a score, an order or the best
//! match the relevance is divided by.

use std::borrow::Cow;
use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

use super::TextIndex;

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

impl super::Scope for Corpus {
    fn documents(&self) -> u64 {
        self.documents
    }

    fn total_words(&self) -> u64 {
        self.total_words
    }

    /// The postings of its documents alone: a word's frequency and the best match are the
    /// corpus's.
    fn postings<'l>(&self, list: Cow<'l, [(u64, u16)]>) -> Cow<'l, [(u64, u16)]> {
        Cow::Owned(
            list.iter()
                .filter(|&&(document, _)| self.contains(document))
                .copied()
                .collect(),
        )
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
}
