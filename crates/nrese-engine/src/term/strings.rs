//! Dictionary-first string tests: the terms whose text contains, starts with, ends with or
//! equals a string, found in one pass over the dictionary arena.
//!
//! A filter such as `CONTAINS(?label, "Semantic")` evaluated per row reads each row's
//! string from the dictionary at a random place: one cache miss per row, 20 million of
//! them for DBpedia's labels. Every distinct string is in the arena once, in one byte
//! array, so the same test can run as a substring search over the arena instead
//! (`memchr`'s SIMD `memmem`), in parallel over slices of it, at memory bandwidth. Each hit
//! is mapped back to its entry by a binary search over the entry ends and checked against
//! the entry's kind and the part of the key that holds the text. The result is the
//! matching ids, sorted: the executor scans the index for those values only.

use super::offsets::Ends;
use rayon::prelude::*;

use super::{TermId, TermKind};

/// Where the text must occur in a term's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Anywhere,
    Start,
    End,
    Whole,
}

/// A test on terms' text: `needle` at `placement`, among the kinds selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringTest<'a> {
    pub needle: &'a str,
    pub placement: Placement,
    /// The text of IRIs (what `STR` gives).
    pub iris: bool,
    /// Simple literals and `xsd:string`.
    pub strings: bool,
    /// Language-tagged strings, with or without a base direction.
    pub lang_strings: bool,
    /// The lexical form of literals of other datatypes.
    pub typed: bool,
    /// Only language-tagged strings with this tag, as stored (what `LANG` returns); the
    /// kinds above are then ignored. With an empty needle, the language alone decides.
    pub language: Option<&'a str>,
    /// The needle (lower-case ASCII) is found whatever the ASCII case of the text, and a
    /// text holding U+0130 or U+212A (whose lower case has an ASCII letter without being
    /// one) passes too: a superset of the texts whose Unicode lower case (`LCASE`) holds
    /// the needle, which the caller tests exactly.
    pub ascii_case_insensitive: bool,
}

/// The characters whose lower case holds an ASCII letter though they aren't ASCII: `İ`
/// (to `i` and a combining dot) and the Kelvin sign (to `k`).
const LOWERING_TO_ASCII: [&str; 2] = ["\u{0130}", "\u{212A}"];

/// Whether `text` holds `needle` (lower-case ASCII) at `placement`, whatever its ASCII
/// case, or one of [`LOWERING_TO_ASCII`].
fn placed_any_case(text: &[u8], needle: &[u8], placement: Placement) -> bool {
    let n = needle.len();
    let special = LOWERING_TO_ASCII
        .iter()
        .any(|c| memchr::memmem::find(text, c.as_bytes()).is_some());
    special
        || match placement {
            Placement::Anywhere => {
                n == 0 || text.windows(n).any(|w| w.eq_ignore_ascii_case(needle))
            }
            Placement::Start => text.len() >= n && text[..n].eq_ignore_ascii_case(needle),
            Placement::End => {
                text.len() >= n && text[text.len() - n..].eq_ignore_ascii_case(needle)
            }
            Placement::Whole => text.eq_ignore_ascii_case(needle),
        }
}

/// Entries per parallel slice.
const SLICE: usize = 1 << 16;

/// What a dictionary string scan did (EXPLAIN's `dictionary string test`; the guard of
/// performance.md §0): a bulk load's terms are 256 arenas, one per shard of
/// [`super::pending`], and searching them one after another, each with searchers of its
/// own, made Olympics q06 4.3 → 18 ms (36704d9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StringScan {
    /// Searchers built: one per pass, whatever the number of arenas.
    pub searchers: usize,
    /// Parallel loops run: one over every slice of every arena.
    pub parallel_loops: usize,
    /// Arenas searched (the base, heaps and shards).
    pub arenas: usize,
    /// Slices searched: at most [`SLICE`] entries of one arena each.
    pub slices: usize,
    /// Keys the test read: each hit's (once per pass), or every one where the needle is
    /// empty (a language alone), or each candidate of a prefix's range.
    pub keys_tested: u64,
}

