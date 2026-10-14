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
//! | `sameTerm(?a, ?b)`; `?a = ?b` / `!=` where either is an IRI or a blank node | id equality (a stored term has one id); `=` between two literals compares values: the generic evaluator |
//! | `LANG(?v) = "tag"`, `LANGMATCHES(LANG(?v), "range")` | the language in the dictionary key |
//! | `CONTAINS/STRSTARTS/STRENDS(?v or STR(?v), "text")`, `REGEX(…, "pattern", "flags")` | the borrowed lexical form |
//! | `?v < > <= >= = 42` (and mirrored) | inline integer ids by value |
//! | `?v < > <= >= = "2000-01-01"^^xsd:date` (or `xsd:dateTime`, and mirrored) | inline ids of the constant's kind and timezone by id (their order is the value order); other values to the generic evaluator |
//! | `sameTerm(a, b)`, `isTRIPLE(a)` over variables, constants, `SUBJECT`/`PREDICATE`/`OBJECT` and `TRIPLE` ([`IdTerm`]) | ids: a triple term's parts read from its key, `TRIPLE` one hash probe ([`Snapshot::components`], [`Snapshot::triple_term`]) |
//! | `geof:sfWithin(?v, C)` and every topological relation between a variable and a constant geometry (or the other way round; `C` may be an expression of constants such as `geof:buffer(…)`) | the store's geometry index: the boxes decide most rows, a point is its box, other geometries are read where the boxes meet ([`super::spatial::CompiledRelation`]) |
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
    pub(crate) fn of(b: bool) -> Self {
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
    /// `?a = ?b`: id equality where either is an IRI or a blank node; between two
    /// literals `=` compares values, which the generic evaluator does. `sameTerm`
    /// uses `SameTerm`, including its fallback for legacy triple-term keys.
    SameVariables(Variable, Variable),
    LangEquals(Variable, String),
    LangMatches(Variable, String),
    /// Operand: the variable itself (`false`) or `STR(?v)` (`true`).
    Text(Variable, bool, TextOp, String),
    Regex(Variable, bool, Regex),
    /// `?v <op> integer`: `Ordering` of the variable against the constant must be in the set.
    IntCompare(Variable, Vec<Ordering>, i64),
    /// `?v <op> date` (or dateTime), the constant's inline id: as [`Self::IntCompare`].
    DateCompare(Variable, Vec<Ordering>, TermId),
    /// `sameTerm(a, b)` by ids.
    SameTerm(IdTerm, IdTerm),
    /// `isTRIPLE(a)` by the id's kind.
    IsTriple(IdTerm),
    /// A topological relation between a variable and a constant geometry.
    Spatial(Box<super::spatial::CompiledRelation>),
}

/// An operand evaluated on ids, nothing decoded: what the rewritten triple term patterns
/// compare and bind (`triple_terms`).
#[derive(Debug, Clone)]
pub(crate) enum IdTerm {
    Var(Variable),
    /// A constant, as [`IdTerm::value`] gives it.
    Const(Id),
    /// `SUBJECT`, `PREDICATE` or `OBJECT` (component 0, 1 or 2) of a triple term.
    Part(Box<IdTerm>, usize),
    /// `TRIPLE(s, p, o)`.
    Triple(Box<[IdTerm; 3]>),
}

/// An [`IdTerm`]'s value in one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Id {
    /// A term of the snapshot: its id is its identity.
    Stored(TermId),
    /// A term the snapshot doesn't have: no stored term is it.
    Absent,
    /// An error (an unbound variable, `SUBJECT` of no triple term, a literal subject).
    Error,
    /// Not decided on ids: a value the query computed, a triple term keyed by its text.
    Unknown,
}

impl IdTerm {
    /// The variables the operand reads.
    pub(crate) fn variables<'a>(&'a self, out: &mut Vec<&'a Variable>) {
        match self {
            Self::Var(v) => out.push(v),
            Self::Const(_) => {}
            Self::Part(inner, _) => inner.variables(out),
            Self::Triple(parts) => parts.iter().for_each(|part| part.variables(out)),
        }
    }

    /// The value in the row `value` gives the variables of (UNDEF if unbound).
    pub(crate) fn value(&self, value: &dyn Fn(&Variable) -> u64, snapshot: &Snapshot) -> Id {
        match self {
            Self::Var(v) => match value(v) {
                UNDEF => Id::Error,
                id if computed_index(id).is_some() => Id::Unknown,
                id => Id::Stored(TermId::from_raw(id)),
            },
            Self::Const(id) => *id,
            Self::Part(inner, at) => match inner.value(value, snapshot) {
                Id::Stored(id) if id.kind() != TermKind::Triple => Id::Error,
                Id::Stored(id) => snapshot
                    .components(id)
                    .map_or(Id::Unknown, |parts| Id::Stored(parts[*at])),
                Id::Error => Id::Error,
                Id::Absent | Id::Unknown => Id::Unknown,
            },
            Self::Triple(parts) => {
                let [s, p, o] = [0, 1, 2].map(|i| parts[i].value(value, snapshot));
                if [s, p, o].contains(&Id::Error) {
                    return Id::Error;
                }
                let (Id::Stored(s), Id::Stored(p), Id::Stored(o)) = (s, p, o) else {
                    return Id::Unknown;
                };
                // RDF 1.2: an IRI or blank node subject, an IRI predicate.
                if !matches!(s.kind(), TermKind::Iri | TermKind::BlankNode)
                    || p.kind() != TermKind::Iri
                {
                    return Id::Error;
                }
                snapshot.triple_term(s, p, o).map_or(Id::Absent, Id::Stored)
            }
        }
    }
}

