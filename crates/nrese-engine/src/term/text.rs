//! Full-text index over the dictionary's string literals.
//!
//! An inverted index from lower-cased words to the literals (simple and language-tagged
//! strings) that contain them, by their entry numbers in the dictionary; postings are
//! varint deltas, the words in 256 shards by their first byte, so that the build runs on
//! every core (DBpedia's 40 M terms). It is built the first time a query searches, and extended
//! with the terms the dictionary gained since before each later search: the dictionary is
//! append-only, so nothing indexed ever changes. A large extension is built apart, as an
//! index of the new documents alone (a fragment), with no lock held, and then taken in
//! ([`TextIndex::absorb`]): a part of the parallel build may run a job that wants the
//! dictionary or this index. Taking it in runs under the index's lock, on threads of its
//! own (scoped, not Rayon's: they run nothing else, so no job they take on can wait for
//! the lock); into an empty index it is a move. Whether a literal still occurs in the data
//! is the query's business (it joins the matches with the statements).
//!
//! Scores are BM25 (k1 = 1.2, b = 0.75) summed over the query's words and divided by the
//! best match's, so relevance lies in (0, 1] as Blazegraph's does. A search ranks among the
//! documents of a [`Corpus`] only: the literals a reader's snapshot and graphs hold (the
//! dictionary is shared by every version and transaction, and holds what no statement uses
//! any more). Its document count, average length and each word's document frequency are
//! counted over the corpus, so no score depends on a literal outside it.
//!
//! A part of the query in double quotes is a phrase (`"quick brown" fox`): it matches the
//! literals whose words hold it as a sequence, and counts as one of the query's terms.
//! Its words find the candidates in the index; each is checked against its text.
//!
//! A word after `-` is excluded: literals that have it don't match. A word ending in `~`
//! (or `~1`, `~2`) matches the indexed words within that many edits (insertions, deletions,
//! substitutions, transpositions; `~` is two), as Lucene's fuzzy queries do.
//!
//! With a stemming language ([`TextQuery::stem`]), words match by their Snowball stem
//! (`connected` finds `connection` and `connecting`), phrases too. The index keeps the
//! words as written; per language, a map from stems to the indexed words with that stem
//! is built at the first stemmed search and extended at later ones.

use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation, Room};
use std::collections::{BTreeMap, HashMap};

use rayon::prelude::*;
use rust_stemmers::{Algorithm, Stemmer};

/// What to search for.
#[derive(Debug, Clone, PartialEq)]
pub struct TextQuery {
    /// The words to find; a word ending in `*` matches every word it starts, and words in
    /// double quotes are a phrase.
    pub text: String,
    /// Every word must occur (else any word may).
    pub all_words: bool,
    /// Every word matches as a prefix.
    pub prefix: bool,
    /// Match words by their stem in this language (ISO 639-1: `en`, `de`, `fr`, ...;
    /// [`stemmer`] lists them). `None`, or a language without a stemmer: as written.
    pub stem: Option<String>,
}

/// The Snowball stemmer for `language` (an ISO 639-1 code, a region after `-` ignored).
pub fn stemmer(language: &str) -> Option<Stemmer> {
    let code = language.split('-').next()?.to_ascii_lowercase();
    let algorithm = match code.as_str() {
        "ar" => Algorithm::Arabic,
        "da" => Algorithm::Danish,
        "nl" => Algorithm::Dutch,
        "en" => Algorithm::English,
        "fi" => Algorithm::Finnish,
        "fr" => Algorithm::French,
        "de" => Algorithm::German,
        "el" => Algorithm::Greek,
        "hu" => Algorithm::Hungarian,
        "it" => Algorithm::Italian,
        "no" | "nb" | "nn" => Algorithm::Norwegian,
        "pt" => Algorithm::Portuguese,
        "ro" => Algorithm::Romanian,
        "ru" => Algorithm::Russian,
        "es" => Algorithm::Spanish,
        "sv" => Algorithm::Swedish,
        "ta" => Algorithm::Tamil,
        "tr" => Algorithm::Turkish,
        _ => return None,
    };
    Some(Stemmer::create(algorithm))
}

/// How a query word matches indexed words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    Exact,
    Prefix,
    /// Within this many edits.
    Fuzzy(u8),
}

/// Whether `a` and `b` are at most `max` edits apart: insertions, deletions and
/// substitutions of characters, and transpositions of neighbours, as Lucene counts them
/// (optimal string alignment).
#[cfg(test)]
fn within_edits(a: &str, b: &str, max: usize) -> bool {
    let budget = Arc::new(Budget::unlimited());
    Edits::new(&budget)
        .within(a, b, max)
        .expect("no budget to exceed")
}

/// The characters and rows [`Edits::within`] compares words with, kept from one word to
/// the next (a fuzzy match compares its word with every indexed word), charged.
struct Edits {
    a: ChargedVec<char>,
    b: ChargedVec<char>,
    before: ChargedVec<usize>,
    row: ChargedVec<usize>,
    next: ChargedVec<usize>,
}

impl Edits {
    fn new(budget: &Arc<Budget>) -> Self {
        Self {
            a: ChargedVec::new(Arc::clone(budget)),
            b: ChargedVec::new(Arc::clone(budget)),
            before: ChargedVec::new(Arc::clone(budget)),
            row: ChargedVec::new(Arc::clone(budget)),
            next: ChargedVec::new(Arc::clone(budget)),
        }
    }

    /// Whether `a` and `b` are at most `max` edits apart ([`within_edits`]).
    fn within(&mut self, a: &str, b: &str, max: usize) -> Result<bool, BudgetExceeded> {
        let Self {
            a: a_chars,
            b: b_chars,
            before,
            row,
            next,
        } = self;
        a_chars.clear();
        a_chars.extend_bounded(a.len(), a.chars())?;
        b_chars.clear();
        b_chars.extend_bounded(b.len(), b.chars())?;
        let (a, b) = (&a_chars[..], &b_chars[..]);
        if a.len().abs_diff(b.len()) > max {
            return Ok(false);
        }
        row.clear();
        row.extend_bounded(b.len() + 1, 0..=b.len())?;
        // Read only from the second row on, when it holds the row before the last.
        before.clear();
        before.resize(b.len() + 1, 0)?;
        next.clear();
        next.resize(b.len() + 1, 0)?;
        for i in 0..a.len() {
            next[..].fill(i + 1);
            for j in 0..b.len() {
                let mut cost = (row[j] + usize::from(a[i] != b[j]))
                    .min(row[j + 1] + 1)
                    .min(next[j] + 1);
                if i > 0 && j > 0 && a[i] == b[j - 1] && a[i - 1] == b[j] {
                    cost = cost.min(before[j - 1] + 1);
                }
                next[j + 1] = cost;
            }
            if next.iter().min().is_some_and(|&least| least > max) {
                return Ok(false);
            }
            std::mem::swap(before, row);
            std::mem::swap(row, next);
        }
        Ok(row[b.len()] <= max)
    }
}

