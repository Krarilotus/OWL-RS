//! Range hints: FILTER comparisons turned into id ranges for the scan that binds the
//! variable.
//!
//! A hint lists the id ranges a variable's value must lie in for the FILTER to have any
//! chance of being true. It only prunes: the full FILTER still runs on every scanned row.
//! So a range must include every value that could pass, and ids of every kind that could
//! compare:
//! - an integer bound covers the inline integer range, plus every inline decimal and every
//!   dictionary typed literal (doubles, floats, xsd:int, non-canonical forms)
//! - a date bound covers the inline date range widened by a day on each side (dates with
//!   timezones compare indeterminately within ±14 h, and the FILTER decides those), plus
//!   every dictionary typed literal (non-canonical dates)
//!
//! - a bound on `YEAR(?v)` covers the inline dates and dateTimes of those years exactly
//!   (an inline id's payload starts with the local year, which is what `YEAR` returns),
//!   plus every dictionary typed literal (non-canonical forms, the g-types, years outside
//!   0000–9999)
//!
//! Other kinds (IRIs, strings, dateTimes against a date, …) make the comparison an error,
//! so they can't pass and are skipped. The kinds' tags are ordered Integer < Decimal < Date
//! < DateTime < TypedLiteral, so the ranges come out increasing, and the scan stays sorted.

use nrese_engine::{Snapshot, TermId, TermKind};
use nrese_rdf::Variable;
use nrese_rdf::vocab::xsd;
use nrese_sparql_syntax::algebra::{Expression, Function};

pub(crate) struct Hint {
    pub(crate) variable: Variable,
    pub(crate) ranges: Vec<(TermId, TermId)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Domain {
    Integer,
    Date,
    /// Bounds on `YEAR(?v)`: inclusive years in `low` and `high`.
    Year,
}

/// Bounds collected for one variable: inclusive integer bounds (of the value, or of its
/// year), or date ids.
struct Bounds {
    domain: Domain,
    low: Option<i64>,
    high: Option<i64>,
    low_id: Option<TermId>,
    high_id: Option<TermId>,
}

/// The range hints implied by the conjuncts of `expr`.
pub(crate) fn hints(expr: &Expression, snapshot: &Snapshot) -> Vec<Hint> {
    let mut bounds: Vec<(Variable, Option<Bounds>)> = Vec::new();
    collect(expr, snapshot, &mut bounds);
    bounds
        .into_iter()
        .filter_map(|(variable, bounds)| {
            Some(Hint {
                variable,
                ranges: ranges(bounds?)?
                    .into_iter()
                    .map(|(range, _)| range)
                    .collect(),
            })
        })
        .collect()
}

fn collect<'e>(
    expr: &'e Expression,
    snapshot: &Snapshot,
    out: &mut Vec<(Variable, Option<Bounds>)>,
) {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let (a, b, orderings): (&Expression, &Expression, &[std::cmp::Ordering]) = match expr {
        Expression::And(a, b) => {
            collect(a, snapshot, out);
            collect(b, snapshot, out);
            return;
        }
        Expression::Greater(a, b) => (a, b, &[Greater]),
        Expression::GreaterOrEqual(a, b) => (a, b, &[Greater, Equal]),
        Expression::Less(a, b) => (a, b, &[Less]),
        Expression::LessOrEqual(a, b) => (a, b, &[Less, Equal]),
        Expression::Equal(a, b) => (a, b, &[Equal]),
        _ => return,
    };
    // Normalise to `?v <ordering> constant` or `YEAR(?v) <ordering> constant`.
    let operand = |e: &'e Expression| -> Option<(&'e Variable, bool)> {
        match e {
            Expression::Variable(v) => Some((v, false)),
            Expression::FunctionCall(Function::Year, args) => match args.as_slice() {
                [Expression::Variable(v)] => Some((v, true)),
                _ => None,
            },
            _ => None,
        }
    };
    let ((variable, year), literal, orderings): (_, _, Vec<_>) = match (a, b) {
        (e, Expression::Literal(l)) if operand(e).is_some() => {
            (operand(e).expect("checked"), l, orderings.to_vec())
        }
        (Expression::Literal(l), e) if operand(e).is_some() => (
            operand(e).expect("checked"),
            l,
            orderings.iter().map(|o| o.reverse()).collect(),
        ),
        _ => return,
    };
    let (domain, integer, id) = if year {
        match (literal.datatype() == xsd::INTEGER)
            .then(|| literal.value().parse::<i64>().ok())
            .flatten()
        {
            Some(value) => (Domain::Year, Some(value), None),
            None => return,
        }
    } else if literal.datatype() == xsd::INTEGER {
        match literal.value().parse::<i64>() {
            Ok(value) => (Domain::Integer, Some(value), None),
            Err(_) => return,
        }
    } else if literal.datatype() == xsd::DATE {
        match snapshot.lookup(literal.as_ref().into()) {
            Some(id) if id.kind() == TermKind::Date => (Domain::Date, None, Some(id)),
            // A non-canonical date constant: no hint (still correct, just not narrowed).
            _ => return,
        }
    } else {
        return;
    };
    let entry = match out.iter_mut().find(|(v, _)| v == variable) {
        Some(entry) => entry,
        None => {
            out.push((
                variable.clone(),
                Some(Bounds {
                    domain,
                    low: None,
                    high: None,
                    low_id: None,
                    high_id: None,
                }),
            ));
            out.last_mut().expect("just pushed")
        }
    };
    let Some(bounds) = &mut entry.1 else {
        return;
    };
    if bounds.domain != domain {
        // Mixed domains on one variable: no hint.
        entry.1 = None;
        return;
    }
    let lower = orderings.contains(&Greater) || orderings == [Equal];
    let upper = orderings.contains(&Less) || orderings == [Equal];
    let strict = !orderings.contains(&Equal);
    match domain {
        Domain::Integer | Domain::Year => {
            let value = integer.expect("integer domain");
            if lower {
                let low = if strict {
                    value.saturating_add(1)
                } else {
                    value
                };
                bounds.low = Some(bounds.low.map_or(low, |l| l.max(low)));
            }
            if upper {
                let high = if strict {
                    value.saturating_sub(1)
                } else {
                    value
                };
                bounds.high = Some(bounds.high.map_or(high, |h| h.min(high)));
            }
        }
        Domain::Date => {
            let id = id.expect("date domain");
            if lower {
                let low = id.date_widened(false).expect("date id");
                bounds.low_id = Some(bounds.low_id.map_or(low, |l| l.max(low)));
            }
            if upper {
                let high = id.date_widened(true).expect("date id");
                bounds.high_id = Some(bounds.high_id.map_or(high, |h| h.min(high)));
            }
        }
    }
}

