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
}

/// Entries per parallel slice.
const SLICE: usize = 1 << 16;

/// The ids of the first `limit` entries of an arena (`bytes`, entry `i` ending at
/// `ends[i]`, its id index `first_index + i`) that pass `test`, sorted.
pub(crate) fn matching(
    bytes: &[u8],
    ends: &[u64],
    first_index: u64,
    limit: u64,
    test: &StringTest<'_>,
) -> Vec<TermId> {
    let entries = ends.len().min(limit as usize);
    if entries == 0 {
        return Vec::new();
    }
    let finder = memchr::memmem::Finder::new(test.needle.as_bytes());
    let start_of = |i: usize| if i == 0 { 0 } else { ends[i - 1] as usize };
    let mut ids: Vec<TermId> = (0..entries.div_ceil(SLICE))
        .into_par_iter()
        .flat_map_iter(|slice| {
            let (first, last) = (slice * SLICE, ((slice + 1) * SLICE).min(entries));
            // No text to find: every entry is tested (a language alone).
            if test.needle.is_empty() {
                return (first..last)
                    .filter_map(|entry| {
                        let key = &bytes[start_of(entry)..ends[entry] as usize];
                        passes(key, first_index + entry as u64, 0, test)
                    })
                    .collect::<Vec<_>>();
            }
            let (from, to) = (start_of(first), ends[last - 1] as usize);
            let mut found = Vec::new();
            let mut entry = first;
            let mut at = from;
            // Each hit's entry, then on past that entry: one test per matching entry.
            while let Some(hit) = finder.find(&bytes[at..to]).map(|offset| at + offset) {
                entry += ends[entry..last].partition_point(|&end| end as usize <= hit);
                let (start, end) = (start_of(entry), ends[entry] as usize);
                if let Some(id) = passes(
                    &bytes[start..end],
                    first_index + entry as u64,
                    hit - start,
                    test,
                ) {
                    found.push(id);
                }
                at = end;
                entry += 1;
                if entry >= last {
                    break;
                }
            }
            found
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// The id of entry `index` with key `key` if `test` holds for its text, given the first
/// occurrence of the needle in the key at `hit`. A later occurrence may be the one that
/// counts (an end placement, or a first hit inside a language tag or datatype), so the
/// text is searched again where the first hit doesn't decide.
pub(crate) fn passes(key: &[u8], index: u64, hit: usize, test: &StringTest<'_>) -> Option<TermId> {
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
/// bytes from `text_start` on, given a first hit at `hit` in the key.
fn placed(
    text: &[u8],
    key_len: usize,
    text_start: usize,
    hit: usize,
    test: &StringTest<'_>,
) -> bool {
    let needle = test.needle.as_bytes();
    match test.placement {
        Placement::Anywhere => {
            (hit >= text_start && hit + needle.len() <= key_len)
                || memchr::memmem::find(text, needle).is_some()
        }
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
    use nrese_rdf::{BaseDirection, Literal, NamedNode, Term};

    use super::*;
    use crate::{Dictionary, TermView};

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
}