/// The key of a stemming language: its code, lower case, without a region.
fn stem_key(language: &str) -> String {
    language
        .split('-')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// A literal that matched, with its relevance (the best match has 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMatch {
    pub id: u64,
    pub relevance: f64,
}

/// A search's matches, best first, held on the budget they were charged to until they
/// drop.
#[derive(Debug)]
pub struct TextMatches {
    /// Before the charge: fields drop in order, the matches first.
    matches: Vec<TextMatch>,
    _charge: Reservation,
}

impl TextMatches {
    /// No matches (nothing allocated).
    pub(crate) fn empty(budget: &Arc<Budget>) -> Self {
        Self {
            matches: Vec::new(),
            _charge: Reservation::new(Arc::clone(budget)),
        }
    }
}

impl std::ops::Deref for TextMatches {
    type Target = [TextMatch];

    fn deref(&self) -> &[TextMatch] {
        &self.matches
    }
}

impl std::ops::DerefMut for TextMatches {
    fn deref_mut(&mut self) -> &mut [TextMatch] {
        &mut self.matches
    }
}

impl PartialEq for TextMatches {
    fn eq(&self, other: &Self) -> bool {
        self.matches == other.matches
    }
}

impl<'a> IntoIterator for &'a TextMatches {
    type Item = &'a TextMatch;
    type IntoIter = std::slice::Iter<'a, TextMatch>;

    fn into_iter(self) -> Self::IntoIter {
        self.matches.iter()
    }
}

/// A document's text, for the phrases of a search ([`TextIndex::search`]): given to
/// `visit` where document `id` has one, what that takes charged first.
pub(crate) type TextOf<'a> = &'a dyn Fn(u64, &mut dyn FnMut(&str)) -> Result<(), BudgetExceeded>;