impl std::fmt::Display for StringScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} searchers, {} parallel loops over {} slices of {} arenas, {} keys tested",
            self.searchers, self.parallel_loops, self.slices, self.arenas, self.keys_tested
        )
    }
}

/// A search for a hit in a haystack: the needle's, or (`Candidate`) a character of
/// [`LOWERING_TO_ASCII`], a hit that only makes its key a candidate.
enum Searcher {
    Exact(memchr::memmem::Finder<'static>),
    AnyCase(aho_corasick::AhoCorasick),
    Candidate(memchr::memmem::Finder<'static>),
}

impl Searcher {
    /// The first hit in `haystack`, and whether it is the needle.
    fn find(&self, haystack: &[u8]) -> Option<(usize, bool)> {
        match self {
            Self::Exact(finder) => finder.find(haystack).map(|at| (at, true)),
            Self::AnyCase(any_case) => any_case.find(haystack).map(|m| (m.start(), true)),
            Self::Candidate(finder) => finder.find(haystack).map(|at| (at, false)),
        }
    }
}

/// A test's searchers, built once for every arena a scan reads: one per pass, the needle's
/// and, without regard to ASCII case, one per character of [`LOWERING_TO_ASCII`].
pub(crate) struct Matcher<'a> {
    test: &'a StringTest<'a>,
    searchers: Vec<Searcher>,
}

impl<'a> Matcher<'a> {
    pub(crate) fn new(test: &'a StringTest<'a>) -> Self {
        let finder = |needle: &str| memchr::memmem::Finder::new(needle.as_bytes()).into_owned();
        let searchers = if !test.ascii_case_insensitive || test.needle.is_empty() {
            vec![Searcher::Exact(finder(test.needle))]
        } else {
            // The needle by one search (its rare bytes in both cases first), and the
            // characters lowering to ASCII by `memmem`, each its own pass.
            let any_case = aho_corasick::AhoCorasick::builder()
                .ascii_case_insensitive(true)
                .build([test.needle.as_bytes()])
                .expect("one short pattern");
            std::iter::once(Searcher::AnyCase(any_case))
                .chain(LOWERING_TO_ASCII.map(|c| Searcher::Candidate(finder(c))))
                .collect()
        };
        Self { test, searchers }
    }

    /// Searchers built (one per pass).
    pub(crate) fn searchers(&self) -> usize {
        self.searchers.len()
    }