/// `sameTerm` of two values on ids.
pub(crate) fn same_term(a: Id, b: Id, snapshot: &Snapshot) -> Tri {
    match (a, b) {
        (Id::Error, _) | (_, Id::Error) => Tri::Error,
        (Id::Stored(a), Id::Stored(b)) if a == b => Tri::True,
        // Two triple terms may be one in two keys if one has the first RDF 1.2 version's.
        (Id::Stored(a), Id::Stored(b))
            if a.kind() == TermKind::Triple
                && b.kind() == TermKind::Triple
                && (snapshot.components(a).is_none() || snapshot.components(b).is_none()) =>
        {
            Tri::Unknown
        }
        (Id::Stored(_), Id::Stored(_) | Id::Absent) | (Id::Absent, Id::Stored(_)) => Tri::False,
        _ => Tri::Unknown,
    }
}

/// `expr` as an operand on ids, if it is one: a variable, a constant whose id is its
/// identity, `SUBJECT`/`PREDICATE`/`OBJECT` and `TRIPLE` of such.
pub(crate) fn compile_term(expr: &Expression, snapshot: &Snapshot) -> Option<IdTerm> {
    Some(match expr {
        Expression::Variable(v) => IdTerm::Var(v.clone()),
        // An alias stands for a blank node of the query (`substitute`): the context knows it.
        Expression::NamedNode(n) if n.as_str().starts_with(super::substitute::ALIAS) => {
            return None;
        }
        Expression::NamedNode(n) => IdTerm::Const(
            snapshot
                .lookup(n.as_ref().into())
                .map_or(Id::Absent, Id::Stored),
        ),
        // Only a literal that is its id's term: an inline id may stand for other lexical
        // forms of its value ("01" and "1"), which are other terms.
        Expression::Literal(l) => IdTerm::Const(match snapshot.lookup(l.as_ref().into()) {
            None => Id::Absent,
            Some(id) if snapshot.decode(id).as_ref() == Some(&l.clone().into()) => Id::Stored(id),
            Some(_) => Id::Unknown,
        }),
        Expression::FunctionCall(function, args) => match (function, args.as_slice()) {
            (Function::Subject, [a]) => IdTerm::Part(Box::new(compile_term(a, snapshot)?), 0),
            (Function::Predicate, [a]) => IdTerm::Part(Box::new(compile_term(a, snapshot)?), 1),
            (Function::Object, [a]) => IdTerm::Part(Box::new(compile_term(a, snapshot)?), 2),
            (Function::Triple, [s, p, o]) => IdTerm::Triple(Box::new([
                compile_term(s, snapshot)?,
                compile_term(p, snapshot)?,
                compile_term(o, snapshot)?,
            ])),
            _ => return None,
        },
        _ => return None,
    })
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
            } else if let (Some(x), Some(y)) = (variable(a), variable(b))
                && matches!(expr, Expression::Equal(..))
            {
                Fast::SameVariables(x.clone(), y.clone())
            } else if let (Some(v), Some(tag)) = (lang_of(a), simple(b)) {
                Fast::LangEquals(v.clone(), tag.to_owned())
            } else if let (Some(tag), Some(v)) = (simple(a), lang_of(b)) {
                Fast::LangEquals(v.clone(), tag.to_owned())
            } else if matches!(expr, Expression::Equal(..)) {
                comparison(a, b, &[Ordering::Equal])?
            } else {
                Fast::SameTerm(compile_term(a, snapshot)?, compile_term(b, snapshot)?)
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
            (Function::IsTriple, [a]) => Fast::IsTriple(compile_term(a, snapshot)?),
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
            (Function::Custom(_), _) => Fast::Spatial(Box::new(
                super::spatial::CompiledRelation::compile(expr, snapshot)?,
            )),
            _ => return None,
        },
        _ => return None,
    })
}

