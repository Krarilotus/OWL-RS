//! Canonical equality answers ([`crate::QueryOptions::equality_canonical`]): the query
//! evaluated over the quotient of the dataset by its `owl:sameAs` classes, each class one
//! term, shown by its representative (research-designs §1, "Canonical answers").
//!
//! **One id space.** Every id the evaluation works on names a class by its representative,
//! so that joins, filters and every rewrite of the plan (filter pushdown across `GRAPH`,
//! sideways information passing) see one consistent id space and stay valid as they are.
//! Ids enter evaluation at four places, and each takes the one mapping,
//! `nrese_engine::Snapshot::canonical_classes`:
//!
//! - **reads**: a canonical snapshot gives representatives from every graph (the engine's
//!   canonical reads; named graphs over representatives too);
//! - **the query's constants**: put in as their representatives once, before planning, in
//!   its pattern and its CONSTRUCT template, inside triple terms too, with pre-bound values
//!   ([`Context::canonical_query`]);
//! - **terms the evaluation makes**: `IRI()` names the representative, through the
//!   canonical-name function the same pass wraps around it ([`FUNCTION`]);
//! - **a remote endpoint's rows**: mapped where they are imported (a triple term part by
//!   part), and a bound value sent to an endpoint as every identity of its class
//!   (`federation`).
//!
//! Whether canonical answers apply is decided once per query (`Context::new`): where its
//! default graph is the one the equality covers (the store's, or the merge of every graph,
//! in a model with inferred statements) and some class exists. Elsewhere, and without the
//! option, nothing here runs: the plan is the one without canonical answers.
//!
//! **Triple terms.** In the quotient a triple term's class is that of its parts:
//! `<<( alias p o )>>` and `<<( rep p o )>>` are one term. Where every triple term the store
//! holds is over representatives, a stored triple term is its class's, a triple term the
//! query writes is put in over representatives, and one the evaluation builds or takes
//! apart (`TRIPLE`, `SUBJECT`, triple patterns) is made of representatives: they answer as
//! any term. A triple term that names another identity has no id that stands for its
//! class (no read can intern one), so canonical answers are refused over a store holding
//! one (`nrese_engine::Snapshot::triple_terms_name_aliases`), as over a store with a
//! literal in a class, whose every computed literal would need the mapping
//! (`QueryEvaluationError::Unsupported`). Both are decided once, before planning.

use std::collections::HashMap;
use std::sync::Arc;

