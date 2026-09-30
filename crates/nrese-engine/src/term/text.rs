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

use std::collections::{BTreeMap, HashMap};

/// What to search for.
#[derive(Debug, Clone, PartialEq)]
pub struct TextQuery {
    /// The words to find; a word ending in `*` matches every word it starts.
    pub text: String,
    /// Every word must occur (else any word may).
    pub all_words: bool,
    /// Every word matches as a prefix.
    pub prefix: bool,
}

/// A literal that matched, with its relevance (the best match has 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMatch {
    pub id: u64,
    pub relevance: f64,
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
            self.postings
                .entry(word.into_boxed_str())
                .or_default()
                .push((id, count));
        }
        self.lengths.insert(id, length);
        self.total_words += u64::from(length);
    }

    /// Marks the dictionary entries below `covered` as looked at.
    pub(crate) fn cover(&mut self, covered: u64) {
        self.covered = covered;
    }

    /// The literals matching `query`, best first (ties by id).
    pub(crate) fn search(&self, query: &TextQuery) -> Vec<TextMatch> {
        let documents = self.lengths.len() as f64;
        if documents == 0.0 {
            return Vec::new();
        }
        let average = self.total_words as f64 / documents;
        let (k1, b) = (1.2, 0.75);
        let mut terms: Vec<(String, bool)> = Vec::new();
        for raw in query.text.split_whitespace() {
            let prefix = query.prefix || raw.ends_with('*');
            for word in words(raw) {
                if !terms.iter().any(|(w, p)| *w == word && *p == prefix) {
                    terms.push((word, prefix));
                }
            }
        }
        if terms.is_empty() {
            return Vec::new();
        }
        // Per literal: its score, and how many of the query's words it has.
        let mut scores: HashMap<u64, (f64, usize)> = HashMap::new();
        for (word, prefix) in &terms {
            let mut found: HashMap<u64, u16> = HashMap::new();
            let lists: Vec<&Vec<(u64, u16)>> = if *prefix {
                self.postings
                    .range::<str, _>((
                        std::ops::Bound::Included(word.as_str()),
                        std::ops::Bound::Unbounded,
                    ))
                    .take_while(|(key, _)| key.starts_with(word.as_str()))
                    .map(|(_, list)| list)
                    .collect()
            } else {
                self.postings.get(word.as_str()).into_iter().collect()
            };
            for list in lists {
                for &(id, count) in list {
                    let entry = found.entry(id).or_default();
                    *entry = entry.saturating_add(count);
                }
            }
            let frequency = found.len() as f64;
            let idf = (1.0 + (documents - frequency + 0.5) / (frequency + 0.5)).ln();
            for (id, count) in found {
                let length = f64::from(self.lengths.get(&id).copied().unwrap_or(1));
                let tf = f64::from(count);
                let score = idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * length / average));
                let entry = scores.entry(id).or_default();
                entry.0 += score;
                entry.1 += 1;
            }
        }
        let needed = if query.all_words { terms.len() } else { 1 };
        let mut matches: Vec<TextMatch> = scores
            .into_iter()
            .filter(|(_, (_, words))| *words >= needed)
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
        let index = index(&[
            "The quick brown fox",
            "A brown dog",
            "Fox, fox and FOX",
            "Überschrift über Füchse",
            "nothing here",
        ]);
        let query = |text: &str, all_words: bool, prefix: bool| {
            index.search(&TextQuery {
                text: text.to_owned(),
                all_words,
                prefix,
            })
        };
        // The literal with fox three times in three words ranks first, at relevance 1.
        let fox = query("fox", false, false);
        assert_eq!(ids(&fox), [2, 0]);
        assert_eq!(fox[0].relevance, 1.0);
        assert!(fox[1].relevance < 1.0 && fox[1].relevance > 0.0);
        // Any word, or all of them.
        let mut any = ids(&query("brown fox", false, false));
        any.sort_unstable();
        assert_eq!(any, [0, 1, 2]);
        assert_eq!(ids(&query("brown fox", true, false)), [0]);
        // Prefixes, by `*` or for every word; case and non-ASCII letters.
        assert_eq!(ids(&query("qui*", false, false)), [0]);
        assert!(query("qui", false, false).is_empty());
        assert_eq!(ids(&query("qu", false, true)), [0]);
        assert_eq!(ids(&query("ÜBER", false, false)), [3]);
        assert_eq!(ids(&query("füchs*", false, false)), [3]);
        assert!(query("", false, false).is_empty());
        assert!(query("cat", false, false).is_empty());
    }
}
