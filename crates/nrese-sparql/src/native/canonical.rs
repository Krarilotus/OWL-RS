//! Canonical equality answers ([`crate::QueryOptions::equality_canonical`]): every term
//! the query names stands for its `owl:sameAs` class, shown by the class's representative
//! (research-designs §1, "Canonical answers").
//!
//! Canonical reads normalise the constants of a triple pattern themselves
//! (`nrese_engine::Snapshot::canonical_classes`); the solutions they give name
//! representatives. A term that reaches the solutions another way has to take the same
//! mapping, or it misses the class it names: `VALUES ?x { <alias> } ?x ex:p ex:o` would
//! join `<alias>` with the representative the scan gives, and `FILTER(?x = <alias>)` would
//! compare them. So, by that one mapping:
//!
//! - the query's constants (triple patterns, paths, VALUES, expressions of FILTER, BIND,
//!   ORDER BY, aggregates and EXISTS, subqueries, pre-bound values) are put in as their
//!   representatives once, before planning ([`Context::canonical_pattern`]);
//! - a term the evaluation makes or receives (`IRI()`, a federated row) gets its
//!   representative's id when it gets an id ([`Context::id`]), and `IRI()` names the
//!   representative inside an expression too ([`super::Evaluator`]);
//! - solutions of a named graph name its terms as stored (named graphs carry no equality,
//!   as in strict answers), and are mapped where they leave `GRAPH`;
//! - values sent to a `SERVICE` endpoint, which names terms as stored, go as every
//!   identity of their class (`federation`'s bind join).
//!
//! Inside `GRAPH` and `SERVICE` nothing is mapped: a named graph is read as stored, and a
//! remote endpoint knows nothing of the classes. Triple terms are one term each, as in
//! the store's classes.

use std::sync::Arc;

use nrese_engine::{EqualityClasses, Snapshot, TermId};
use nrese_rdf::{Literal, NamedNode, Term, TermRef};
use nrese_sparql_syntax::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression, PropertyPathExpression,
};
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

use super::Context;

/// The classes canonical answers name terms by, with the snapshot that decodes them.
#[derive(Clone)]
pub(crate) struct Names {
    snapshot: Snapshot,
    classes: Arc<EqualityClasses>,
}

impl Names {
    /// The names of canonical reads of `snapshot` in `model`, if they rename anything.
    pub(super) fn of(snapshot: &Snapshot, model: nrese_engine::ReadModel) -> Option<Self> {
        Some(Self {
            snapshot: snapshot.clone(),
            classes: snapshot.canonical_classes(model)?,
        })
    }

    /// The representative of the stored term `id` (itself outside every class).
    pub(super) fn id(&self, id: u64) -> u64 {
        self.classes.representative(TermId::from_raw(id)).raw()
    }

    /// The identities the stored term `id` stands for: its class's, or itself.
    pub(super) fn members<'a>(&'a self, id: &'a u64) -> &'a [u64] {
        self.classes.members_of(id)
    }

    /// `term`'s representative and its id, if `term` is another identity of a class.
    fn representative(&self, term: TermRef<'_>) -> Option<(u64, Term)> {
        let id = self.snapshot.lookup(term)?;
        let representative = self.classes.representative(id);
        if representative == id {
            return None;
        }
        Some((representative.raw(), self.snapshot.decode(representative)?))
    }

    /// `term` as canonical answers name it.
    pub(crate) fn term(&self, term: Term) -> Term {
        match self.representative(term.as_ref()) {
            Some((_, representative)) => representative,
            None => term,
        }
    }
}

impl Context<'_> {
    /// The canonical names where the triple patterns are read now: the default graph as a
    /// whole, under canonical answers.
    pub(super) fn canonical_here(&self) -> Option<&Names> {
        self.canonical.as_ref().filter(|_| self.late_scope())
    }

    /// `pattern` with the query's constants put in as their representatives (module
    /// docs); unchanged without canonical answers.
    pub(super) fn canonical_pattern(&self, pattern: GraphPattern) -> GraphPattern {
        match self.canonical_here() {
            Some(names) => Rewrite { ctx: self, names }.pattern(&pattern),
            None => pattern,
        }
    }

    /// Solutions leaving `GRAPH`, their stored names mapped (module docs).
    pub(super) fn canonical_solutions(&self, mut solutions: super::Solutions) -> super::Solutions {
        if let Some(names) = self.canonical_here() {
            let columns = std::mem::take(&mut solutions.table).into_columns();
            let columns = columns
                .into_iter()
                .map(|column| column.into_iter().map(|id| names.id(id)).collect())
                .collect();
            solutions.table = nrese_exec::IdTable::from_columns(columns);
            solutions.ordered = false;
        }
        solutions
    }
}

/// A constant put in for another identity of a class.
enum Constant {
    Iri(NamedNode),
    Literal(Literal),
}

struct Rewrite<'c, 'a> {
    ctx: &'c Context<'a>,
    names: &'c Names,
}