/// Whether the words of `text`, as a phrase compares them (lower case; with `stemmer`,
/// stemmed), hold `phrase` in sequence. `active[k]`: whether the last words read end the
/// phrase's first `k + 1` (so no word is read twice). A word's lower case or stem is made
/// only where it differs from the word's ASCII lower case read in place, charged first
/// (by the text's length).
fn holds_phrase(
    text: &str,
    phrase: &[String],
    stemmer: Option<&Stemmer>,
    active: &mut [bool],
    budget: &Arc<Budget>,
) -> Result<bool, BudgetExceeded> {
    let mut charge = Reservation::new(Arc::clone(budget));
    if stemmer.is_some() || !text.is_ascii() {
        charge.charge(text.len() * 12 + 64)?;
    }
    active.fill(false);
    let last = phrase.len() - 1;
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        let normal: Option<String> = match stemmer {
            None if word.is_ascii() => None,
            None => Some(word.to_lowercase()),
            Some(stemmer) => Some(stemmer.stem(&word.to_lowercase()).into_owned()),
        };
        let same = |target: &str| match &normal {
            None => word.eq_ignore_ascii_case(target),
            Some(normal) => normal == target,
        };
        for k in (1..=last).rev() {
            active[k] = active[k - 1] && same(&phrase[k]);
        }
        active[0] = same(&phrase[0]);
        if active[last] {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The text an IRI is found by in autocompletion: its local name (after the last `#`,
/// `/` or `:`) with camel case split into words, and the local name whole
/// (`AlbertEinstein` → `Albert Einstein AlbertEinstein`).
pub(crate) fn local_name_text(iri: &str) -> Option<String> {
    let local = iri.rsplit(['#', '/', ':']).next()?;
    if local.is_empty() {
        return None;
    }
    let mut spaced = String::with_capacity(local.len() * 2);
    let mut previous: Option<char> = None;
    for c in local.chars() {
        if previous.is_some_and(|p| p.is_lowercase() && c.is_uppercase()) {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    let whole: String = local.chars().filter(|c| c.is_alphanumeric()).collect();
    Some(format!("{spaced} {whole}"))
}

/// The words of `text`: maximal runs of letters and digits, lower-cased.
pub fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Words go into shards by their first byte, so that a build fills the shards in parallel
/// and a prefix reads one.
const SHARDS: usize = 256;

/// Documents tokenised per parallel part of a build.
pub(crate) const BUILD_PART: usize = 1 << 14;

/// Fewer documents than this are indexed one at a time, on one thread.
pub(crate) const SERIAL_DOCUMENTS: u64 = 2 * BUILD_PART as u64;

fn shard_of(word: &str) -> usize {
    usize::from(word.as_bytes()[0])
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn get_varint(bytes: &[u8], position: &mut usize) -> u64 {
    let (mut value, mut shift) = (0u64, 0);
    loop {
        let byte = bytes[*position];
        *position += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return value;
        }
        shift += 7;
    }
}

/// A word's documents and occurrences, document numbers ascending: varint deltas, about
/// two bytes a document where a pair of numbers took sixteen.
#[derive(Debug, Default, Clone)]
struct Postings {
    bytes: Vec<u8>,
    last: u64,
    len: u32,
}

impl Postings {
    fn push(&mut self, document: u64, count: u16) {
        let delta = match self.len {
            0 => document,
            _ => document - self.last,
        };
        put_varint(&mut self.bytes, delta);
        put_varint(&mut self.bytes, u64::from(count));
        self.last = document;
        self.len += 1;
    }

    /// `other`'s documents after this one's (all numbered after them).
    fn append(&mut self, other: Postings) {
        if self.len == 0 {
            *self = other;
            return;
        }
        // Its first number is a delta from 0: from this one's last now.
        let mut position = 0;
        let first = get_varint(&other.bytes, &mut position);
        debug_assert!(other.len == 0 || first > self.last);
        if other.len == 0 {
            return;
        }
        put_varint(&mut self.bytes, first - self.last);
        self.bytes.extend_from_slice(&other.bytes[position..]);
        self.last = other.last;
        self.len += other.len;
    }

    /// The documents of `corpus`, with their occurrences, into `out` (with room for
    /// [`Self::bound`]).
    fn decode(&self, corpus: &Corpus, out: &mut Room<'_, (u64, u16)>) {
        // The representation's test hoisted out of the loop: a bit, or a member list walked
        // forward with the postings (both ascending).
        match &corpus.members {
            Members::Bits(bits) => self.decode_where(out, |document| {
                let word = (document / 64) as usize;
                word < bits.len() && bits[word] >> (document % 64) & 1 == 1
            }),
            Members::Sorted(sorted) => {
                let mut at = 0;
                self.decode_where(out, |document| {
                    at += sorted[at..].partition_point(|&member| member < document);
                    sorted.get(at) == Some(&document)
                })
            }
        }
    }

    /// The most documents of `corpus` it has.
    fn bound(&self, corpus: &Corpus) -> usize {
        (self.len as usize).min(corpus.documents as usize)
    }

    /// The documents `keep` takes (asked in ascending order), with their occurrences.
    #[inline]
    fn decode_where(&self, out: &mut Room<'_, (u64, u16)>, mut keep: impl FnMut(u64) -> bool) {
        let (mut position, mut document) = (0, 0u64);
        for _ in 0..self.len {
            document += get_varint(&self.bytes, &mut position);
            let count = get_varint(&self.bytes, &mut position) as u16;
            if keep(document) {
                out.push((document, count));
            }
        }
    }
}

/// Threads for work under a lock ([`scoped`]).
pub(crate) fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// `f` of each of `parts`, each on a thread of its own (scoped, not Rayon's): for work
/// done while a lock is held, since these threads take on no other job, which might wait
/// for that lock.
pub(crate) fn scoped<T: Send, R: Send>(parts: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = parts
            .into_iter()
            .map(|part| scope.spawn(move || f(part)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a scoped part"))
            .collect()
    })
}

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

/// The documents a search ranks among (by number), with the statistics BM25 takes from
/// them: built by the caller from what a reader sees ([`Corpus::finish`]). Its members are
/// held on the budget they were charged to before they were allocated, until it drops.
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
    /// Its documents (the indexed ones: literals with a word).
    pub fn documents(&self) -> u64 {
        self.documents
    }

    /// Its words, counted with repetition.
    pub fn total_words(&self) -> u64 {
        self.total_words
    }

    /// Whether `document` is one of its documents.
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

/// The index. Documents are numbered by the caller (the dictionary's entry numbers), in
/// ascending order as they are added.
#[derive(Debug)]
pub(crate) struct TextIndex {
    /// Dictionary entries looked at so far.
    covered: u64,
    /// Word → its postings, in the shard of its first byte.
    shards: Vec<BTreeMap<Box<str>, Postings>>,
    /// Words per document, by document number from `first` (0: not indexed).
    lengths: Vec<u16>,
    /// The number of `lengths`' first entry: 0, but for a fragment ([`Self::fragment`]).
    first: u64,
    documents: u64,
    total_words: u64,
    /// Distinct words so far: when it changed, the stem maps are stale.
    word_count: usize,
    /// Per stemming language ([`stem_key`]): its stems' words.
    stems: HashMap<String, Stems>,
}

impl Default for TextIndex {
    fn default() -> Self {
        Self {
            covered: 0,
            shards: vec![BTreeMap::new(); SHARDS],
            lengths: Vec::new(),
            first: 0,
            documents: 0,
            total_words: 0,
            word_count: 0,
            stems: HashMap::new(),
        }
    }
}

/// The indexed words by stem, for one language.
#[derive(Debug, Default)]
struct Stems {
    /// [`TextIndex::word_count`] when the map was built.
    covered: usize,
    /// Stem → its words.
    groups: HashMap<Box<str>, Vec<Box<str>>>,
}

/// A document's words with their occurrences, and how many words it has.
fn tokens(text: &str) -> (HashMap<String, u16>, u16) {
    let mut counts: HashMap<String, u16> = HashMap::new();
    let mut length: u16 = 0;
    for word in words(text) {
        let count = counts.entry(word).or_default();
        *count = count.saturating_add(1);
        length = length.saturating_add(1);
    }
    (counts, length)
}

/// The version of [`TextIndex::write`]'s layout.
const TEXT_FORMAT: u32 = 1;

/// Lengths written per piece.
const LENGTH_PIECE: usize = 1 << 16;

impl TextIndex {
    pub(crate) fn covered(&self) -> u64 {
        self.covered
    }

    /// The index as bytes ([`super::derived`]); the stem maps are left out (built again
    /// per language at the first stemmed search).
    pub(crate) fn write<W: std::io::Write>(
        &self,
        out: &mut super::derived::Writer<W>,
    ) -> std::io::Result<()> {
        debug_assert_eq!(self.first, 0, "fragments aren't written");
        out.u32(TEXT_FORMAT)?;
        out.u64(self.covered)?;
        out.u64(self.documents)?;
        out.u64(self.total_words)?;
        out.u64(self.word_count as u64)?;
        out.u64(self.lengths.len() as u64)?;
        for piece in self.lengths.chunks(LENGTH_PIECE) {
            let bytes: Vec<u8> = piece.iter().flat_map(|l| l.to_le_bytes()).collect();
            out.bytes(&bytes)?;
        }
        for shard in &self.shards {
            out.u64(shard.len() as u64)?;
            for (word, postings) in shard {
                out.blob(word.as_bytes())?;
                out.u64(postings.last)?;
                out.u32(postings.len)?;
                out.blob(&postings.bytes)?;
            }
        }
        Ok(())
    }

    /// The index [`Self::write`] wrote; `None` if the bytes aren't one.
    pub(crate) fn read(input: &mut super::derived::Reader<'_>) -> Option<Self> {
        if input.u32()? != TEXT_FORMAT {
            return None;
        }
        let covered = input.u64()?;
        let documents = input.u64()?;
        let total_words = input.u64()?;
        let word_count = usize::try_from(input.u64()?).ok()?;
        let n = input.len(2)?;
        let lengths: Vec<u16> = input
            .bytes(n * 2)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect();
        let mut shards = Vec::with_capacity(SHARDS);
        for _ in 0..SHARDS {
            let words = input.len(1)?;
            let mut shard = BTreeMap::new();
            for _ in 0..words {
                let word = std::str::from_utf8(input.blob()?).ok()?;
                let last = input.u64()?;
                let len = input.u32()?;
                let bytes = input.blob()?.to_vec();
                shard.insert(Box::from(word), Postings { bytes, last, len });
            }
            shards.push(shard);
        }
        input.is_done().then_some(Self {
            covered,
            shards,
            lengths,
            first: 0,
            documents,
            total_words,
            word_count,
            stems: HashMap::new(),
        })
    }

    /// An empty index of the dictionary's entries from `first` on: a fragment, extended
    /// ([`Self::extend`]) and covered apart, for [`Self::absorb`] to take in.
    pub(crate) fn starting_at(first: u64) -> Self {
        Self {
            first,
            covered: first,
            ..Self::default()
        }
    }

    /// Takes in `fragment`, an index of the entries from this one's coverage on (built
    /// apart, [`Self::starting_at`]): its documents' lengths, its words' postings after
    /// theirs, its coverage. Into an empty index, a move; else the shards are merged on
    /// threads of their own ([`scoped`]), since this runs under the index's lock.
    pub(crate) fn absorb(&mut self, mut fragment: TextIndex) {
        debug_assert_eq!(fragment.first, self.covered);
        if self.documents == 0 && self.word_count == 0 {
            // Numbered from 0 again, as a whole index is (and written); with room to grow,
            // as the fragment's own lengths have (one commit's documents later would
            // otherwise copy them all: 80 MB on DBpedia core).
            let lengths = match fragment.first {
                0 => std::mem::take(&mut fragment.lengths),
                first => {
                    let len = first as usize + fragment.lengths.len();
                    let mut lengths = Vec::with_capacity(len + len / 4);
                    lengths.resize(first as usize, 0);
                    lengths.append(&mut fragment.lengths);
                    lengths
                }
            };
            *self = Self {
                lengths,
                first: 0,
                stems: std::mem::take(&mut self.stems),
                ..fragment
            };
            return;
        }
        let end = (fragment.first + fragment.lengths.len() as u64 - self.first) as usize;
        if self.lengths.len() < end {
            self.lengths.resize(end, 0);
        }
        let from = (fragment.first - self.first) as usize;
        self.lengths[from..end].copy_from_slice(&fragment.lengths);
        self.documents += fragment.documents;
        self.total_words += fragment.total_words;
        let per = SHARDS.div_ceil(threads());
        let pairs: Vec<_> = self
            .shards
            .chunks_mut(per)
            .zip(fragment.shards.chunks_mut(per))
            .collect();
        let new_words: usize = scoped(pairs, |(ours, theirs)| {
            let mut new_words = 0;
            for (shard, theirs) in ours.iter_mut().zip(theirs.iter_mut()) {
                for (word, postings) in std::mem::take(theirs) {
                    match shard.get_mut(&word) {
                        Some(ours) => ours.append(postings),
                        None => {
                            shard.insert(word, postings);
                            new_words += 1;
                        }
                    }
                }
            }
            new_words
        })
        .into_iter()
        .sum();
        self.word_count += new_words;
        self.covered = fragment.covered;
    }

    /// Words of document `document` (0: not indexed).
    fn length_of(&self, document: u64) -> u16 {
        document
            .checked_sub(self.first)
            .and_then(|i| self.lengths.get(i as usize))
            .copied()
            .unwrap_or(0)
    }

    fn record_length(&mut self, document: u64, length: u16) {
        let i = (document - self.first) as usize;
        if self.lengths.len() <= i {
            self.lengths.resize(i + 1, 0);
        }
        self.lengths[i] = length;
        self.documents += 1;
        self.total_words += u64::from(length);
    }

    /// Indexes document `document` (after every one added before) holding `text`.
    pub(crate) fn add(&mut self, document: u64, text: &str) {
        let (counts, length) = tokens(text);
        if length == 0 {
            return;
        }
        self.record_length(document, length);
        for (word, count) in counts {
            let shard = &mut self.shards[shard_of(&word)];
            match shard.get_mut(word.as_str()) {
                Some(postings) => postings.push(document, count),
                None => {
                    let mut postings = Postings::default();
                    postings.push(document, count);
                    shard.insert(word.into_boxed_str(), postings);
                    self.word_count += 1;
                }
            }
        }
    }

    /// Indexes `documents` (numbers ascending, after every one added before): tokenised in
    /// parallel parts, then every shard filled in parallel from the parts in order, `part`
    /// run at each part's start. Few documents: one at a time, on this thread.
    pub(crate) fn extend(&mut self, documents: &[(u64, &str)], part: &(dyn Fn() + Sync)) {
        if (documents.len() as u64) < SERIAL_DOCUMENTS {
            for &(document, text) in documents {
                self.add(document, text);
            }
            return;
        }
        type Part = (Vec<(u64, u16)>, Vec<Vec<(Box<str>, u64, u16)>>);
        let parts: Vec<Part> = documents
            .par_chunks(BUILD_PART)
            .map(|chunk| {
                part();
                let mut lengths = Vec::with_capacity(chunk.len());
                let mut by_shard: Vec<Vec<(Box<str>, u64, u16)>> = vec![Vec::new(); SHARDS];
                for &(document, text) in chunk {
                    let (counts, length) = tokens(text);
                    if length == 0 {
                        continue;
                    }
                    lengths.push((document, length));
                    for (word, count) in counts {
                        by_shard[shard_of(&word)].push((word.into_boxed_str(), document, count));
                    }
                }
                (lengths, by_shard)
            })
            .collect();
        for (lengths, _) in &parts {
            for &(document, length) in lengths {
                self.record_length(document, length);
            }
        }
        let new_words: usize = self
            .shards
            .par_iter_mut()
            .enumerate()
            .map(|(s, shard)| {
                if s % 64 == 0 {
                    part();
                }
                let mut new_words = 0;
                for (_, by_shard) in &parts {
                    for (word, document, count) in &by_shard[s] {
                        match shard.get_mut(word.as_ref()) {
                            Some(postings) => postings.push(*document, *count),
                            None => {
                                let mut postings = Postings::default();
                                postings.push(*document, *count);
                                shard.insert(word.clone(), postings);
                                new_words += 1;
                            }
                        }
                    }
                }
                new_words
            })
            .sum();
        self.word_count += new_words;
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

    /// Marks the dictionary entries below `covered` as looked at.
    pub(crate) fn cover(&mut self, covered: u64) {
        self.covered = covered;
    }

    /// Whether the stems of `language` cover every indexed word (or it has no stemmer).
    pub(crate) fn stems_ready(&self, language: &str) -> bool {
        stemmer(language).is_none()
            || self
                .stems
                .get(&stem_key(language))
                .is_some_and(|stems| stems.covered == self.word_count)
    }

    /// Groups the indexed words by their stem in `language` (again, where words came since).
    pub(crate) fn prepare_stems(&mut self, language: &str) {
        let Some(stemmer) = stemmer(language) else {
            return;
        };
        let mut groups: HashMap<Box<str>, Vec<Box<str>>> = HashMap::new();
        for shard in &self.shards {
            for word in shard.keys() {
                groups
                    .entry(stemmer.stem(word).into())
                    .or_default()
                    .push(word.clone());
            }
        }
        self.stems.insert(
            stem_key(language),
            Stems {
                covered: self.word_count,
                groups,
            },
        );
    }

    /// The postings of `word` in `corpus`: of the words it starts as a prefix, of the words
    /// within the edits of a fuzzy match, of the words with its stem with `stems`, else its
    /// own; merged by document (occurrences added up). Charged to `budget` first: the lists
    /// found, and room for the most documents of the corpus each has.
    fn postings_of(
        &self,
        word: &str,
        matching: Match,
        stems: Option<(&Stemmer, &Stems)>,
        corpus: &Corpus,
        budget: &Arc<Budget>,
    ) -> Result<ChargedVec<(u64, u16)>, BudgetExceeded> {
        let mut lists: ChargedVec<&Postings> = ChargedVec::new(Arc::clone(budget));
        match (matching, stems) {
            (Match::Fuzzy(edits), _) => {
                let mut compare = Edits::new(budget);
                for (key, list) in self.shards.iter().flat_map(|shard| shard.iter()) {
                    if compare.within(key, word, usize::from(edits))? {
                        lists.push(list)?;
                    }
                }
            }
            (Match::Prefix, _) => {
                for (_, list) in self.shards[shard_of(word)]
                    .range::<str, _>((std::ops::Bound::Included(word), std::ops::Bound::Unbounded))
                    .take_while(|(key, _)| key.starts_with(word))
                {
                    lists.push(list)?;
                }
            }
            (Match::Exact, Some((stemmer, stems))) => {
                let found = stems.groups.get(stemmer.stem(word).as_ref());
                for word in found.into_iter().flatten() {
                    if let Some(list) = self.shards[shard_of(word)].get(word) {
                        lists.push(list)?;
                    }
                }
            }
            (Match::Exact, None) => {
                if let Some(list) = self.shards[shard_of(word)].get(word) {
                    lists.push(list)?;
                }
            }
        }
        let bound = lists.iter().map(|list| list.bound(corpus)).sum();
        let mut merged = ChargedVec::with_capacity(Arc::clone(budget), bound)?;
        merged.fill(bound, |room| {
            for list in lists.iter() {
                list.decode(corpus, room);
            }
        })?;
        if lists.len() > 1 {
            merged.sort_unstable_by_key(|&(id, _)| id);
            // Repeats of a document as one, their occurrences added up, in place.
            let mut len = 0;
            for i in 0..merged.len() {
                match len > 0 && merged[len - 1].0 == merged[i].0 {
                    true => merged[len - 1].1 = merged[len - 1].1.saturating_add(merged[i].1),
                    false => {
                        merged[len] = merged[i];
                        len += 1;
                    }
                }
            }
            merged.truncate(len);
        }
        Ok(merged)
    }

    /// Words per document (1 where unknown).
    fn length(&self, document: u64) -> f64 {
        match self.length_of(document) {
            0 => 1.0,
            length => f64::from(length),
        }
    }

    /// The literals of `corpus` matching `query`, best first (ties by id), scored with the
    /// corpus's statistics; `text_of` gives a literal's text, to check phrases. Everything
    /// it holds is charged to `budget` before it is allocated: the query's words (within a
    /// bound from its length), each word's postings in the corpus, the phrases' checks, the
    /// matches (returned with their charge). Scores are summed by a merge of the words'
    /// and phrases' lists, each in document order: no table of scores.
    pub(crate) fn search(
        &self,
        query: &TextQuery,
        corpus: &Corpus,
        text_of: TextOf<'_>,
        budget: &Arc<Budget>,
    ) -> Result<TextMatches, BudgetExceeded> {
        let documents = corpus.documents as f64;
        if documents == 0.0 {
            return Ok(TextMatches::empty(budget));
        }
        let average = corpus.total_words as f64 / documents;
        let (k1, b) = (1.2, 0.75);
        // The query's words, parsed: bounded by its length (every word lower-cased, at most
        // three bytes for two; lists of them; their stems).
        let mut parsing = Reservation::new(Arc::clone(budget));
        parsing.charge(query.text.len() * 160 + 1024)?;
        // Stemming, where the language has a stemmer and its stems are prepared.
        let stemming = query
            .stem
            .as_deref()
            .and_then(|language| Some((stemmer(language)?, self.stems.get(&stem_key(language))?)));
        let stems = stemming.as_ref().map(|(stemmer, stems)| (stemmer, *stems));
        // Words compared in phrases: their stems when stemming.
        let normal = |word: &str| -> String {
            match &stemming {
                Some((stemmer, _)) => stemmer.stem(word).into_owned(),
                None => word.to_owned(),
            }
        };
        // Quoted parts are phrases; the rest are words, and the words to exclude.
        let mut terms: Vec<(String, Match)> = Vec::new();
        let mut excluded_words: Vec<String> = Vec::new();
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
                    excluded_words.extend(words(rest));
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
        if terms.is_empty() && phrases.is_empty() {
            return Ok(TextMatches::empty(budget));
        }
        let score = |idf: f64, id: u64, count: u16| {
            let length = self.length(id);
            let tf = f64::from(count);
            idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * length / average))
        };
        let idf = |frequency: usize| {
            let frequency = frequency as f64;
            (1.0 + (documents - frequency + 0.5) / (frequency + 0.5)).ln()
        };
        // The lists merged below, each in document order: a word's postings, or a phrase's
        // literals with their score.
        let mut words_found: ChargedVec<(ChargedVec<(u64, u16)>, f64)> =
            ChargedVec::with_capacity(Arc::clone(budget), terms.len())?;
        let mut phrases_found: ChargedVec<ChargedVec<(u64, f64)>> =
            ChargedVec::with_capacity(Arc::clone(budget), phrases.len())?;
        for phrase in &phrases {
            let mut lists: ChargedVec<ChargedVec<(u64, u16)>> =
                ChargedVec::with_capacity(Arc::clone(budget), phrase.len())?;
            for word in phrase {
                lists.push(self.postings_of(word, Match::Exact, stems, corpus, budget)?)?;
            }
            if lists.iter().any(|list| list.is_empty()) {
                continue;
            }
            let idfs: ChargedVec<f64> = ChargedVec::collect(
                Arc::clone(budget),
                lists.len(),
                lists.iter().map(|list| idf(list.len())),
            )?;
            let phrase: Vec<String> = phrase.iter().map(|word| normal(word)).collect();
            // The literals with every word of the phrase, then with them in sequence.
            let rarest = lists
                .iter()
                .min_by_key(|list| list.len())
                .expect("two words");
            let mut found: ChargedVec<(u64, f64)> = ChargedVec::new(Arc::clone(budget));
            let mut active: ChargedVec<bool> = ChargedVec::new(Arc::clone(budget));
            active.resize(phrase.len(), false)?;
            let stemmer = stemming.as_ref().map(|(stemmer, _)| stemmer);
            for &(id, _) in rarest.iter() {
                let mut total = 0.0;
                let in_all = lists.iter().zip(idfs.iter()).all(|(list, &idf)| {
                    match list.binary_search_by_key(&id, |&(i, _)| i) {
                        Ok(at) => {
                            total += score(idf, id, list[at].1);
                            true
                        }
                        Err(_) => false,
                    }
                });
                if !in_all {
                    continue;
                }
                let mut held = Ok(false);
                text_of(id, &mut |text| {
                    held = holds_phrase(text, &phrase, stemmer, &mut active, budget);
                })?;
                if held? {
                    found.push((id, total))?;
                }
            }
            phrases_found.push(found)?;
        }
        for (word, matching) in &terms {
            let list = self.postings_of(word, *matching, stems, corpus, budget)?;
            let idf = idf(list.len());
            words_found.push((list, idf))?;
        }
        let needed = if query.all_words {
            terms.len() + phrases.len()
        } else {
            1
        };
        let mut excluded: ChargedVec<u64> = ChargedVec::new(Arc::clone(budget));
        for word in &excluded_words {
            let list = self.postings_of(word, Match::Exact, stems, corpus, budget)?;
            excluded.extend_bounded(list.len(), list.iter().map(|&(id, _)| id))?;
        }
        excluded.sort_unstable();
        // The merge: the next document of any list, its score summed over the lists that
        // have it, kept where enough of the query's terms have it and no excluded word.
        let mut word_at: ChargedVec<usize> =
            ChargedVec::with_capacity(Arc::clone(budget), words_found.len())?;
        word_at.resize(words_found.len(), 0)?;
        let mut phrase_at: ChargedVec<usize> =
            ChargedVec::with_capacity(Arc::clone(budget), phrases_found.len())?;
        phrase_at.resize(phrases_found.len(), 0)?;
        let mut excluded_at = 0;
        let mut matches: ChargedVec<TextMatch> = ChargedVec::new(Arc::clone(budget));
        loop {
            let next_word = words_found
                .iter()
                .zip(word_at.iter())
                .filter_map(|((list, _), &at)| list.get(at).map(|&(id, _)| id));
            let next_phrase = phrases_found
                .iter()
                .zip(phrase_at.iter())
                .filter_map(|(list, &at)| list.get(at).map(|&(id, _)| id));
            let Some(id) = next_word.chain(next_phrase).min() else {
                break;
            };
            // Summed in the query's order: its phrases, then its words.
            let (mut total, mut have) = (0.0, 0);
            for (list, at) in phrases_found.iter().zip(phrase_at.iter_mut()) {
                if let Some(&(document, phrase_score)) = list.get(*at)
                    && document == id
                {
                    total += phrase_score;
                    have += 1;
                    *at += 1;
                }
            }
            for ((list, idf), at) in words_found.iter().zip(word_at.iter_mut()) {
                if let Some(&(document, count)) = list.get(*at)
                    && document == id
                {
                    total += score(*idf, id, count);
                    have += 1;
                    *at += 1;
                }
            }
            excluded_at += excluded[excluded_at..].partition_point(|&e| e < id);
            if have >= needed && excluded.get(excluded_at) != Some(&id) {
                matches.push(TextMatch {
                    id,
                    relevance: total,
                })?;
            }
        }
        let best = matches
            .iter()
            .map(|m| m.relevance)
            .fold(f64::MIN_POSITIVE, f64::max);
        for m in matches.iter_mut() {
            m.relevance /= best;
        }
        matches.sort_unstable_by(|a, b| {
            b.relevance
                .total_cmp(&a.relevance)
                .then_with(|| a.id.cmp(&b.id))
        });
        // What the parse and the lists hold goes before the matches leave.
        drop((words_found, phrases_found, excluded, word_at, phrase_at));
        drop((terms, excluded_words, phrases));
        drop(parsing);
        let (matches, charge) = matches.into_parts();
        Ok(TextMatches {
            matches,
            _charge: charge,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(texts: &[&str]) -> TextIndex {
        let mut index = TextIndex::default();
        for (i, text) in texts.iter().enumerate() {
            index.add(i as u64, text);
        }
        index
    }

    /// `index`'s search, with the texts `text_of` gives, unbounded.
    fn search(
        index: &TextIndex,
        query: &TextQuery,
        corpus: &Corpus,
        text_of: &dyn Fn(u64) -> Option<String>,
    ) -> TextMatches {
        let budget = Arc::new(Budget::unlimited());
        let text_of = |id: u64, visit: &mut dyn FnMut(&str)| {
            if let Some(text) = text_of(id) {
                visit(&text);
            }
            Ok(())
        };
        index
            .search(query, corpus, &text_of, &budget)
            .expect("no budget to exceed")
    }

    fn ids(matches: &[TextMatch]) -> Vec<u64> {
        matches.iter().map(|m| m.id).collect()
    }

    /// The bits of `members` below `span`.
    fn bits(members: &[u64], span: u64) -> Vec<u64> {
        let mut bits = vec![0u64; span.div_ceil(64) as usize];
        for &m in members {
            bits[(m / 64) as usize] |= 1 << (m % 64);
        }
        bits
    }

    /// The corpus of `bits` (a bit per number below `span`), unbounded.
    fn corpus_of(index: &TextIndex, bits: Vec<u64>, span: u64) -> Corpus {
        let budget = Arc::new(Budget::unlimited());
        let mut charged = ChargedVec::with_capacity(Arc::clone(&budget), bits.len()).unwrap();
        charged.extend_from_slice(&bits).unwrap();
        Corpus::finish(&index.corpus(charged), span, &budget).unwrap()
    }

    /// Every document the index holds.
    fn every(index: &TextIndex) -> Corpus {
        let span = index.lengths.len() as u64;
        corpus_of(index, vec![u64::MAX; span.div_ceil(64) as usize], span)
    }

    #[test]
    fn words_prefixes_all_words_and_ranking() {
        let texts = [
            "The quick brown fox",
            "A brown dog",
            "Fox, fox and FOX",
            "Überschrift über Füchse",
            "nothing here",
            "brown, quick: the fox",
        ];
        let index = index(&texts);
        let corpus = every(&index);
        let text_of = |id: u64| texts.get(id as usize).map(|t| (*t).to_owned());
        let query = |text: &str, all_words: bool, prefix: bool| {
            search(
                &index,
                &TextQuery {
                    text: text.to_owned(),
                    all_words,
                    prefix,
                    stem: None,
                },
                &corpus,
                &text_of,
            )
        };
        // A phrase: its words in sequence (punctuation between words doesn't count).
        assert_eq!(ids(&query("\"quick brown\"", false, false)), [0]);
        assert_eq!(ids(&query("\"brown quick\"", false, false)), [5]);
        assert_eq!(
            ids(&query("\"quick brown\" dog", true, false)),
            Vec::<u64>::new()
        );
        let mut either = ids(&query("\"quick brown\" dog", false, false));
        either.sort_unstable();
        assert_eq!(either, [0, 1]);
        // The literal with fox three times in three words ranks first, at relevance 1.
        let fox = query("fox", false, false);
        assert_eq!(ids(&fox), [2, 0, 5]);
        assert_eq!(fox[0].relevance, 1.0);
        assert!(fox[1].relevance < 1.0 && fox[1].relevance > 0.0);
        // Any word, or all of them.
        let mut any = ids(&query("brown fox", false, false));
        any.sort_unstable();
        assert_eq!(any, [0, 1, 2, 5]);
        assert_eq!(ids(&query("brown fox", true, false)), [0, 5]);
        // Prefixes, by `*` or for every word; case and non-ASCII letters.
        assert_eq!(ids(&query("qui*", false, false)), [0, 5]);
        assert!(query("qui", false, false).is_empty());
        assert_eq!(ids(&query("qu", false, true)), [0, 5]);
        assert_eq!(ids(&query("ÜBER", false, false)), [3]);
        assert_eq!(ids(&query("füchs*", false, false)), [3]);
        assert!(query("", false, false).is_empty());
        assert!(query("cat", false, false).is_empty());
    }

    /// With a stemming language, a word finds the words with its stem, in phrases too;
    /// prefixes stay prefixes, and a language without a stemmer searches as written.
    #[test]
    fn stemmed_words_and_phrases() {
        let texts = [
            "Connected systems",
            "The connection of systems",
            "connecting a system",
            "Les chevaux sont connectés",
            "unrelated",
        ];
        let mut index = index(&texts);
        let text_of = |id: u64| texts.get(id as usize).map(|t| (*t).to_owned());
        assert!(!index.stems_ready("en"));
        index.prepare_stems("en");
        assert!(index.stems_ready("en-GB"));
        assert!(index.stems_ready("xx"));
        let query = |index: &TextIndex, text: &str, stem: Option<&str>| {
            let mut found = ids(&search(
                index,
                &TextQuery {
                    text: text.to_owned(),
                    all_words: false,
                    prefix: false,
                    stem: stem.map(str::to_owned),
                },
                &every(index),
                &text_of,
            ));
            found.sort_unstable();
            found
        };
        assert_eq!(query(&index, "connect", None), Vec::<u64>::new());
        assert_eq!(query(&index, "connect", Some("en")), [0, 1, 2]);
        assert_eq!(query(&index, "connections", Some("en")), [0, 1, 2]);
        assert_eq!(query(&index, "\"connected system\"", Some("en")), [0]);
        assert_eq!(query(&index, "\"connecting systems\"", Some("en")), [0]);
        assert_eq!(
            query(&index, "\"connected system\"", None),
            Vec::<u64>::new()
        );
        assert_eq!(query(&index, "connect", Some("xx")), Vec::<u64>::new());
        // Words indexed later are stemmed at the next preparation.
        index.add(9, "reconnecting systems");
        assert!(!index.stems_ready("en"));
        index.prepare_stems("en");
        assert_eq!(query(&index, "system", Some("en")), [0, 1, 2, 9]);
        // French has its own stemmer: `connectés` and `connection` stem to `connect`.
        index.prepare_stems("fr");
        assert_eq!(query(&index, "connecté", None), Vec::<u64>::new());
        assert_eq!(query(&index, "connecté", Some("fr")), [1, 3]);
    }

    #[test]
    fn local_names_split_for_autocompletion() {
        assert_eq!(
            local_name_text("http://dbpedia.org/resource/Albert_Einstein").as_deref(),
            Some("Albert_Einstein AlbertEinstein")
        );
        assert_eq!(
            local_name_text("http://example.com/onto#hasPart").as_deref(),
            Some("has Part hasPart")
        );
        assert_eq!(local_name_text("urn:isbn:123").as_deref(), Some("123 123"));
        assert_eq!(local_name_text("http://example.com/"), None);
    }

    /// `-word` excludes the literals that have it; `word~` matches words within two edits,
    /// `word~1` within one.
    #[test]
    fn exclusion_and_fuzzy_words() {
        let texts = [
            "the quick brown fox",
            "a brown dog",
            "the quack of a duck",
            "quickly",
        ];
        let index = index(&texts);
        let corpus = every(&index);
        let text_of = |id: u64| texts.get(id as usize).map(|t| (*t).to_owned());
        let query = |text: &str| {
            let mut found = ids(&search(
                &index,
                &TextQuery {
                    text: text.to_owned(),
                    all_words: false,
                    prefix: false,
                    stem: None,
                },
                &corpus,
                &text_of,
            ));
            found.sort_unstable();
            found
        };
        assert_eq!(query("brown -fox"), [1]);
        assert_eq!(query("brown -fox -dog"), Vec::<u64>::new());
        assert_eq!(query("-fox"), Vec::<u64>::new());
        assert_eq!(query("quick~1"), [0, 2]);
        // Two edits: `quickly` (two letters more) and `duck` too.
        assert_eq!(query("quick~"), [0, 2, 3]);
        assert_eq!(query("quick~0"), [0]);
        assert_eq!(query("quik~1"), [0]);
        assert!(within_edits("kitten", "sitting", 3));
        assert!(!within_edits("kitten", "sitting", 2));
        assert!(within_edits("über", "uber", 1));
        assert!(within_edits("brigde", "bridge", 1));
    }

    /// A build in parallel parts gives the same answers as one document at a time.
    #[test]
    fn parallel_builds_equal_sequential_ones() {
        let texts: Vec<String> = (0..70_000u64)
            .map(|i| {
                format!(
                    "word{} common w{} {}",
                    i % 1000,
                    i % 7,
                    if i % 3 == 0 { "third" } else { "" }
                )
            })
            .collect();
        let documents: Vec<(u64, &str)> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| (i as u64 * 2, text.as_str()))
            .collect();
        let mut parallel = TextIndex::default();
        parallel.extend(&documents, &|| {});
        let mut sequential = TextIndex::default();
        for &(document, text) in &documents {
            sequential.add(document, text);
        }
        let text_of = |id: u64| texts.get(id as usize / 2).cloned();
        for text in [
            "word7",
            "w3 third",
            "\"common w3\"",
            "word99*",
            "commn~1",
            "third -w2",
        ] {
            let query = TextQuery {
                text: text.to_owned(),
                all_words: true,
                prefix: false,
                stem: None,
            };
            let a = search(&parallel, &query, &every(&parallel), &text_of);
            let b = search(&sequential, &query, &every(&sequential), &text_of);
            assert!(!a.is_empty(), "{text}");
            assert_eq!(a, b, "{text}");
        }
        assert_eq!(parallel.word_count, sequential.word_count);
    }

    /// An index extended by a fragment built apart ([`TextIndex::absorb`]) answers as one
    /// built document by document: postings continue across the seam, lengths and counts
    /// add up, words new to the fragment are found.
    #[test]
    fn an_absorbed_fragment_equals_documents_added_one_by_one() {
        let texts: Vec<String> = (0..80_000u64)
            .map(|i| {
                format!(
                    "word{} common w{} {}",
                    i % 1000,
                    i % 7,
                    if i >= 50_000 { "late" } else { "" }
                )
            })
            .collect();
        let documents: Vec<(u64, &str)> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| (i as u64 + 3, text.as_str()))
            .collect();
        let mut sequential = TextIndex::default();
        for &(document, text) in &documents {
            sequential.add(document, text);
        }
        sequential.cover(80_003);
        let (early, late) = documents.split_at(10_000);
        let mut absorbed = TextIndex::default();
        absorbed.extend(early, &|| {});
        absorbed.cover(10_003);
        let parts = std::sync::atomic::AtomicUsize::new(0);
        let count = || {
            parts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        };
        // Into an empty index, a move that keeps room to grow.
        let mut moved = TextIndex::default();
        let mut whole = TextIndex::starting_at(0);
        whole.extend(&documents, &|| {});
        whole.cover(80_003);
        moved.absorb(whole);
        assert!(moved.lengths.capacity() > moved.lengths.len());
        assert_eq!(moved.documents, sequential.documents);
        let mut fragment = TextIndex::starting_at(10_003);
        fragment.extend(late, &count);
        fragment.cover(80_003);
        absorbed.absorb(fragment);
        assert!(parts.into_inner() > 1, "built in parallel parts");
        assert_eq!(
            (
                absorbed.covered,
                absorbed.documents,
                absorbed.total_words,
                absorbed.word_count
            ),
            (
                sequential.covered,
                sequential.documents,
                sequential.total_words,
                sequential.word_count
            )
        );
        let text_of = |id: u64| texts.get(id as usize - 3).cloned();
        for text in [
            "word7",
            "w3 late",
            "\"common w3\"",
            "word99*",
            "commn~1",
            "late -w2",
        ] {
            let query = TextQuery {
                text: text.to_owned(),
                all_words: true,
                prefix: false,
                stem: None,
            };
            let a = search(&absorbed, &query, &every(&absorbed), &text_of);
            let b = search(&sequential, &query, &every(&sequential), &text_of);
            assert!(!a.is_empty(), "{text}");
            assert_eq!(a, b, "{text}");
        }
    }

    /// A search in a corpus scores as an index of the corpus's documents alone would: the
    /// other documents change no relevance and no order, in either representation.
    #[test]
    fn a_corpus_scores_as_its_documents_alone() {
        let texts = [
            "alpha one two",
            "beta",
            "beta extra",
            "beta more extra",
            "gamma alpha",
            "filler",
            "\"quoted\" alpha beta",
            "beta three",
            "beta four",
            "beta five",
        ];
        let kept = [0u64, 1, 4, 5, 6];
        let whole = index(&texts);
        let mut alone = TextIndex::default();
        for &i in &kept {
            alone.add(i, texts[i as usize]);
        }
        let text_of = |id: u64| texts.get(id as usize).map(|t| (*t).to_owned());
        let sparse = corpus_of(&whole, bits(&kept, 1 << 20), 1 << 20);
        let dense = corpus_of(&whole, bits(&kept, texts.len() as u64), texts.len() as u64);
        assert!(matches!(sparse.members, Members::Sorted(_)));
        assert!(matches!(dense.members, Members::Bits(_)));
        assert_eq!((dense.documents(), dense.total_words()), (5, 10));
        for text in [
            "alpha beta",
            "beta",
            "\"alpha beta\" gamma",
            "ext*",
            "bet~1",
        ] {
            let query = TextQuery {
                text: text.to_owned(),
                all_words: false,
                prefix: false,
                stem: None,
            };
            let expected = search(&alone, &query, &every(&alone), &text_of);
            assert_eq!(
                search(&whole, &query, &sparse, &text_of),
                expected,
                "{text}"
            );
            assert_eq!(search(&whole, &query, &dense, &text_of), expected, "{text}");
        }
        // Without the corpus's restriction, `beta`'s other literals put `alpha` ahead.
        let query = TextQuery {
            text: "alpha beta".to_owned(),
            all_words: false,
            prefix: false,
            stem: None,
        };
        let place = |found: &[TextMatch], id: u64| found.iter().position(|m| m.id == id);
        let kept_order = search(&whole, &query, &dense, &text_of);
        assert!(place(&kept_order, 1) < place(&kept_order, 0));
        let every_order = search(&whole, &query, &every(&whole), &text_of);
        assert!(place(&every_order, 0) < place(&every_order, 1));
    }
}
