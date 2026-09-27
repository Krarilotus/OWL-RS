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
//! Other kinds (IRIs, strings, dateTimes against a date, …) make the comparison an error,
//! so they can't pass and are skipped. The kinds' tags are ordered Integer < Decimal < Date
//! < TypedLiteral, so the ranges come out increasing, and the scan stays sorted.

use nrese_engine::{Snapshot, TermId, TermKind};
use oxrdf::Variable;
use oxrdf::vocab::xsd;
use spargebra::algebra::Expression;

pub(crate) struct Hint {
    pub(crate) variable: Variable,
    pub(crate) ranges: Vec<(TermId, TermId)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Domain {
    Integer,
    Date,
}

/// Bounds collected for one variable: inclusive integer bounds, or date ids.
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
                ranges: ranges(bounds?)?,
            })
        })
        .collect()
}

fn collect(expr: &Expression, snapshot: &Snapshot, out: &mut Vec<(Variable, Option<Bounds>)>) {
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
    // Normalise to `?v <ordering> constant`.
    let (variable, literal, orderings): (_, _, Vec<_>) = match (a, b) {
        (Expression::Variable(v), Expression::Literal(l)) => (v, l, orderings.to_vec()),
        (Expression::Literal(l), Expression::Variable(v)) => {
            (v, l, orderings.iter().map(|o| o.reverse()).collect())
        }
        _ => return,
    };
    let (domain, integer, id) = if literal.datatype() == xsd::INTEGER {
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
        Domain::Integer => {
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

fn ranges(bounds: Bounds) -> Option<Vec<(TermId, TermId)>> {
    let kind = |k: TermKind| TermId::kind_range(k);
    let typed = kind(TermKind::TypedLiteral);
    Some(match bounds.domain {
        Domain::Integer => {
            let (min, max) = kind(TermKind::Integer);
            let low = bounds.low.map_or(min, |l| {
                TermId::inline_integer(l).unwrap_or(if l < 0 { min } else { max })
            });
            let high = bounds.high.map_or(max, |h| {
                TermId::inline_integer(h).unwrap_or(if h < 0 { min } else { max })
            });
            let mut ranges = Vec::new();
            if low <= high {
                ranges.push((low, high));
            }
            ranges.push(kind(TermKind::Decimal));
            ranges.push(typed);
            ranges
        }
        Domain::Date => {
            let (min, max) = kind(TermKind::Date);
            let low = bounds.low_id.unwrap_or(min);
            let high = bounds.high_id.unwrap_or(max);
            let mut ranges = Vec::new();
            if low <= high {
                ranges.push((low, high));
            }
            ranges.push(typed);
            ranges
        }
    })
}