impl Rewrite<'_, '_> {
    /// The constant for `term`'s representative, if `term` is another identity of a class.
    /// A blank node (or, where only an IRI may stand, a literal) is put in as an alias IRI
    /// that stands for it ([`super::substitute`]).
    fn constant(&self, term: TermRef<'_>, iri_only: bool) -> Option<Constant> {
        let (id, representative) = self.names.representative(term)?;
        Some(match representative {
            Term::NamedNode(n) => Constant::Iri(n),
            Term::Literal(l) if !iri_only => Constant::Literal(l),
            Term::Triple(_) => return None,
            Term::BlankNode(_) | Term::Literal(_) => {
                let alias = super::substitute::alias(&format!("canonical-{id}"));
                self.ctx.register_alias(alias.as_str(), id);
                Constant::Iri(alias)
            }
        })
    }

    fn iri(&self, n: &NamedNode) -> NamedNode {
        match self.constant(n.as_ref().into(), true) {
            Some(Constant::Iri(m)) => m,
            _ => n.clone(),
        }
    }

    fn term_pattern(&self, term: &TermPattern) -> TermPattern {
        let constant = match term {
            TermPattern::NamedNode(n) => self.constant(n.as_ref().into(), false),
            TermPattern::Literal(l) => self.constant(l.as_ref().into(), false),
            _ => None,
        };
        match constant {
            Some(Constant::Iri(n)) => TermPattern::NamedNode(n),
            Some(Constant::Literal(l)) => TermPattern::Literal(l),
            None => term.clone(),
        }
    }

    fn named_node_pattern(&self, name: &NamedNodePattern) -> NamedNodePattern {
        match name {
            NamedNodePattern::NamedNode(n) => NamedNodePattern::NamedNode(self.iri(n)),
            other => other.clone(),
        }
    }

    fn triple(&self, triple: &TriplePattern) -> TriplePattern {
        TriplePattern {
            subject: self.term_pattern(&triple.subject),
            predicate: self.named_node_pattern(&triple.predicate),
            object: self.term_pattern(&triple.object),
        }
    }

    fn path(&self, path: &PropertyPathExpression) -> PropertyPathExpression {
        let p = |x: &PropertyPathExpression| Box::new(self.path(x));
        match path {
            PropertyPathExpression::NamedNode(n) => PropertyPathExpression::NamedNode(self.iri(n)),
            PropertyPathExpression::Reverse(x) => PropertyPathExpression::Reverse(p(x)),
            PropertyPathExpression::Sequence(steps) => {
                PropertyPathExpression::Sequence(steps.iter().map(|x| self.path(x)).collect())
            }
            PropertyPathExpression::Alternative(steps) => {
                PropertyPathExpression::Alternative(steps.iter().map(|x| self.path(x)).collect())
            }
            PropertyPathExpression::ZeroOrMore(x) => PropertyPathExpression::ZeroOrMore(p(x)),
            PropertyPathExpression::OneOrMore(x) => PropertyPathExpression::OneOrMore(p(x)),
            PropertyPathExpression::ZeroOrOne(x) => PropertyPathExpression::ZeroOrOne(p(x)),
            PropertyPathExpression::NegatedPropertySet(names) => {
                PropertyPathExpression::NegatedPropertySet(
                    names.iter().map(|n| self.iri(n)).collect(),
                )
            }
        }
    }

    fn ground(&self, term: &GroundTerm) -> GroundTerm {
        let constant = match term {
            GroundTerm::NamedNode(n) => self.constant(n.as_ref().into(), false),
            GroundTerm::Literal(l) => self.constant(l.as_ref().into(), false),
            GroundTerm::Triple(_) => None,
        };
        match constant {
            Some(Constant::Iri(n)) => GroundTerm::NamedNode(n),
            Some(Constant::Literal(l)) => GroundTerm::Literal(l),
            None => term.clone(),
        }
    }

