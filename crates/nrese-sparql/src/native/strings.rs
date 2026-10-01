//! Dictionary-first string filters: `CONTAINS`, `STRSTARTS` and `STRENDS` on a large
//! pattern's object are answered from the dictionary first.
//!
//! Evaluated per row, `?s rdfs:label ?l FILTER(CONTAINS(?l, "Semantic"))` reads every
//! label's text at a random place in the dictionary: on DBpedia 20 million cache misses
//! for 445 answers. The engine can instead find the terms that pass in one parallel pass
//! over the dictionary's text ([`Snapshot::matching_strings`]); the scan then reads the
//! index for those terms only, as for a range hint. The conjunct still runs on every row
//! it returns, so the ids only prune.
//!
//! | Conjunct | Terms that can pass |
//! |---|---|
//! | `CONTAINS/STRSTARTS/STRENDS(?v, "text")` | simple and language-tagged literals with the text (others are errors) |
//! | the same on `STR(?v)` | IRIs and literals of every kind with the text, plus every inline literal (numbers and dates: their text isn't in the dictionary) |
//! | `LANG(?v) = "tag"` | language-tagged strings with that tag; with one of the above on `?v`, those of them with the text |
//!
//! The pass costs the dictionary's size, so it runs only for patterns with at least one
//! row per [`DICTIONARY_BYTES_PER_ROW`] bytes of dictionary text; for smaller ones the
//! per-row test is cheaper. It is dropped where more terms pass than the pattern has rows.
//! Many passing terms are matched by a scan of the whole pattern against a bitmap of them
//! rather than a seek each (`Context::scan_ranges`).

use nrese_engine::{Placement, Snapshot, StringTest, TermId, TermKind};
use nrese_rdf::Variable;
use nrese_rdf::vocab::xsd;
use nrese_sparql_syntax::algebra::{Expression, Function};

/// Dictionary bytes one row's random read costs about as much as reading in order (a
/// cache miss against memory bandwidth, the pass running on all cores).
pub(crate) const DICTIONARY_BYTES_PER_ROW: u64 = 512;

/// A string test on one variable, from filter conjuncts.
#[derive(Clone, Copy)]
pub(crate) struct Condition<'e> {
    pub(crate) variable: &'e Variable,
    /// Empty: no text test (a language alone).
    needle: &'e str,
    placement: Placement,
    /// The test is on `STR(?v)`: every kind of term has a text.
    of_str: bool,
    /// `LANG(?v) = "tag"`.
    language: Option<&'e str>,
}

impl Condition<'_> {
    /// A prefix test: answered by binary search where the dictionary's text order is at
    /// hand ([`Snapshot::text_order_ready`]), whatever the pattern's size.
    pub(crate) fn is_prefix(&self) -> bool {
        matches!(self.placement, Placement::Start | Placement::Whole) && !self.needle.is_empty()
    }
}

/// The string tests `conjuncts` make on `variable`, as one condition: a text test and a
/// language, each the first found.
pub(crate) fn combined<'e>(
    conjuncts: impl IntoIterator<Item = &'e Expression>,
    variable: &Variable,
) -> Option<Condition<'e>> {
    let mut result: Option<Condition<'e>> = None;
    for condition in conjuncts.into_iter().filter_map(condition) {
        if condition.variable != variable {
            continue;
        }
        result = Some(match result {
            None => condition,
            Some(mut found) => {
                if found.needle.is_empty() && !condition.needle.is_empty() {
                    found.needle = condition.needle;
                    found.placement = condition.placement;
                    found.of_str = condition.of_str;
                }
                found.language = found.language.or(condition.language);
                found
            }
        });
    }
    result
}

/// The string test `conjunct` makes on a variable, if it is one of the shapes above.
pub(crate) fn condition(conjunct: &Expression) -> Option<Condition<'_>> {
    if let Expression::Equal(a, b) = conjunct {
        let (call, tag) = match (&**a, &**b) {
            (call @ Expression::FunctionCall(..), Expression::Literal(tag))
            | (Expression::Literal(tag), call @ Expression::FunctionCall(..)) => (call, tag),
            _ => return None,
        };
        let Expression::FunctionCall(Function::Lang, args) = call else {
            return None;
        };
        let [Expression::Variable(variable)] = args.as_slice() else {
            return None;
        };
        // LANG returns a simple literal; equality with another kind is false or an error.
        if tag.datatype() != xsd::STRING || tag.value().is_empty() {
            return None;
        }
        return Some(Condition {
            variable,
            needle: "",
            placement: Placement::Anywhere,
            of_str: false,
            language: Some(tag.value()),
        });
    }
    let Expression::FunctionCall(function, args) = conjunct else {
        return None;
    };
    let placement = match function {
        Function::Contains => Placement::Anywhere,
        Function::StrStarts => Placement::Start,
        Function::StrEnds => Placement::End,
        _ => return None,
    };
    let [subject, Expression::Literal(needle)] = args.as_slice() else {
        return None;
    };
    // A simple literal: a language-tagged one is only compatible with some arguments.
    if needle.datatype() != xsd::STRING || needle.value().is_empty() {
        return None;
    }
    let (variable, of_str) = match subject {
        Expression::Variable(v) => (v, false),
        Expression::FunctionCall(Function::Str, inner) => match inner.as_slice() {
            [Expression::Variable(v)] => (v, true),
            _ => return None,
        },
        _ => return None,
    };
    Some(Condition {
        variable,
        needle: needle.value(),
        placement,
        of_str,
        language: None,
    })
}

/// The id ranges of the terms that can pass `condition`, sorted, for a pattern of `rows`
/// rows; `None` if they would hardly prune it.
pub(crate) fn ranges(
    snapshot: &Snapshot,
    condition: &Condition<'_>,
    rows: u64,
) -> Option<Vec<(TermId, TermId)>> {
    let test = StringTest {
        needle: condition.needle,
        placement: condition.placement,
        iris: condition.of_str,
        strings: true,
        lang_strings: true,
        typed: condition.of_str,
        language: condition.language,
    };
    let ids = snapshot.matching_strings(&test);
    if ids.len() as u64 > rows {
        return None;
    }
    let mut ranges: Vec<(TermId, TermId)> = Vec::with_capacity(ids.len());
    for id in ids {
        match ranges.last_mut() {
            Some((_, high)) if high.raw() + 1 == id.raw() => *high = id,
            _ => ranges.push((id, id)),
        }
    }
    // A language admits language-tagged strings only.
    if condition.of_str && condition.language.is_none() {
        ranges.extend(
            [
                TermKind::Integer,
                TermKind::Boolean,
                TermKind::Decimal,
                TermKind::Date,
                TermKind::DateTime,
            ]
            .map(TermId::kind_range),
        );
        ranges.sort_unstable();
    }
    Some(ranges)
}
