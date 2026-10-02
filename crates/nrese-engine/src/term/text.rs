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

use std::collections::{BTreeMap, HashMap};

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
        // Quoted parts are phrases; the rest are words.
        let mut terms: Vec<(String, bool)> = Vec::new();
        let mut phrases: Vec<Vec<String>> = Vec::new();
        for (i, part) in query.text.split('"').enumerate() {
            if i % 2 == 1 {
                let phrase: Vec<String> = words(part).collect();
                match phrase.len() {
                    0 => {}
                    1 => terms.push((phrase[0].clone(), false)),
                    _ => phrases.push(phrase),
                }
                continue;
            }
            for raw in part.split_whitespace() {
                let prefix = query.prefix || raw.ends_with('*');
                for word in words(raw) {
                    if !terms.iter().any(|(w, p)| *w == word && *p == prefix) {
                        terms.push((word, prefix));
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
            let lists: Option<Vec<&Vec<(u64, u16)>>> = phrase
                .iter()
                .map(|word| self.postings.get(word.as_str()))
                .collect();
            let Some(lists) = lists else {
                continue;
            };
            let mut phrase_scores: HashMap<u64, f64> = HashMap::new();
            for list in &lists {
                bm25(list, &mut phrase_scores);
            }
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
                        let text: Vec<String> = words(&text).collect();
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
        let needed = if query.all_words {
            terms.len() + phrases.len()
        } else {
            1
        };
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
}