/// The id ranges of `bounds`, each with whether the comparisons hold for every id in it
/// (exact: inline integers and integer-derived literals between the bounds, inline dates
/// of the years) or only may (decimals, typed literals, dates within a day of a bound).
fn ranges(bounds: Bounds) -> Option<Vec<((TermId, TermId), bool)>> {
    let kind = |k: TermKind| TermId::kind_range(k);
    let typed = (kind(TermKind::TypedLiteral), false);
    Some(match bounds.domain {
        Domain::Integer => {
            let (min, max) = kind(TermKind::Integer);
            let low = bounds.low.map_or(min, |l| {
                TermId::inline_integer(l).unwrap_or(if l < 0 { min } else { max })
            });
            let high = bounds.high.map_or(max, |h| {
                TermId::inline_integer(h).unwrap_or(if h < 0 { min } else { max })
            });
            // A bound past the inline integers on the far side is clamped to their last
            // (or first) value, which it excludes: then the range is wider than the values.
            let exact = bounds
                .low
                .is_none_or(|l| l < 0 || TermId::inline_integer(l).is_some())
                && bounds
                    .high
                    .is_none_or(|h| h > 0 || TermId::inline_integer(h).is_some());
            let mut ranges = Vec::new();
            if low <= high {
                ranges.push(((low, high), exact));
            }
            ranges.push((kind(TermKind::Decimal), false));
            ranges.push(typed);
            // Integer-derived literals (xsd:int, ...), inline and ordered by value; bounds
            // past their inline range are clamped to it, where no value lies.
            if let Some(range) = TermId::derived_integer_range(
                bounds.low.unwrap_or(i64::MIN),
                bounds.high.unwrap_or(i64::MAX),
            ) {
                ranges.push((range, true));
            }
            ranges
        }
        Domain::Date => {
            let (min, max) = kind(TermKind::Date);
            let low = bounds.low_id.unwrap_or(min);
            let high = bounds.high_id.unwrap_or(max);
            let mut ranges = Vec::new();
            if low <= high {
                ranges.push(((low, high), false));
            }
            ranges.push(typed);
            ranges
        }
        Domain::Year => {
            // Inline years are 0000-9999; the rest are in the dictionary.
            let low = bounds.low.unwrap_or(0).max(0);
            let high = bounds.high.unwrap_or(9999).min(9999);
            let mut ranges = Vec::new();
            if low <= high {
                for kind in [TermKind::Date, TermKind::DateTime] {
                    let (first, last) = TermId::year_range(kind, low as u32, high as u32)
                        .expect("a date kind and inline years");
                    ranges.push(((first, last), true));
                }
            }
            ranges.push(typed);
            ranges
        }
    })
}

/// A FILTER whose every conjunct bounds one variable to id ranges (comparisons with a
/// constant as [`hints`] reads them, `isIRI`, `isBlank`, `isLiteral`): the ranges where it
/// holds for every id (`exact`) and those where it holds for some (`checked`, decided per
/// value).
pub(crate) struct Bounded {
    pub(crate) variable: Variable,
    pub(crate) exact: Vec<(TermId, TermId)>,
    pub(crate) checked: Vec<(TermId, TermId)>,
}

