//! Full-text index over the dictionary's string literals.
//!
//! An inverted index from lower-cased words to the literals (simple and language-tagged
//! strings) that contain them. It is built the first time a query searches, and extended
//! with the terms the dictionary gained since before each later search: the dictionary is
//! append-only, so nothing indexed ever changes. Whether a literal still occurs in the data
//! is the query's business (it joins the matches with the statements).
//!
//! Scores are BM25 (k1 = 1.2, b = 0.75) summed over the query's words and divided by the
//! best match's, so relevance lies in (0, 1] as Blazegraph's does.
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

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

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
fn within_edits(a: &str, b: &str, max: usize) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > max {
        return false;
    }
    let mut before: Vec<usize> = Vec::new();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 0..a.len() {
        let mut next = vec![i + 1; b.len() + 1];
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
            return false;
        }
        before = std::mem::replace(&mut row, next);
    }
    row[b.len()] <= max
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

#[derive(Debug, Default)]
pub(crate) struct TextIndex {
    /// Dictionary entries looked at so far.
    covered: u64,
    /// Word → (literal id, occurrences), ids ascending.
    postings: BTreeMap<Box<str>, Vec<(u64, u16)>>,
    /// Words per indexed literal.
    lengths: HashMap<u64, u16>,
    total_words: u64,
    /// The distinct words in the order they were first indexed: what the stem maps cover.
    words: Vec<Box<str>>,
    /// Per stemming language ([`stem_key`]): its stems' words.
    stems: HashMap<String, Stems>,
}

/// The indexed words by stem, for one language.
#[derive(Debug, Default)]
struct Stems {
    /// Words (of [`TextIndex::words`]) looked at so far.
    covered: usize,
    /// Stem → the indexes of its words in [`TextIndex::words`].
    groups: HashMap<Box<str>, Vec<u32>>,
}

impl TextIndex {
    pub(crate) fn covered(&self) -> u64 {
        self.covered
    }