use nrese_engine::{EqualityClasses, Snapshot, TermId};
use nrese_rdf::{Literal, NamedNode, NamedOrBlankNode, Term, TermRef, Triple, Variable};
use nrese_sparql_syntax::algebra::{
    AggregateExpression, Expression, Function, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
use nrese_sparql_syntax::term::{
    GroundTerm, GroundTriple, NamedNodePattern, TermPattern, TriplePattern,
};

use super::Context;
use crate::results::QueryEvaluationError;

/// The canonical-name function: its argument's representative. Only canonical answers'
/// plans hold it.
pub(crate) const FUNCTION: &str = "urn:nrese:canonical-name";

/// The classes canonical answers name terms by, with the snapshot that decodes them.
#[derive(Clone)]
pub(crate) struct Names {
    snapshot: Snapshot,
    classes: Arc<EqualityClasses>,
}

impl Names {
    /// The names of canonical reads of `snapshot` in `model`, if some class renames
    /// anything.
    pub(super) fn of(snapshot: &Snapshot, model: nrese_engine::ReadModel) -> Option<Self> {
        Some(Self {
            snapshot: snapshot.clone(),
            classes: snapshot.canonical_classes(model)?,
        })
    }

    /// The id of `term`'s class, if the snapshot knows its name for it ([`Self::term`]).
    pub(super) fn id(&self, term: &Term) -> Option<u64> {
        let id = match term {
            Term::Triple(_) => self.snapshot.lookup(self.term(term.clone()).as_ref())?,
            _ => self.snapshot.lookup(term.as_ref())?,
        };
        Some(self.classes.representative(id).raw())
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

    /// `term` as canonical answers name it: a triple term over its parts' names.
    pub(crate) fn term(&self, term: Term) -> Term {
        if let Term::Triple(t) = term {
            let subject = NamedOrBlankNode::try_from(self.term(t.subject.into()));
            return match subject {
                Ok(subject) => {
                    Triple::new(subject, self.iri(t.predicate), self.term(t.object)).into()
                }
                Err(_) => unreachable!("an IRI's or blank node's representative names one"),
            };
        }
        match self.representative(term.as_ref()) {
            Some((_, representative)) => representative,
            None => term,
        }
    }

    fn iri(&self, n: NamedNode) -> NamedNode {
        match self.term(n.clone().into()) {
            Term::NamedNode(m) => m,
            _ => n,
        }
    }

    /// Pre-bound values as canonical answers name them.
    pub(super) fn values(&self, values: &HashMap<Variable, Term>) -> HashMap<Variable, Term> {
        values
            .iter()
            .map(|(v, t)| (v.clone(), self.term(t.clone())))
            .collect()
    }

    /// Graphs by the names canonical reads give them, each once.
    pub(super) fn graphs(&self, graphs: Vec<TermId>) -> Vec<TermId> {
        let mut graphs: Vec<TermId> = graphs
            .into_iter()
            .map(|g| self.classes.representative(g))
            .collect();
        graphs.sort_unstable();
        graphs.dedup();
        graphs
    }
}

impl Context<'_> {
    /// The query's pattern and form with its constants put in as their representatives,
    /// the CONSTRUCT template's too, `IRI()` wrapped in the canonical-name function (module
    /// docs); unchanged without canonical answers. An error for what canonical answers
    /// refuse.
    pub(super) fn canonical_query<'q>(
        &self,
        pattern: GraphPattern,
        form: super::Form<'q>,
    ) -> Result<(GraphPattern, super::Form<'q>), QueryEvaluationError> {
        let Some(names) = &self.canonical else {
            return Ok((pattern, form));
        };
        if names.classes.relate_literals() {
            return Err(refused("a literal is an identity of an owl:sameAs class"));
        }
        if self.snapshot.triple_terms_name_aliases() {
            return Err(refused(
                "a triple term the store holds names an identity other than its class's representative",
            ));
        }
        let rewrite = Rewrite { ctx: self, names };
        let form = match form {
            super::Form::Construct(template) => super::Form::Construct(std::borrow::Cow::Owned(
                template.iter().map(|t| rewrite.triple(t)).collect(),
            )),
            form => form,
        };
        Ok((rewrite.pattern(&pattern), form))
    }
}

fn refused(what: &str) -> QueryEvaluationError {
    QueryEvaluationError::Unsupported(format!(
        "canonical equality answers (equality.answers = \"canonical\"): {what}"
    ))
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
            TermPattern::Triple(inner) => {
                return TermPattern::Triple(Box::new(self.triple(inner)));
            }
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
            GroundTerm::Triple(t) => {
                return GroundTerm::Triple(Box::new(GroundTriple {
                    subject: self.iri(&t.subject),
                    predicate: self.iri(&t.predicate),
                    object: self.ground(&t.object),
                }));
            }
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
            Expression::FunctionCall(Function::Iri, args) => Expression::FunctionCall(
                Function::Custom(NamedNode::new_unchecked(FUNCTION)),
                vec![Expression::FunctionCall(Function::Iri, list(args))],
            ),
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
            // Named graphs are read over representatives too, their names included.
            GraphPattern::Graph { name, inner } => GraphPattern::Graph {
                name: self.named_node_pattern(name),
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
            // The endpoint knows no classes: its block goes as written.
            GraphPattern::Service { .. } => pattern.clone(),
        }
    }
}
