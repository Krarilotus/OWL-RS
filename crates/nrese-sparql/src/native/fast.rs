//! Compiled FILTERs: common expression shapes evaluated on ids and borrowed dictionary text,
//! without building terms.
//!
//! [`compile`] turns an expression into a [`Fast`] predicate if every node is one of the
//! shapes below; otherwise the generic evaluator runs. Per row, a predicate answers true,
//! false, error (both drop the row) or [`Tri::Unknown`] for a value it doesn't handle (an
//! inline literal under `STR`, a non-integer in a numeric comparison), and the generic
//! evaluator decides that row. The semantics are the generic evaluator's; the randomized
//! differential test covers every shape.
//!
//! The predicates read stored ids. A value the query computed itself (`BIND`, a `SELECT`
//! expression, an aggregate) has an id of the query's own, which says nothing about the
//! term: every predicate but `BOUND` leaves such a row to the generic evaluator.
//!
//! | Shape | Evaluation |
//! |---|---|
//! | `BOUND(?v)`, `isIRI/isBlank/isLiteral(?v)` | the id's kind |
//! | `?v = <iri>` / `!=` | id equality (a stored IRI has one id) |
//! | `LANG(?v) = "tag"`, `LANGMATCHES(LANG(?v), "range")` | the language in the dictionary key |
//! | `CONTAINS/STRSTARTS/STRENDS(?v or STR(?v), "text")`, `REGEX(…, "pattern", "flags")` | the borrowed lexical form |
//! | `?v < > <= >= = 42` (and mirrored) | inline integer ids by value |
//! | `?v < > <= >= = "2000-01-01"^^xsd:date` (or `xsd:dateTime`, and mirrored) | inline ids of the constant's kind and timezone by id (their order is the value order); other values to the generic evaluator |
//! | `&&`, `||`, `!` | SPARQL's three-valued logic |

use std::cmp::Ordering;

use nrese_engine::{Snapshot, TermId, TermKind, TermView};
use nrese_exec::{UNDEF, computed_index};
use nrese_rdf::Variable;
use nrese_rdf::vocab::xsd;
use nrese_sparql_syntax::algebra::{Expression, Function};
use regex::Regex;

use super::expr::compile_regex;

/// A three-valued result, plus "not decided here".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tri {
    True,
    False,
    Error,
    Unknown,
}