    /// Indexes the dictionary entry `index` (id `id`) holding the string `text`.
    pub(crate) fn add(&mut self, id: u64, text: &str) {
        let mut counts: HashMap<String, u16> = HashMap::new();
        let mut length: u16 = 0;
        for word in words(text) {
            let count = counts.entry(word).or_default();
            *count = count.saturating_add(1);
            length = length.saturating_add(1);
        }
        if length == 0 {
            return;
        }
        for (word, count) in counts {
            match self.postings.entry(word.into_boxed_str()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    self.words.push(entry.key().clone());
                    entry.insert(vec![(id, count)]);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().push((id, count));
                }
            }
        }
        self.lengths.insert(id, length);
        self.total_words += u64::from(length);
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
                .is_some_and(|stems| stems.covered == self.words.len())
    }

    /// Groups the words indexed since the last call by their stem in `language`.
    pub(crate) fn prepare_stems(&mut self, language: &str) {
        let Some(stemmer) = stemmer(language) else {
            return;
        };
        let stems = self.stems.entry(stem_key(language)).or_default();
        for (i, word) in self.words.iter().enumerate().skip(stems.covered) {
            stems
                .groups
                .entry(stemmer.stem(word).into())
                .or_default()
                .push(i as u32);
        }
        stems.covered = self.words.len();
    }

    /// The postings of `word`: of the words it starts as a prefix, of the words within the
    /// edits of a fuzzy match, of the words with its stem with `stems`, else its own; merged
    /// by literal (occurrences added up).
    fn postings_of(
        &self,
        word: &str,
        matching: Match,
        stems: Option<(&Stemmer, &Stems)>,
    ) -> Cow<'_, [(u64, u16)]> {
        let lists: Vec<&Vec<(u64, u16)>> = match (matching, stems) {
            (Match::Fuzzy(edits), _) => self
                .postings
                .iter()
                .filter(|(key, _)| within_edits(key, word, usize::from(edits)))
                .map(|(_, list)| list)
                .collect(),
            (Match::Prefix, _) => self
                .postings
                .range::<str, _>((std::ops::Bound::Included(word), std::ops::Bound::Unbounded))
                .take_while(|(key, _)| key.starts_with(word))
                .map(|(_, list)| list)
                .collect(),
            (Match::Exact, Some((stemmer, stems))) => stems
                .groups
                .get(stemmer.stem(word).as_ref())
                .into_iter()
                .flatten()
                .filter_map(|&i| self.postings.get(&self.words[i as usize]))
                .collect(),
            (Match::Exact, None) => self.postings.get(word).into_iter().collect(),
        };
        match lists.as_slice() {
            [] => Cow::Borrowed(&[]),
            [list] => Cow::Borrowed(list.as_slice()),
            _ => {
                let mut merged: Vec<(u64, u16)> = lists.into_iter().flatten().copied().collect();
                merged.sort_unstable_by_key(|&(id, _)| id);
                merged.dedup_by(|later, kept| {
                    let same = later.0 == kept.0;
                    if same {
                        kept.1 = kept.1.saturating_add(later.1);
                    }
                    same
                });
                Cow::Owned(merged)
            }
        }
    }

    /// The literals matching `query`, best first (ties by id); `text_of` gives a literal's
    /// text, to check phrases.
    pub(crate) fn search(
        &self,
        query: &TextQuery,
        text_of: &dyn Fn(u64) -> Option<String>,
    ) -> Vec<TextMatch> {
        let documents = self.lengths.len() as f64;
        if documents == 0.0 {
            return Vec::new();
        }
        let average = self.total_words as f64 / documents;
        let (k1, b) = (1.2, 0.75);
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
        if terms.is_empty() && phrases.is_empty() {
            return Vec::new();
        }
        // Per literal: its score, and how many of the query's terms it has.
        let mut scores: HashMap<u64, (f64, usize)> = HashMap::new();
        let bm25 = |list: &[(u64, u16)], out: &mut HashMap<u64, f64>| {
            let frequency = list.len() as f64;
            let idf = (1.0 + (documents - frequency + 0.5) / (frequency + 0.5)).ln();
            for &(id, count) in list {
                let length = f64::from(self.lengths.get(&id).copied().unwrap_or(1));
                let tf = f64::from(count);
                *out.entry(id).or_default() +=
                    idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * length / average));
            }
        };
        for phrase in &phrases {
            let lists: Vec<Cow<'_, [(u64, u16)]>> = phrase
                .iter()
                .map(|word| self.postings_of(word, Match::Exact, stems))
                .collect();
            if lists.iter().any(|list| list.is_empty()) {
                continue;
            }
            let mut phrase_scores: HashMap<u64, f64> = HashMap::new();
            for list in &lists {
                bm25(list, &mut phrase_scores);
            }
            let phrase: Vec<String> = phrase.iter().map(|word| normal(word)).collect();
            // The literals with every word of the phrase, then with them in sequence.
            let rarest = lists
                .iter()
                .min_by_key(|list| list.len())
                .expect("two words");
            for &(id, _) in rarest.iter() {
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
            let found = self.postings_of(word, *matching, stems);
            let frequency = found.len() as f64;
            let idf = (1.0 + (documents - frequency + 0.5) / (frequency + 0.5)).ln();
            for &(id, count) in found.iter() {
                let length = f64::from(self.lengths.get(&id).copied().unwrap_or(1));
                let tf = f64::from(count);
                let score = idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * length / average));
                let entry = scores.entry(id).or_default();
                entry.0 += score;
                entry.1 += 1;
            }
        }
        let needed = if query.all_words {
            terms.len() + phrases.len()
        } else {
            1
        };
        let excluded: std::collections::HashSet<u64> = excluded
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

    fn ids(matches: &[TextMatch]) -> Vec<u64> {
        matches.iter().map(|m| m.id).collect()
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
        let text_of = |id: u64| texts.get(id as usize).map(|t| (*t).to_owned());
        let query = |text: &str, all_words: bool, prefix: bool| {
            index.search(
                &TextQuery {
                    text: text.to_owned(),
                    all_words,
                    prefix,
                    stem: None,
                },
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
            let mut found = ids(&index.search(
                &TextQuery {
                    text: text.to_owned(),
                    all_words: false,
                    prefix: false,
                    stem: stem.map(str::to_owned),
                },
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
        let text_of = |id: u64| texts.get(id as usize).map(|t| (*t).to_owned());
        let query = |text: &str| {
            let mut found = ids(&index.search(
                &TextQuery {
                    text: text.to_owned(),
                    all_words: false,
                    prefix: false,
                    stem: None,
                },
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
}
