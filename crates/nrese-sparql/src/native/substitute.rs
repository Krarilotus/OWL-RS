//! `EXISTS` by substitution (SPARQL 1.1 §18.6): the pattern with an outer solution's
//! values put in for its variables, evaluated once per distinct outer key.
//!
//! [`super::exists`] evaluates most `EXISTS` patterns once and joins their solutions to the
//! outer ones. That gives the substitution's answer unless an outer variable is read
//! somewhere the pattern doesn't bind it first (a filter inside an OPTIONAL, a BIND, a
//! subquery): [`super::exists::correlation_safe`]. Those patterns come here.
//!
//! A value is put in as a constant. SPARQL has no syntax for a particular blank node, so a
//! blank node is put in as an alias IRI ([`ALIAS`] and its id), which the evaluation
//! context reads as the node itself: in patterns ([`Context::lookup_const`]) and in
//! expressions ([`super::expr::Evaluator`]'s aliases).

use std::collections::HashMap;

use nrese_rdf::{NamedNode, Term, Variable};
use nrese_sparql_syntax::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression,
};
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

/// The namespace of the alias IRIs that stand for blank nodes.
pub(super) const ALIAS: &str = "urn:nrese:substituted-blank-node:";

/// An outer solution's values, as constants to put in.
pub(super) struct Values<'a> {
    pub(super) terms: &'a HashMap<Variable, Term>,
}

impl Values<'_> {
    fn constant(&self, variable: &Variable) -> Option<Constant> {
        Some(match self.terms.get(variable)? {
            Term::NamedNode(n) => Constant::Iri(n.clone()),
            Term::Literal(l) => Constant::Literal(l.clone()),
            Term::BlankNode(b) => Constant::Iri(alias(b.as_str())),
            // Triple terms aren't substituted into the pattern: it stays a join.
            Term::Triple(_) => return None,
        })
    }

    fn term_pattern(&self, term: &TermPattern) -> TermPattern {
        match term {
            TermPattern::Variable(v) => match self.constant(v) {
                Some(Constant::Iri(n)) => TermPattern::NamedNode(n),
                Some(Constant::Literal(l)) => TermPattern::Literal(l),
                None => term.clone(),
            },
            other => other.clone(),
        }
    }

    fn named_node_pattern(&self, name: &NamedNodePattern) -> NamedNodePattern {
        match name {
            NamedNodePattern::Variable(v) => match self.constant(v) {
                Some(Constant::Iri(n)) => NamedNodePattern::NamedNode(n),
                // A literal where only an IRI may stand: a node no data has.
                Some(Constant::Literal(_)) => NamedNodePattern::NamedNode(alias("literal")),
                None => name.clone(),
            },
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

    pub(super) fn expression(&self, expression: &Expression) -> Expression {
        let e = |x: &Expression| Box::new(self.expression(x));
        match expression {
            Expression::Variable(v) => match self.constant(v) {
                Some(Constant::Iri(n)) => Expression::NamedNode(n),
                Some(Constant::Literal(l)) => Expression::Literal(l),
                None => expression.clone(),
            },
            Expression::Bound(v) if self.terms.contains_key(v) => {
                Expression::Literal(nrese_rdf::Literal::from(true))
            }
            Expression::NamedNode(_) | Expression::Literal(_) | Expression::Bound(_) => {
                expression.clone()
            }
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
            Expression::In(a, list) => {
                Expression::In(e(a), list.iter().map(|x| self.expression(x)).collect())
            }
            Expression::If(a, b, c) => Expression::If(e(a), e(b), e(c)),
            Expression::Coalesce(list) => {
                Expression::Coalesce(list.iter().map(|x| self.expression(x)).collect())
            }
            Expression::FunctionCall(function, args) => Expression::FunctionCall(
                function.clone(),
                args.iter().map(|x| self.expression(x)).collect(),
            ),
            Expression::Exists(pattern) => Expression::Exists(Box::new(self.pattern(pattern))),
        }
    }

    /// A whole query's pattern with the values put in: as [`Self::pattern`], except that
    /// the query's own projection doesn't hide them (they are pre-bound for the whole
    /// query); it only stops listing them.
    pub(super) fn top(&self, pattern: &GraphPattern) -> GraphPattern {
        match pattern {
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => GraphPattern::Slice {
                inner: Box::new(self.top(inner)),
                start: *start,
                length: *length,
            },
            GraphPattern::Distinct { inner } => GraphPattern::Distinct {
                inner: Box::new(self.top(inner)),
            },
            GraphPattern::Reduced { inner } => GraphPattern::Reduced {
                inner: Box::new(self.top(inner)),
            },
            GraphPattern::Project { inner, variables } => GraphPattern::Project {
                inner: Box::new(self.pattern(inner)),
                variables: variables
                    .iter()
                    .filter(|v| !self.terms.contains_key(*v))
                    .cloned()
                    .collect(),
            },
            other => self.pattern(other),
        }
    }

    /// `pattern` with the values put in.
    pub(super) fn pattern(&self, pattern: &GraphPattern) -> GraphPattern {
        let p = |x: &GraphPattern| Box::new(self.pattern(x));
        let kept = |variables: &[Variable]| -> Vec<Variable> {
            variables
                .iter()
                .filter(|v| !self.terms.contains_key(*v))
                .cloned()
                .collect()
        };
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
                path: path.clone(),
                object: self.term_pattern(object),
            },
            GraphPattern::Lateral { left, right } => GraphPattern::Lateral {
                left: p(left),
                right: p(right),
            },
            GraphPattern::Join { left, right } => GraphPattern::Join {
                left: p(left),
                right: p(right),
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
            GraphPattern::Filter { expr, inner } => GraphPattern::Filter {
                expr: self.expression(expr),
                inner: p(inner),
            },
            GraphPattern::Union { left, right } => GraphPattern::Union {
                left: p(left),
                right: p(right),
            },
            GraphPattern::Graph { name, inner } => GraphPattern::Graph {
                name: self.named_node_pattern(name),
                inner: p(inner),
            },
            // Binding a variable the outer solution binds: the value must be the same.
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => match self.constant(variable) {
                Some(constant) => GraphPattern::Filter {
                    expr: Expression::SameTerm(
                        Box::new(self.expression(expression)),
                        Box::new(constant.expression()),
                    ),
                    inner: p(inner),
                },
                None => GraphPattern::Extend {
                    inner: p(inner),
                    variable: variable.clone(),
                    expression: self.expression(expression),
                },
            },
            GraphPattern::Minus { left, right } => GraphPattern::Minus {
                left: p(left),
                right: p(right),
            },
            // The rows that agree with the outer values, without their columns.
            GraphPattern::Values {
                variables,
                bindings,
            } => {
                let put: Vec<(usize, Option<Constant>)> = variables
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| self.terms.contains_key(*v))
                    .map(|(i, v)| (i, self.constant(v)))
                    .collect();
                let agrees = |row: &[Option<GroundTerm>]| {
                    put.iter().all(|(i, constant)| match (&row[*i], constant) {
                        (None, _) => true,
                        (Some(GroundTerm::NamedNode(n)), Some(Constant::Iri(m))) => n == m,
                        (Some(GroundTerm::Literal(l)), Some(Constant::Literal(m))) => l == m,
                        _ => false,
                    })
                };
                GraphPattern::Values {
                    variables: kept(variables),
                    bindings: bindings
                        .iter()
                        .filter(|row| agrees(row))
                        .map(|row| {
                            row.iter()
                                .enumerate()
                                .filter(|(i, _)| !put.iter().any(|(j, _)| j == i))
                                .map(|(_, value)| value.clone())
                                .collect()
                        })
                        .collect(),
                }
            }
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
            // A subquery's variables are the outer ones only where it projects them.
            GraphPattern::Project { inner, variables } => {
                let visible: HashMap<Variable, Term> = self
                    .terms
                    .iter()
                    .filter(|(v, _)| variables.contains(v))
                    .map(|(v, t)| (v.clone(), t.clone()))
                    .collect();
                GraphPattern::Project {
                    inner: Box::new(Values { terms: &visible }.pattern(inner)),
                    variables: kept(variables),
                }
            }
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
            // A group key put in stays a key, bound to its value before the grouping: over
            // no rows, GROUP BY with keys gives no group (no row), unlike without keys.
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => GraphPattern::Group {
                inner: Box::new(variables.iter().fold(self.pattern(inner), |grouped, v| {
                    match self.constant(v) {
                        Some(constant) => GraphPattern::Extend {
                            inner: Box::new(grouped),
                            variable: v.clone(),
                            expression: constant.expression(),
                        },
                        None => grouped,
                    }
                })),
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
            GraphPattern::Service {
                name,
                inner,
                silent,
            } => GraphPattern::Service {
                name: self.named_node_pattern(name),
                inner: p(inner),
                silent: *silent,
            },
        }
    }
}

