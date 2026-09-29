//! Compiled FILTERs: common expression shapes evaluated on ids and borrowed dictionary text,
//! without building terms.
//!
//! [`compile`] turns an expression into a [`Fast`] predicate if every node is one of the
//! shapes below; otherwise the generic evaluator runs. Per row, a predicate answers true,
//! false, error (both drop the row) or [`Tri::Unknown`] for a value it doesn't handle (an
//! inline literal under `STR`, a non-integer in a numeric comparison), and the generic
//! evaluator decides that row. The semantics are spareval's; the randomized differential
//! test covers every shape.
//!
//! | Shape | Evaluation |
//! |---|---|
//! | `BOUND(?v)`, `isIRI/isBlank/isLiteral(?v)` | the id's kind |
//! | `?v = <iri>` / `!=` | id equality (a stored IRI has one id) |
//! | `LANG(?v) = "tag"`, `LANGMATCHES(LANG(?v), "range")` | the language in the dictionary key |
//! | `CONTAINS/STRSTARTS/STRENDS(?v or STR(?v), "text")`, `REGEX(…, "pattern", "flags")` | the borrowed lexical form |
//! | `?v < > <= >= = 42` (and mirrored) | inline integer ids by value |
//! | `&&`, `||`, `!` | SPARQL's three-valued logic |

use std::cmp::Ordering;

use nrese_engine::{Snapshot, TermId, TermKind, TermView};
use nrese_exec::UNDEF;
use oxrdf::Variable;
use oxrdf::vocab::xsd;
use regex::Regex;
use spargebra::algebra::{Expression, Function};

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
        let kind = |v: &Variable| {
            let id = value(v);
            (id != UNDEF).then(|| TermId::from_raw(id).kind())
        };
        let is_literal = |k: TermKind| {
            !matches!(
                k,
                TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
            )
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
            Self::IsIri(v) => kind(v).map_or(Tri::Error, |k| Tri::of(k == TermKind::Iri)),
            Self::IsBlank(v) => kind(v).map_or(Tri::Error, |k| Tri::of(k == TermKind::BlankNode)),
            Self::IsLiteral(v) => kind(v).map_or(Tri::Error, |k| Tri::of(is_literal(k))),
            Self::SameIri(v, iri) => {
                let id = value(v);
                if id == UNDEF {
                    Tri::Error
                } else {
                    Tri::of(Some(id) == *iri)
                }
            }
            Self::LangEquals(v, tag) => match kind(v) {
                None => Tri::Error,
                Some(TermKind::Iri | TermKind::BlankNode) => Tri::Error,
                Some(TermKind::LangString) => {
                    let id = TermId::from_raw(value(v));
                    snapshot
                        .with_view(id, |view| match view {
                            TermView::LangString { language, .. } => Tri::of(language == tag),
                            _ => Tri::Unknown,
                        })
                        .unwrap_or(Tri::Unknown)
                }
                // Any other literal has the empty language tag.
                Some(_) => Tri::of(tag.is_empty()),
            },
            Self::LangMatches(v, range) => match kind(v) {
                None | Some(TermKind::Iri | TermKind::BlankNode) => Tri::Error,
                Some(TermKind::LangString) => {
                    let id = TermId::from_raw(value(v));
                    snapshot
                        .with_view(id, |view| match view {
                            TermView::LangString { language, .. } => {
                                Tri::of(lang_matches(language, range))
                            }
                            _ => Tri::Unknown,
                        })
                        .unwrap_or(Tri::Unknown)
                }
                Some(_) => Tri::of(lang_matches("", range)),
            },
            Self::Text(v, str, op, needle) => {
                self.on_text(v, *str, value, snapshot, |text| match op {
                    TextOp::Contains => text.contains(needle.as_str()),
                    TextOp::StartsWith => text.starts_with(needle.as_str()),
                    TextOp::EndsWith => text.ends_with(needle.as_str()),
                })
            }
            Self::Regex(v, str, regex) => {
                self.on_text(v, *str, value, snapshot, |text| regex.is_match(text))
            }
            Self::IntCompare(v, orderings, constant) => {
                let id = value(v);
                if id == UNDEF {
                    return Tri::Error;
                }
                match TermId::from_raw(id).as_inline_integer() {
                    Some(x) => Tri::of(orderings.contains(&x.cmp(constant))),
                    None => Tri::Unknown,
                }
            }
        }
    }

    /// A string test on `?v` (a string literal) or `STR(?v)` (also IRIs and every literal).
    fn on_text(
        &self,
        v: &Variable,
        str: bool,
        value: &dyn Fn(&Variable) -> u64,
        snapshot: &Snapshot,
        test: impl FnOnce(&str) -> bool,
    ) -> Tri {
        let id = value(v);
        if id == UNDEF {
            return Tri::Error;
        }
        let id = TermId::from_raw(id);
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
                // STR of a typed literal is its canonical form (see `value::canonical`).
                (TermView::Typed { .. }, true) => Tri::Unknown,
                (TermView::BlankNode(_), _) | (_, false) => Tri::Error,
            })
            .unwrap_or(Tri::Unknown)
    }
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