    /// Every pass over entries `first..last` of an arena (`bytes`, entry `i` ending at
    /// `ends[i]`, its id index `first_index + i`), on this thread: the ids of those that
    /// pass `test` pushed to `out` (once per pass that finds them); returns the keys tested.
    pub(crate) fn slice(
        &self,
        bytes: &[u8],
        ends: &(impl Ends + ?Sized),
        (first, last): (usize, usize),
        first_index: u64,
        out: &mut Vec<TermId>,
    ) -> u64 {
        let test = self.test;
        debug_assert!(last <= ends.count(), "a slice of the arena's entries");
        if first >= last {
            return 0;
        }
        // No text to find: every entry is tested (a language alone).
        if test.needle.is_empty() {
            out.extend((first..last).filter_map(|entry| {
                let key = &bytes[ends.start(entry)..ends.end(entry)];
                passes(key, first_index + entry as u64, None, test)
            }));
            return (last - first) as u64;
        }
        let (from, to) = (ends.start(first), ends.end(last - 1));
        let mut tested = 0;
        for searcher in &self.searchers {
            let mut entry = first;
            let mut at = from;
            // Each hit's entry, then on past that entry: one test per matching entry.
            while let Some((hit, needle)) = searcher.find(&bytes[at..to]).map(|(o, n)| (at + o, n))
            {
                entry = ends.holding(entry, last, hit);
                let (start, end) = (ends.start(entry), ends.end(entry));
                tested += 1;
                if let Some(id) = passes(
                    &bytes[start..end],
                    first_index + entry as u64,
                    needle.then_some(hit - start),
                    test,
                ) {
                    out.push(id);
                }
                at = end;
                entry += 1;
                if entry >= last {
                    break;
                }
            }
        }
        tested
    }
}

/// The slices of an arena's first `entries` entries: [`SLICE`] entries each.
pub(crate) fn slices(entries: usize) -> impl Iterator<Item = (usize, usize)> {
    (0..entries.div_ceil(SLICE)).map(move |s| (s * SLICE, ((s + 1) * SLICE).min(entries)))
}

/// The ids `search` finds in `parts` (slices of arenas, `arenas` of them), in one
/// parallel loop over all of them, sorted and deduplicated; and what the scan did.
pub(crate) fn scan<P: Sync>(
    matcher: &Matcher<'_>,
    arenas: usize,
    parts: &[P],
    search: impl Fn(&P, &mut Vec<TermId>) -> u64 + Sync,
) -> (Vec<TermId>, StringScan) {
    let tested = std::sync::atomic::AtomicU64::new(0);
    let mut ids: Vec<TermId> = parts
        .par_iter()
        .flat_map_iter(|part| {
            let mut found = Vec::new();
            let keys = search(part, &mut found);
            tested.fetch_add(keys, std::sync::atomic::Ordering::Relaxed);
            found
        })
        .collect();
    // In parallel: a bulk load's shards give their ids out of order (YAGO q10: 0.4 M of
    // them, 10 ms on one thread).
    ids.par_sort_unstable();
    ids.dedup();
    let scan = StringScan {
        searchers: matcher.searchers(),
        parallel_loops: 1,
        arenas,
        slices: parts.len(),
        keys_tested: tested.into_inner(),
    };
    (ids, scan)
}

/// The id of entry `index` with key `key` if `test` holds for its text, given the first
/// occurrence of the needle in the key at `hit` (`None`: not known, as for a candidate's
/// hit). A later occurrence may be the one that counts (an end placement, or a first hit
/// inside a language tag or datatype), so the text is searched again where the first hit
/// doesn't decide.
pub(crate) fn passes(
    key: &[u8],
    index: u64,
    hit: Option<usize>,
    test: &StringTest<'_>,
) -> Option<TermId> {
    if let Some(language) = test.language {
        let tag_end = 1 + memchr::memchr(0, key.get(1..)?)?;
        if !matches!(key[0], b'L' | b'D') || &key[1..tag_end] != language.as_bytes() {
            return None;
        }
        let text_start = after_separators(key, if key[0] == b'L' { 1 } else { 2 })?;
        return placed(&key[text_start..], key.len(), text_start, hit, test)
            .then(|| TermId::new(TermKind::LangString, index));
    }
    let (kind, text_start) = match key[0] {
        b'I' if test.iris => (TermKind::Iri, 1),
        b'S' if test.strings => (TermKind::String, 1),
        b'L' if test.lang_strings => (TermKind::LangString, after_separators(key, 1)?),
        b'D' if test.lang_strings => (TermKind::LangString, after_separators(key, 2)?),
        b'T' if test.typed => (TermKind::TypedLiteral, after_separators(key, 1)?),
        _ => return None,
    };
    placed(&key[text_start..], key.len(), text_start, hit, test).then(|| TermId::new(kind, index))
}

/// Whether the needle is where `test` wants it in `text`, the part of a key of `key_len`
/// bytes from `text_start` on, given a first hit of the needle at `hit` in the key.
fn placed(
    text: &[u8],
    key_len: usize,
    text_start: usize,
    hit: Option<usize>,
    test: &StringTest<'_>,
) -> bool {
    let needle = test.needle.as_bytes();
    // The needle wholly within the text: found anywhere, whatever the case (the hit of a
    // search without regard to it), without searching the text again. A candidate's hit
    // proves nothing, nor one in a tag or a datatype, nor one running into the next key.
    if test.placement == Placement::Anywhere
        && hit.is_some_and(|hit| hit >= text_start && hit + needle.len() <= key_len)
    {
        return true;
    }
    if test.ascii_case_insensitive {
        return placed_any_case(text, needle, test.placement);
    }
    match test.placement {
        Placement::Anywhere => memchr::memmem::find(text, needle).is_some(),
        Placement::Start => text.starts_with(needle),
        Placement::End => text.ends_with(needle),
        Placement::Whole => text == needle,
    }
}

/// The position after the `n`th separator (a zero byte) of `key`.
fn after_separators(key: &[u8], n: usize) -> Option<usize> {
    let mut at = 0;
    for _ in 0..n {
        at += memchr::memchr(0, &key[at..])? + 1;
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use nrese_rdf::{BaseDirection, BlankNode, Literal, NamedNode, Term};

    use super::*;
    use crate::{Dictionary, TermView};

    /// An empty needle without regard to case: every text holds it, and only an empty
    /// one is it (it panicked in `windows(0)`: Sol's review, 18 October).
    #[test]
    fn an_empty_needle_without_regard_to_case_holds_everywhere() {
        let dictionary = Dictionary::default();
        let texts = ["hello", "", "HeLLo \u{0130}"];
        let ids: Vec<TermId> = texts
            .iter()
            .map(|t| dictionary.intern(Literal::new_simple_literal(*t).as_ref().into()))
            .collect();
        for (placement, expected) in [
            (Placement::Anywhere, ids.clone()),
            (Placement::Start, ids.clone()),
            (Placement::End, ids.clone()),
            // The empty text, and the candidate (its lower case is the caller's to test).
            (Placement::Whole, vec![ids[1], ids[2]]),
        ] {
            let test = StringTest {
                needle: "",
                placement,
                iris: false,
                strings: true,
                lang_strings: false,
                typed: false,
                language: None,
                ascii_case_insensitive: true,
            };
            assert_eq!(
                dictionary.matching_strings(&test, u64::MAX),
                expected,
                "{placement:?}"
            );
        }
    }

    /// The arena search against a test of every entry's view.
    #[test]
    fn the_arena_search_equals_testing_every_term() {
        let dictionary = Dictionary::default();
        let mut terms: Vec<Term> = Vec::new();
        let words = ["Semantic", "web", "semantic", "Saint", "wind", "x", "Sem"];
        let mut state = 7u64;
        let mut next = |n: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % n
        };
        for i in 0..3_000 {
            let text: String = (0..next(4))
                .map(|_| words[next(words.len() as u64) as usize])
                .collect::<Vec<_>>()
                .join(if next(2) == 0 { " " } else { "" });
            let term: Term = match next(6) {
                0 => NamedNode::new_unchecked(format!("http://e/{text}{i}")).into(),
                1 => Literal::new_simple_literal(format!("{text}{i}")).into(),
                2 => Literal::new_simple_literal(text.clone()).into(),
                3 => Literal::new_language_tagged_literal_unchecked(text.clone(), "sem").into(),
                4 => Literal::new_directional_language_tagged_literal_unchecked(
                    text.clone(),
                    "en",
                    BaseDirection::Rtl,
                )
                .into(),
                _ => Literal::new_typed_literal(
                    text.clone(),
                    NamedNode::new_unchecked("http://e/Semantic"),
                )
                .into(),
            };
            terms.push(term);
        }
        let ids: Vec<TermId> = terms
            .iter()
            .map(|t| dictionary.intern(t.as_ref()))
            .collect();
        for needle in [
            "Semantic", "sem", "Sem", "wind", "x", "nticweb", "Saint", "nope",
        ] {
            for placement in [
                Placement::Anywhere,
                Placement::Start,
                Placement::End,
                Placement::Whole,
            ] {
                for (iris, strings, lang_strings, typed) in [
                    (false, true, true, false),
                    (true, true, true, true),
                    (false, false, false, true),
                ] {
                    let test = StringTest {
                        needle,
                        placement,
                        iris,
                        strings,
                        lang_strings,
                        typed,
                        language: None,
                        ascii_case_insensitive: false,
                    };
                    let mut expected: Vec<TermId> = ids
                        .iter()
                        .copied()
                        .filter(|&id| id.kind().is_dictionary())
                        .filter(|&id| {
                            dictionary
                                .with_view(id, |view| {
                                    let text = match view {
                                        TermView::Iri(t) if iris => t,
                                        TermView::String(t) if strings => t,
                                        TermView::LangString { value, .. } if lang_strings => value,
                                        TermView::Typed { value, .. } if typed => value,
                                        _ => return false,
                                    };
                                    match placement {
                                        Placement::Anywhere => text.contains(needle),
                                        Placement::Start => text.starts_with(needle),
                                        Placement::End => text.ends_with(needle),
                                        Placement::Whole => text == needle,
                                    }
                                })
                                .unwrap_or(false)
                        })
                        .collect();
                    expected.sort_unstable();
                    expected.dedup();
                    let got = dictionary.matching_strings(&test, u64::MAX);
                    assert_eq!(got, expected, "{test:?}");
                    // A limit hides later entries.
                    let limit = 1_000;
                    let bounded = dictionary.matching_strings(&test, limit);
                    let visible: Vec<TermId> = expected
                        .into_iter()
                        .filter(|id| id.payload() < limit)
                        .collect();
                    assert_eq!(bounded, visible, "{test:?} below {limit}");
                }
            }
        }
        // A language, alone or with text: only language-tagged strings with that tag.
        for (language, needle) in [
            ("en", ""),
            ("sem", ""),
            ("en", "Sem"),
            ("sem", "web"),
            ("de", ""),
        ] {
            let test = StringTest {
                needle,
                placement: Placement::Anywhere,
                iris: true,
                strings: true,
                lang_strings: true,
                typed: true,
                language: Some(language),
                ascii_case_insensitive: false,
            };
            let mut expected: Vec<TermId> = ids
                .iter()
                .copied()
                .filter(|&id| {
                    dictionary
                        .with_view(id, |view| {
                            matches!(view, TermView::LangString { value, language: tag, .. }
                                if tag == language && value.contains(needle))
                        })
                        .unwrap_or(false)
                })
                .collect();
            expected.sort_unstable();
            expected.dedup();
            assert_eq!(
                dictionary.matching_strings(&test, u64::MAX),
                expected,
                "{test:?}"
            );
        }
    }

    /// A needle's hit decides without the text searched again only where it lies wholly
    /// in the text: not a candidate's hit, nor one in a tag or a datatype, nor one running
    /// into the next key (`mich` then `ael`), against each term's view.
    #[test]
    fn needle_hits_decide_only_within_the_text() {
        let dictionary = Dictionary::default();
        let mut ids = vec![
            dictionary.intern(Literal::new_simple_literal("mich").as_ref().into()),
            dictionary.intern(Literal::new_simple_literal("ael").as_ref().into()),
        ];
        let datatype = NamedNode::new_unchecked("http://e/michael/\u{0130}/\u{212A}");
        for text in [
            "",
            "MiChAeL",
            "x MICHAEL tail",
            "\u{0130}",
            "\u{212A}",
            "nothing",
        ] {
            let terms: [Term; 6] = [
                NamedNode::new_unchecked(format!("http://e/{text}")).into(),
                BlankNode::new_unchecked(format!("b{text}")).into(),
                Literal::new_simple_literal(text).into(),
                Literal::new_language_tagged_literal_unchecked(text, "michael").into(),
                Literal::new_directional_language_tagged_literal_unchecked(
                    text,
                    "en",
                    BaseDirection::Rtl,
                )
                .into(),
                Literal::new_typed_literal(text, datatype.clone()).into(),
            ];
            ids.extend(terms.iter().map(|term| dictionary.intern(term.as_ref())));
        }
        for needle in ["michael", "chs", "i", "k", "missing"] {
            for placement in [
                Placement::Anywhere,
                Placement::Start,
                Placement::End,
                Placement::Whole,
            ] {
                for ascii_case_insensitive in [false, true] {
                    for language in [None, Some("michael"), Some("en"), Some("de")] {
                        let test = StringTest {
                            needle,
                            placement,
                            iris: true,
                            strings: true,
                            lang_strings: true,
                            typed: true,
                            language,
                            ascii_case_insensitive,
                        };
                        let text_passes = |text: &str| {
                            if ascii_case_insensitive
                                && (text.contains('\u{0130}') || text.contains('\u{212A}'))
                            {
                                return true;
                            }
                            let text = match ascii_case_insensitive {
                                true => text.to_ascii_lowercase(),
                                false => text.to_owned(),
                            };
                            match placement {
                                Placement::Anywhere => text.contains(needle),
                                Placement::Start => text.starts_with(needle),
                                Placement::End => text.ends_with(needle),
                                Placement::Whole => text == needle,
                            }
                        };
                        let mut expected: Vec<TermId> = ids
                            .iter()
                            .copied()
                            .filter(|&id| {
                                dictionary
                                    .with_view(id, |view| match (language, view) {
                                        (
                                            Some(wanted),
                                            TermView::LangString {
                                                value, language, ..
                                            },
                                        ) => language == wanted && text_passes(value),
                                        (Some(_), _) => false,
                                        (
                                            None,
                                            TermView::Iri(value)
                                            | TermView::String(value)
                                            | TermView::LangString { value, .. }
                                            | TermView::Typed { value, .. },
                                        ) => text_passes(value),
                                        _ => false,
                                    })
                                    .unwrap_or(false)
                            })
                            .collect();
                        expected.sort_unstable();
                        assert_eq!(
                            dictionary.matching_strings(&test, u64::MAX),
                            expected,
                            "{test:?}"
                        );
                        let limit = dictionary.len() / 2;
                        expected.retain(|id| id.payload() < limit);
                        assert_eq!(
                            dictionary.matching_strings(&test, limit),
                            expected,
                            "{test:?} below {limit}"
                        );
                    }
                }
            }
        }
    }

    /// Without regard to ASCII case: every text whose Unicode lower case holds the needle
    /// is found (with İ and the Kelvin sign, whose lower case holds ASCII letters).
    #[test]
    fn any_case_finds_every_text_whose_lower_case_holds_the_needle() {
        let dictionary = Dictionary::default();
        let texts = [
            "Michael",
            "MICHAEL",
            "MİCHAEL",
            "mİ",
            "Kelvin",
            "300 \u{212A}",
            "nothing",
            "MiChAeL x",
        ];
        let ids: Vec<TermId> = texts
            .iter()
            .map(|t| dictionary.intern(Literal::new_simple_literal(*t).as_ref().into()))
            .collect();
        for (needle, placement) in [
            ("michael", Placement::Anywhere),
            ("mi", Placement::Start),
            ("k", Placement::Anywhere),
            ("michael", Placement::Whole),
            ("x", Placement::End),
        ] {
            let test = StringTest {
                needle,
                placement,
                iris: false,
                strings: true,
                lang_strings: true,
                typed: false,
                language: None,
                ascii_case_insensitive: true,
            };
            let found = dictionary.matching_strings(&test, u64::MAX);
            for (text, id) in texts.iter().zip(&ids) {
                let lower = text.to_lowercase();
                let exact = match placement {
                    Placement::Anywhere => lower.contains(needle),
                    Placement::Start => lower.starts_with(needle),
                    Placement::End => lower.ends_with(needle),
                    Placement::Whole => lower == needle,
                };
                if exact {
                    assert!(found.contains(id), "{needle} {placement:?}: {text}");
                }
            }
            assert!(!found.contains(&ids[6]), "{needle}: nothing");
        }
    }
}