impl Fast {
    /// Evaluates for one row; `value(v)` is the row's id for `v` (UNDEF if unbound), `view`
    /// looks up dictionary terms ([`Snapshot::with_views`]: one lock for many rows).
    pub(crate) fn eval<'v>(
        &self,
        value: &dyn Fn(&Variable) -> u64,
        view: &'v dyn Fn(TermId) -> Option<TermView<'v>>,
        snapshot: &Snapshot,
    ) -> Tri {
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
            TermKind::LangString => match view(id) {
                Some(TermView::LangString { language, .. }) => Tri::of(test(language)),
                _ => Tri::Unknown,
            },
            // Any other literal has the empty language tag.
            _ => Tri::of(test("")),
        };
        match self {
            Self::And(a, b) => match (a.eval(value, view, snapshot), b.eval(value, view, snapshot))
            {
                (Tri::False, _) | (_, Tri::False) => Tri::False,
                (Tri::True, Tri::True) => Tri::True,
                (Tri::Unknown, _) | (_, Tri::Unknown) => Tri::Unknown,
                _ => Tri::Error,
            },
            Self::Or(a, b) => {
                match (a.eval(value, view, snapshot), b.eval(value, view, snapshot)) {
                    (Tri::True, _) | (_, Tri::True) => Tri::True,
                    (Tri::False, Tri::False) => Tri::False,
                    (Tri::Unknown, _) | (_, Tri::Unknown) => Tri::Unknown,
                    _ => Tri::Error,
                }
            }
            Self::Not(a) => match a.eval(value, view, snapshot) {
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
                    TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph | TermKind::Triple
                )
            }),
            Self::SameVariables(a, b) => match (stored(a), stored(b)) {
                (Err(Tri::Error), _) | (_, Err(Tri::Error)) => Tri::Error,
                (Err(_), _) | (_, Err(_)) => Tri::Unknown,
                (Ok(x), Ok(y)) => {
                    let node =
                        |id: TermId| matches!(id.kind(), TermKind::Iri | TermKind::BlankNode);
                    if node(x) || node(y) {
                        Tri::of(x == y)
                    } else {
                        Tri::Unknown
                    }
                }
            },
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
                Ok(id) => on_text(id, *str, view, |text| match op {
                    TextOp::Contains => text.contains(needle.as_str()),
                    TextOp::StartsWith => text.starts_with(needle.as_str()),
                    TextOp::EndsWith => text.ends_with(needle.as_str()),
                }),
                Err(undecided) => undecided,
            },
            Self::Regex(v, str, regex) => match stored(v) {
                Ok(id) => on_text(id, *str, view, |text| regex.is_match(text)),
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
            Self::SameTerm(a, b) => {
                same_term(a.value(value, snapshot), b.value(value, snapshot), snapshot)
            }
            Self::Spatial(relation) => match stored(relation.variable()) {
                Ok(id) => relation.eval(id, view),
                Err(undecided) => undecided,
            },
            Self::IsTriple(a) => match a.value(value, snapshot) {
                Id::Stored(id) => Tri::of(id.kind() == TermKind::Triple),
                Id::Error => Tri::Error,
                Id::Absent | Id::Unknown => Tri::Unknown,
            },
        }
    }
}

/// A string test on a term (a string literal) or on its `STR` (also IRIs and every literal).
fn on_text<'v>(
    id: TermId,
    str: bool,
    view: &'v dyn Fn(TermId) -> Option<TermView<'v>>,
    test: impl FnOnce(&str) -> bool,
) -> Tri {
    if id.kind().is_inline() {
        // STR of an inline literal needs its canonical form; not string-typed without STR.
        return if str { Tri::Unknown } else { Tri::Error };
    }
    match view(id) {
        Some(found) => match (found, str) {
            (TermView::String(text) | TermView::LangString { value: text, .. }, _) => {
                Tri::of(test(text))
            }
            (TermView::Iri(text), true) => Tri::of(test(text)),
            // STR of a typed literal is its lexical form (§17.4.2.5).
            (TermView::Typed { value, .. }, true) => Tri::of(test(value)),
            (TermView::BlankNode(_) | TermView::Triple, _) | (_, false) => Tri::Error,
        },
        None => Tri::Unknown,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_term_variables_preserve_uninspectable_triple_fallback() {
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let snapshot = engine.snapshot();
        let a = Variable::new_unchecked("a");
        let b = Variable::new_unchecked("b");
        let expression = Expression::SameTerm(
            Box::new(Expression::Variable(a.clone())),
            Box::new(Expression::Variable(b)),
        );
        let fast = compile(&expression, &snapshot).unwrap();
        // No component key is available for these ids, as for legacy text-keyed
        // triple terms. Distinct ids alone cannot decide their RDF identity.
        let (x, y) = TermId::kind_range(TermKind::Triple);
        let (x, y) = (x.raw(), y.raw());
        let value = |v: &Variable| if v == &a { x } else { y };
        assert_eq!(fast.eval(&value, &|_| None, &snapshot), Tri::Unknown);
        assert_eq!(fast.eval(&|_| x, &|_| None, &snapshot), Tri::True);
        assert_eq!(fast.eval(&|_| UNDEF, &|_| None, &snapshot), Tri::Error);
    }
}