impl Tri {
    fn of(b: bool) -> Self {
        if b { Self::True } else { Self::False }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TextOp {
    Contains,
    StartsWith,
    EndsWith,
}

pub(crate) enum Fast {
    And(Box<Fast>, Box<Fast>),
    Or(Box<Fast>, Box<Fast>),
    Not(Box<Fast>),
    Bound(Variable),
    IsIri(Variable),
    IsBlank(Variable),
    IsLiteral(Variable),
    /// `?v = <iri>`; `None` if the store doesn't know the IRI (never equal).
    SameIri(Variable, Option<u64>),
    LangEquals(Variable, String),
    LangMatches(Variable, String),
    /// Operand: the variable itself (`false`) or `STR(?v)` (`true`).
    Text(Variable, bool, TextOp, String),
    Regex(Variable, bool, Regex),
    /// `?v <op> integer`: `Ordering` of the variable against the constant must be in the set.
    IntCompare(Variable, Vec<Ordering>, i64),
    /// `?v <op> date` (or dateTime), the constant's inline id: as [`Self::IntCompare`].
    DateCompare(Variable, Vec<Ordering>, TermId),
}

fn variable(expr: &Expression) -> Option<&Variable> {
    match expr {
        Expression::Variable(v) => Some(v),
        _ => None,
    }
}

/// `?v` or `STR(?v)`.
fn string_operand(expr: &Expression) -> Option<(&Variable, bool)> {
    match expr {
        Expression::Variable(v) => Some((v, false)),
        Expression::FunctionCall(Function::Str, args) if args.len() == 1 => {
            Some((variable(&args[0])?, true))
        }
        _ => None,
    }
}

/// A simple (untyped, untagged) literal constant.
fn simple(expr: &Expression) -> Option<&str> {
    match expr {
        Expression::Literal(l) if l.language().is_none() && l.datatype() == xsd::STRING => {
            Some(l.value())
        }
        _ => None,
    }
}

fn integer_constant(expr: &Expression) -> Option<i64> {
    match expr {
        Expression::Literal(l) if l.datatype() == xsd::INTEGER => {
            let value: i64 = l.value().parse().ok()?;
            // Only canonical forms, so the constant and stored ids agree on identity.
            (value.to_string() == l.value()).then_some(value)
        }
        _ => None,
    }
}

/// An `xsd:date` or `xsd:dateTime` constant with an inline id (canonical, in range).
fn date_constant(expr: &Expression, snapshot: &Snapshot) -> Option<TermId> {
    match expr {
        Expression::Literal(l) if l.datatype() == xsd::DATE || l.datatype() == xsd::DATE_TIME => {
            snapshot
                .lookup(l.as_ref().into())
                .filter(|id| id.date_timezone().is_some())
        }
        _ => None,
    }
}

fn lang_of(expr: &Expression) -> Option<&Variable> {
    match expr {
        Expression::FunctionCall(Function::Lang, args) if args.len() == 1 => variable(&args[0]),
        _ => None,
    }
}

/// Compiles `expr` if every node has a fast form.
pub(crate) fn compile(expr: &Expression, snapshot: &Snapshot) -> Option<Fast> {
    let comparison = |a: &Expression, b: &Expression, orderings: &[Ordering]| -> Option<Fast> {
        // ?v op constant, or constant op ?v with the ordering mirrored.
        if let (Some(v), Some(c)) = (variable(a), integer_constant(b)) {
            return Some(Fast::IntCompare(v.clone(), orderings.to_vec(), c));
        }
        if let (Some(c), Some(v)) = (integer_constant(a), variable(b)) {
            return Some(Fast::IntCompare(
                v.clone(),
                orderings.iter().map(|o| o.reverse()).collect(),
                c,
            ));
        }
        if let (Some(v), Some(c)) = (variable(a), date_constant(b, snapshot)) {
            return Some(Fast::DateCompare(v.clone(), orderings.to_vec(), c));
        }
        if let (Some(c), Some(v)) = (date_constant(a, snapshot), variable(b)) {
            return Some(Fast::DateCompare(
                v.clone(),
                orderings.iter().map(|o| o.reverse()).collect(),
                c,
            ));
        }
        None
    };
    Some(match expr {
        Expression::And(a, b) => Fast::And(
            Box::new(compile(a, snapshot)?),
            Box::new(compile(b, snapshot)?),
        ),
        Expression::Or(a, b) => Fast::Or(
            Box::new(compile(a, snapshot)?),
            Box::new(compile(b, snapshot)?),
        ),
        Expression::Not(a) => Fast::Not(Box::new(compile(a, snapshot)?)),
        Expression::Bound(v) => Fast::Bound(v.clone()),
        Expression::Equal(a, b) | Expression::SameTerm(a, b) => {
            let iri = |x: &Expression| match x {
                Expression::NamedNode(n) => Some(n.clone()),
                _ => None,
            };
            let same_iri = match (variable(a), iri(b), variable(b), iri(a)) {
                (Some(v), Some(n), _, _) | (_, _, Some(v), Some(n)) => Some((v, n)),
                _ => None,
            };
            if let Some((v, n)) = same_iri {
                Fast::SameIri(
                    v.clone(),
                    snapshot.lookup(n.as_ref().into()).map(TermId::raw),
                )
            } else if let (Some(v), Some(tag)) = (lang_of(a), simple(b)) {
                Fast::LangEquals(v.clone(), tag.to_owned())
            } else if let (Some(tag), Some(v)) = (simple(a), lang_of(b)) {
                Fast::LangEquals(v.clone(), tag.to_owned())
            } else if matches!(expr, Expression::Equal(..)) {
                comparison(a, b, &[Ordering::Equal])?
            } else {
                return None;
            }
        }
        Expression::Greater(a, b) => comparison(a, b, &[Ordering::Greater])?,
        Expression::GreaterOrEqual(a, b) => {
            comparison(a, b, &[Ordering::Greater, Ordering::Equal])?
        }
        Expression::Less(a, b) => comparison(a, b, &[Ordering::Less])?,
        Expression::LessOrEqual(a, b) => comparison(a, b, &[Ordering::Less, Ordering::Equal])?,
        Expression::FunctionCall(function, args) => match (function, args.as_slice()) {
            (Function::IsIri, [a]) => Fast::IsIri(variable(a)?.clone()),
            (Function::IsBlank, [a]) => Fast::IsBlank(variable(a)?.clone()),
            (Function::IsLiteral, [a]) => Fast::IsLiteral(variable(a)?.clone()),
            (Function::LangMatches, [a, b]) => {
                Fast::LangMatches(lang_of(a)?.clone(), simple(b)?.to_owned())
            }
            (Function::Contains | Function::StrStarts | Function::StrEnds, [a, b]) => {
                let (v, str) = string_operand(a)?;
                let op = match function {
                    Function::Contains => TextOp::Contains,
                    Function::StrStarts => TextOp::StartsWith,
                    _ => TextOp::EndsWith,
                };
                Fast::Text(v.clone(), str, op, simple(b)?.to_owned())
            }
            (Function::Regex, [a, pattern]) | (Function::Regex, [a, pattern, _]) => {
                let (v, str) = string_operand(a)?;
                let flags = match args.get(2) {
                    Some(f) => simple(f)?,
                    None => "",
                };
                Fast::Regex(v.clone(), str, compile_regex(simple(pattern)?, flags)?)
            }
            _ => return None,
        },
        _ => return None,
    })
}

impl Fast {
    /// Evaluates for one row; `value(v)` is the row's id for `v` (UNDEF if unbound).
    pub(crate) fn eval(&self, value: &dyn Fn(&Variable) -> u64, snapshot: &Snapshot) -> Tri {
        // The stored id of `v` in this row. `Err` is the answer without one: an error for
        // an unbound variable, and "not decided here" for a term the query computed.
        let stored = |v: &Variable| -> Result<TermId, Tri> {
            let id = value(v);
            if id == UNDEF {
                Err(Tri::Error)
            } else if computed_index(id).is_some() {
                Err(Tri::Unknown)
            } else {
                Ok(TermId::from_raw(id))
            }
        };
        let on_kind = |v: &Variable, test: &dyn Fn(TermKind) -> bool| match stored(v) {
            Ok(id) => Tri::of(test(id.kind())),
            Err(undecided) => undecided,
        };
        let language = |id: TermId, test: &dyn Fn(&str) -> bool| match id.kind() {
            TermKind::Iri | TermKind::BlankNode => Tri::Error,
            TermKind::LangString => snapshot
                .with_view(id, |view| match view {
                    TermView::LangString { language, .. } => Tri::of(test(language)),
                    _ => Tri::Unknown,
                })
                .unwrap_or(Tri::Unknown),
            // Any other literal has the empty language tag.
            _ => Tri::of(test("")),
        };
        match self {
            Self::And(a, b) => match (a.eval(value, snapshot), b.eval(value, snapshot)) {
                (Tri::False, _) | (_, Tri::False) => Tri::False,
                (Tri::True, Tri::True) => Tri::True,
                (Tri::Unknown, _) | (_, Tri::Unknown) => Tri::Unknown,
                _ => Tri::Error,
            },
            Self::Or(a, b) => match (a.eval(value, snapshot), b.eval(value, snapshot)) {
                (Tri::True, _) | (_, Tri::True) => Tri::True,
                (Tri::False, Tri::False) => Tri::False,
                (Tri::Unknown, _) | (_, Tri::Unknown) => Tri::Unknown,
                _ => Tri::Error,
            },
            Self::Not(a) => match a.eval(value, snapshot) {
                Tri::True => Tri::False,
                Tri::False => Tri::True,
                other => other,
            },
            Self::Bound(v) => Tri::of(value(v) != UNDEF),
            Self::IsIri(v) => on_kind(v, &|k| k == TermKind::Iri),
            Self::IsBlank(v) => on_kind(v, &|k| k == TermKind::BlankNode),
            Self::IsLiteral(v) => on_kind(v, &|k| {
                !matches!(
                    k,
                    TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
                )
            }),
            Self::SameIri(v, iri) => match stored(v) {
                Ok(id) => Tri::of(Some(id.raw()) == *iri),
                Err(undecided) => undecided,
            },
            Self::LangEquals(v, tag) => match stored(v) {
                Ok(id) => language(id, &|language| language == tag),
                Err(undecided) => undecided,
            },
            Self::LangMatches(v, range) => match stored(v) {
                Ok(id) => language(id, &|language| lang_matches(language, range)),
                Err(undecided) => undecided,
            },
            Self::Text(v, str, op, needle) => match stored(v) {
                Ok(id) => on_text(id, *str, snapshot, |text| match op {
                    TextOp::Contains => text.contains(needle.as_str()),
                    TextOp::StartsWith => text.starts_with(needle.as_str()),
                    TextOp::EndsWith => text.ends_with(needle.as_str()),
                }),
                Err(undecided) => undecided,
            },
            Self::Regex(v, str, regex) => match stored(v) {
                Ok(id) => on_text(id, *str, snapshot, |text| regex.is_match(text)),
                Err(undecided) => undecided,
            },
            Self::IntCompare(v, orderings, constant) => match stored(v) {
                Ok(id) => match id.as_inline_integer() {
                    Some(x) => Tri::of(orderings.contains(&x.cmp(constant))),
                    None => Tri::Unknown,
                },
                Err(undecided) => undecided,
            },
            Self::DateCompare(v, orderings, constant) => match stored(v) {
                Ok(id)
                    if id.kind() == constant.kind()
                        && id.date_timezone() == constant.date_timezone() =>
                {
                    Tri::of(orderings.contains(&id.cmp(constant)))
                }
                Ok(_) => Tri::Unknown,
                Err(undecided) => undecided,
            },
        }
    }
}

/// A string test on a term (a string literal) or on its `STR` (also IRIs and every literal).
fn on_text(id: TermId, str: bool, snapshot: &Snapshot, test: impl FnOnce(&str) -> bool) -> Tri {
    if id.kind().is_inline() {
        // STR of an inline literal needs its canonical form; not string-typed without STR.
        return if str { Tri::Unknown } else { Tri::Error };
    }
    snapshot
        .with_view(id, |view| match (view, str) {
            (TermView::String(text) | TermView::LangString { value: text, .. }, _) => {
                Tri::of(test(text))
            }
            (TermView::Iri(text), true) => Tri::of(test(text)),
            // STR of a typed literal is its lexical form (§17.4.2.5).
            (TermView::Typed { value, .. }, true) => Tri::of(test(value)),
            (TermView::BlankNode(_) | TermView::Triple, _) | (_, false) => Tri::Error,
        })
        .unwrap_or(Tri::Unknown)
}

/// SPARQL `LANGMATCHES` (RFC 4647 basic filtering).
pub(crate) fn lang_matches(tag: &str, range: &str) -> bool {
    if range == "*" {
        return !tag.is_empty();
    }
    tag.len() >= range.len()
        && tag[..range.len()].eq_ignore_ascii_case(range)
        && (tag.len() == range.len() || tag.as_bytes()[range.len()] == b'-')
}