enum Constant {
    Iri(NamedNode),
    Literal(nrese_rdf::Literal),
}

impl Constant {
    fn expression(self) -> Expression {
        match self {
            Constant::Iri(n) => Expression::NamedNode(n),
            Constant::Literal(l) => Expression::Literal(l),
        }
    }
}

/// The alias IRI of blank node `label`.
pub(super) fn alias(label: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{ALIAS}{label}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_sparql_syntax::SparqlParser;

    #[test]
    fn values_go_in_everywhere_a_variable_is_read() {
        let query = SparqlParser::new()
            .parse_query(
                "SELECT * WHERE { ?x <http://e/p> ?y OPTIONAL { ?y <http://e/q> ?z FILTER(?z > ?o) } BIND(?o AS ?w) VALUES ?o { 1 2 } }",
            )
            .unwrap();
        let nrese_sparql_syntax::Query::Select { pattern, .. } = query else {
            panic!()
        };
        let terms = HashMap::from([
            (
                Variable::new_unchecked("o"),
                Term::from(nrese_rdf::Literal::from(1)),
            ),
            (
                Variable::new_unchecked("x"),
                Term::from(nrese_rdf::BlankNode::new_unchecked("b7")),
            ),
        ]);
        let text = Values { terms: &terms }.pattern(&pattern).to_string();
        assert!(!text.contains("?o"), "{text}");
        assert!(text.contains(&format!("<{ALIAS}b7>")), "{text}");
        // VALUES keeps the agreeing row and drops the column.
        assert!(!text.contains(" 2 "), "{text}");
    }
}