/// `expr` as [`Bounded`], if it is one.
pub(crate) fn bounded(expr: &Expression, snapshot: &Snapshot) -> Option<Bounded> {
    let mut conjuncts = Vec::new();
    super::pushdown::conjuncts_of(expr, &mut conjuncts);
    let mut variable: Option<Variable> = None;
    let same = |variable: &mut Option<Variable>, v: &Variable| -> bool {
        variable.get_or_insert_with(|| v.clone()) == v
    };
    let mut kinds: Option<Vec<(TermId, TermId)>> = None;
    let mut compared: Vec<(Variable, Option<Bounds>)> = Vec::new();
    // A language test holds for language-tagged strings only: their range, decided by
    // the filter (Sparqloscope 096: none among 30.5 M names, a pass over every term).
    let mut language = false;
    for conjunct in conjuncts {
        if let Some(v) = language_test(conjunct) {
            if !same(&mut variable, v) {
                return None;
            }
            language = true;
            continue;
        }
        if let Expression::FunctionCall(function, args) = conjunct
            && let [Expression::Variable(v)] = args.as_slice()
            && let Some(ranges) = kind_ranges(function)
        {
            if !same(&mut variable, v) {
                return None;
            }
            kinds = Some(match kinds {
                None => ranges,
                Some(before) => intersection(&before, &ranges),
            });
            continue;
        }
        let mut alone = Vec::new();
        collect(conjunct, snapshot, &mut alone);
        let [(v, Some(_))] = alone.as_slice() else {
            return None;
        };
        if !same(&mut variable, v) {
            return None;
        }
        collect(conjunct, snapshot, &mut compared);
    }
    let variable = variable?;
    let (mut exact, mut checked) = (Vec::new(), Vec::new());
    match compared.pop() {
        None if language => {
            let tagged = [TermId::kind_range(TermKind::LangString)];
            checked = match &kinds {
                Some(kinds) => intersection(kinds, &tagged),
                None => tagged.to_vec(),
            };
        }
        None => exact = kinds?,
        Some((_, bounds)) => {
            for (range, holds) in ranges(bounds?)? {
                let parts = match &kinds {
                    Some(kinds) => intersection(kinds, &[range]),
                    None => vec![range],
                };
                match holds {
                    true => exact.extend(parts),
                    false => checked.extend(parts),
                }
            }
        }
    }
    if language {
        // Only the language-tagged strings among them, every one decided by the filter.
        let tagged = [TermId::kind_range(TermKind::LangString)];
        checked = intersection(&checked, &tagged);
        checked.extend(intersection(&exact, &tagged));
        checked.sort_unstable();
        exact.clear();
    }
    Some(Bounded {
        variable,
        exact,
        checked,
    })
}

/// The variable of `LANG(?v) = "tag"` (either way round, a non-empty simple literal tag).
fn language_test(conjunct: &Expression) -> Option<&Variable> {
    let Expression::Equal(a, b) = conjunct else {
        return None;
    };
    let (call, tag) = match (&**a, &**b) {
        (call @ Expression::FunctionCall(..), Expression::Literal(tag))
        | (Expression::Literal(tag), call @ Expression::FunctionCall(..)) => (call, tag),
        _ => return None,
    };
    let Expression::FunctionCall(Function::Lang, args) = call else {
        return None;
    };
    let [Expression::Variable(v)] = args.as_slice() else {
        return None;
    };
    (tag.datatype() == xsd::STRING && !tag.value().is_empty()).then_some(v)
}

/// The ids for which the term-kind test `function` holds: literals are the kinds from
/// simple strings to typed literals and the integer-derived ones (a triple term is none).
fn kind_ranges(function: &Function) -> Option<Vec<(TermId, TermId)>> {
    let kind = |k: TermKind| TermId::kind_range(k);
    Some(match function {
        Function::IsIri => vec![kind(TermKind::Iri)],
        Function::IsBlank => vec![kind(TermKind::BlankNode)],
        Function::IsLiteral => vec![
            (kind(TermKind::String).0, kind(TermKind::TypedLiteral).1),
            kind(TermKind::DerivedInteger),
        ],
        _ => return None,
    })
}

/// The ids in both lists of inclusive ranges, each sorted and disjoint.
fn intersection(a: &[(TermId, TermId)], b: &[(TermId, TermId)]) -> Vec<(TermId, TermId)> {
    let mut out = Vec::new();
    for &(a_low, a_high) in a {
        for &(b_low, b_high) in b {
            let (low, high) = (a_low.max(b_low), a_high.min(b_high));
            if low <= high {
                out.push((low, high));
            }
        }
    }
    out.sort_unstable();
    out
}