    fn expression(&self, expression: &Expression) -> Expression {
        let e = |x: &Expression| Box::new(self.expression(x));
        let list = |xs: &[Expression]| xs.iter().map(|x| self.expression(x)).collect();
        let constant = match expression {
            Expression::NamedNode(n) => self.constant(n.as_ref().into(), false),
            Expression::Literal(l) => self.constant(l.as_ref().into(), false),
            _ => None,
        };
        if let Some(constant) = constant {
            return match constant {
                Constant::Iri(n) => Expression::NamedNode(n),
                Constant::Literal(l) => Expression::Literal(l),
            };
        }
        match expression {
            Expression::NamedNode(_)
            | Expression::Literal(_)
            | Expression::Variable(_)
            | Expression::Bound(_) => expression.clone(),
            Expression::Or(a, b) => Expression::Or(e(a), e(b)),
            Expression::And(a, b) => Expression::And(e(a), e(b)),
            Expression::Equal(a, b) => Expression::Equal(e(a), e(b)),
            Expression::SameTerm(a, b) => Expression::SameTerm(e(a), e(b)),
            Expression::Greater(a, b) => Expression::Greater(e(a), e(b)),
            Expression::GreaterOrEqual(a, b) => Expression::GreaterOrEqual(e(a), e(b)),
            Expression::Less(a, b) => Expression::Less(e(a), e(b)),
            Expression::LessOrEqual(a, b) => Expression::LessOrEqual(e(a), e(b)),
            Expression::Add(a, b) => Expression::Add(e(a), e(b)),
            Expression::Subtract(a, b) => Expression::Subtract(e(a), e(b)),
            Expression::Multiply(a, b) => Expression::Multiply(e(a), e(b)),
            Expression::Divide(a, b) => Expression::Divide(e(a), e(b)),
            Expression::UnaryPlus(a) => Expression::UnaryPlus(e(a)),
            Expression::UnaryMinus(a) => Expression::UnaryMinus(e(a)),
            Expression::Not(a) => Expression::Not(e(a)),
            Expression::In(a, xs) => Expression::In(e(a), list(xs)),
            Expression::If(a, b, c) => Expression::If(e(a), e(b), e(c)),
            Expression::Coalesce(xs) => Expression::Coalesce(list(xs)),
            Expression::FunctionCall(function, args) => {
                Expression::FunctionCall(function.clone(), list(args))
            }
            Expression::Exists(pattern) => Expression::Exists(Box::new(self.pattern(pattern))),
        }
    }

    fn pattern(&self, pattern: &GraphPattern) -> GraphPattern {
        let p = |x: &GraphPattern| Box::new(self.pattern(x));
        match pattern {
            GraphPattern::Bgp { patterns } => GraphPattern::Bgp {
                patterns: patterns.iter().map(|t| self.triple(t)).collect(),
            },
            GraphPattern::Path {
                subject,
                path,
                object,
            } => GraphPattern::Path {
                subject: self.term_pattern(subject),
                path: self.path(path),
                object: self.term_pattern(object),
            },
            GraphPattern::Join { inputs } => GraphPattern::Join {
                inputs: inputs.iter().map(|x| self.pattern(x)).collect(),
            },
            GraphPattern::Union { inputs } => GraphPattern::Union {
                inputs: inputs.iter().map(|x| self.pattern(x)).collect(),
            },
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => GraphPattern::LeftJoin {
                left: p(left),
                right: p(right),
                expression: expression.as_ref().map(|x| self.expression(x)),
            },
            GraphPattern::Lateral { left, right } => GraphPattern::Lateral {
                left: p(left),
                right: p(right),
            },
            GraphPattern::Filter { expr, inner } => GraphPattern::Filter {
                expr: self.expression(expr),
                inner: p(inner),
            },
            GraphPattern::Extend { inner, bindings } => GraphPattern::Extend {
                inner: p(inner),
                bindings: bindings
                    .iter()
                    .map(|(v, x)| (v.clone(), self.expression(x)))
                    .collect(),
            },
            GraphPattern::Minus { left, right } => GraphPattern::Minus {
                left: p(left),
                right: p(right),
            },
            GraphPattern::Values {
                variables,
                bindings,
            } => GraphPattern::Values {
                variables: variables.clone(),
                bindings: bindings
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|t| t.as_ref().map(|t| self.ground(t)))
                            .collect()
                    })
                    .collect(),
            },
            GraphPattern::OrderBy { inner, expression } => GraphPattern::OrderBy {
                inner: p(inner),
                expression: expression
                    .iter()
                    .map(|o| match o {
                        OrderExpression::Asc(x) => OrderExpression::Asc(self.expression(x)),
                        OrderExpression::Desc(x) => OrderExpression::Desc(self.expression(x)),
                    })
                    .collect(),
            },
            GraphPattern::Project { inner, variables } => GraphPattern::Project {
                inner: p(inner),
                variables: variables.clone(),
            },
            GraphPattern::Distinct { inner } => GraphPattern::Distinct { inner: p(inner) },
            GraphPattern::Reduced { inner } => GraphPattern::Reduced { inner: p(inner) },
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => GraphPattern::Slice {
                inner: p(inner),
                start: *start,
                length: *length,
            },
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => GraphPattern::Group {
                inner: p(inner),
                variables: variables.clone(),
                aggregates: aggregates
                    .iter()
                    .map(|(v, a)| {
                        let a = match a {
                            AggregateExpression::CountSolutions { distinct } => {
                                AggregateExpression::CountSolutions {
                                    distinct: *distinct,
                                }
                            }
                            AggregateExpression::FunctionCall {
                                name,
                                expr,
                                distinct,
                            } => AggregateExpression::FunctionCall {
                                name: name.clone(),
                                expr: self.expression(expr),
                                distinct: *distinct,
                            },
                        };
                        (v.clone(), a)
                    })
                    .collect(),
            },
            // Read as stored, or by another endpoint (module docs).
            GraphPattern::Graph { .. } | GraphPattern::Service { .. } => pattern.clone(),
        }
    }
}
