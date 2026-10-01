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
//!
//! The pass costs the dictionary's size, so it runs only for patterns with at least one
//! row per [`DICTIONARY_BYTES_PER_ROW`] bytes of dictionary text; for smaller ones the
//! per-row test is cheaper. It is dropped where too many terms pass to prune much.

use nrese_engine::{Placement, Snapshot, StringTest, TermId, TermKind};
use nrese_rdf::Variable;
use nrese_rdf::vocab::xsd;
use nrese_sparql_syntax::algebra::{Expression, Function};

/// Dictionary bytes one row's random read costs about as much as reading in order (a
/// cache miss against memory bandwidth, the pass running on all cores).
pub(crate) const DICTIONARY_BYTES_PER_ROW: u64 = 512;

/// A string test on one variable, from a filter conjunct.
pub(crate) struct Condition<'e> {
    pub(crate) variable: &'e Variable,
    needle: &'e str,
    placement: Placement,
    /// The test is on `STR(?v)`: every kind of term has a text.
    of_str: bool,
}

/// The string test `conjunct` makes on a variable, if it is one of the shapes above.
pub(crate) fn condition(conjunct: &Expression) -> Option<Condition<'_>> {
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
    };
    let ids = snapshot.matching_strings(&test);
    // Each id is a seek in the scan: past a quarter of the rows, scanning them is cheaper.
    if ids.len() as u64 > rows / 4 {
        return None;
    }
    let mut ranges: Vec<(TermId, TermId)> = Vec::with_capacity(ids.len());
    for id in ids {
        match ranges.last_mut() {
            Some((_, high)) if high.raw() + 1 == id.raw() => *high = id,
            _ => ranges.push((id, id)),
        }
    }
    if condition.of_str {
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
