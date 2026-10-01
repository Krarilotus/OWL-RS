//! SPARQL 1.2 triple-term patterns with variables, as plain algebra (step 6 of the
//! migration plan).
//!
//! RDF 1.2 allows a triple term only as an object, so the scan of the triple pattern it
//! stands in always binds it. A triple term pattern with variables (or blank nodes) inside
//! becomes a fresh variable in that scan, and its parts are taken apart with `SUBJECT`,
//! `PREDICATE` and `OBJECT`: a `BIND` where a variable is first bound, a
//! `FILTER(sameTerm(…))` where it is bound already, and `isTRIPLE` on the fresh variable.
//! Nested triple term patterns get fresh variables of their own the same way. Every
//! operator and optimisation of the executor then applies unchanged; ground triple terms
//! stay constants (dictionary lookups).

use std::collections::HashSet;

use nrese_rdf::{BlankNode, Variable};
use nrese_sparql_syntax::algebra::{Expression, Function, GraphPattern};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};

/// The variable a blank node of a pattern stands for in the executor.
pub(super) fn blank_variable(b: &BlankNode) -> Variable {
    Variable::new_unchecked(format!("_bnode_{}", b.as_str()))
}

/// Whether a triple term pattern holds no variable and no blank node.
pub(super) fn ground(t: &TriplePattern) -> bool {
    let part = |p: &TermPattern| match p {
        TermPattern::NamedNode(_) | TermPattern::Literal(_) => true,
        TermPattern::Triple(t) => ground(t),
        TermPattern::Variable(_) | TermPattern::BlankNode(_) => false,
    };
    part(&t.subject) && matches!(t.predicate, NamedNodePattern::NamedNode(_)) && part(&t.object)
}

fn open(term: &TermPattern) -> bool {
    matches!(term, TermPattern::Triple(t) if !ground(t))
}

/// Whether `pattern` (its expressions' `EXISTS` included) has a triple term pattern with
/// variables in a basic graph pattern.
pub(super) fn has_open(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => {
            patterns.iter().any(|t| open(&t.subject) || open(&t.object))
        }
        GraphPattern::Path { .. } | GraphPattern::Values { .. } => false,
        GraphPattern::Join { left, right }
        | GraphPattern::Lateral { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => has_open(left) || has_open(right),
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => has_open(left) || has_open(right) || expression.as_ref().is_some_and(has_open_expr),
        GraphPattern::Filter { expr, inner } => has_open_expr(expr) || has_open(inner),
        GraphPattern::Extend {
            inner, expression, ..
        } => has_open_expr(expression) || has_open(inner),
        GraphPattern::Graph { inner, .. }
        | GraphPattern::Service { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Group { inner, .. } => has_open(inner),
    }
}

fn has_open_expr(expression: &Expression) -> bool {
    let mut found = false;
    visit_exists(expression, &mut |p| found |= has_open(p));
    found
}

fn visit_exists(expression: &Expression, f: &mut impl FnMut(&GraphPattern)) {
    match expression {
        Expression::Exists(p) => f(p),
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Variable(_)
        | Expression::Bound(_) => {}
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            visit_exists(a, f);
            visit_exists(b, f);
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            visit_exists(a, f);
        }
        Expression::In(a, list) => {
            visit_exists(a, f);
            list.iter().for_each(|e| visit_exists(e, f));
        }
        Expression::If(a, b, c) => {
            visit_exists(a, f);
            visit_exists(b, f);
            visit_exists(c, f);
        }
        Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
            list.iter().for_each(|e| visit_exists(e, f));
        }
    }
}

/// `pattern` with every open triple term pattern rewritten (see the module's notes).
pub(super) fn rewrite(pattern: &GraphPattern) -> GraphPattern {
    Rewriter { next: 0 }.pattern(pattern)
}

struct Rewriter {
    next: usize,
}

impl Rewriter {
    fn fresh(&mut self) -> Variable {
        let v = Variable::new_unchecked(format!("_triple_term_{}", self.next));
        self.next += 1;
        v
    }

    fn pattern(&mut self, pattern: &GraphPattern) -> GraphPattern {
        let b = |r: &mut Self, p: &GraphPattern| Box::new(r.pattern(p));
        match pattern {
            GraphPattern::Bgp { patterns } => self.bgp(patterns),
            GraphPattern::Path { .. } | GraphPattern::Values { .. } => pattern.clone(),
            GraphPattern::Join { left, right } => GraphPattern::Join {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::Lateral { left, right } => GraphPattern::Lateral {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::Union { left, right } => GraphPattern::Union {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::Minus { left, right } => GraphPattern::Minus {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => GraphPattern::LeftJoin {
                left: b(self, left),
                right: b(self, right),
                expression: expression.as_ref().map(|e| self.expression(e)),
            },
            GraphPattern::Filter { expr, inner } => GraphPattern::Filter {
                expr: self.expression(expr),
                inner: b(self, inner),
            },
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => GraphPattern::Extend {
                inner: b(self, inner),
                variable: variable.clone(),
                expression: self.expression(expression),
            },
            GraphPattern::Graph { name, inner } => GraphPattern::Graph {
                name: name.clone(),
                inner: b(self, inner),
            },
            GraphPattern::Service {
                name,
                inner,
                silent,
            } => GraphPattern::Service {
                name: name.clone(),
                inner: b(self, inner),
                silent: *silent,
            },
            GraphPattern::OrderBy { inner, expression } => GraphPattern::OrderBy {
                inner: b(self, inner),
                expression: expression.clone(),
            },
            GraphPattern::Project { inner, variables } => GraphPattern::Project {
                inner: b(self, inner),
                variables: variables.clone(),
            },
            GraphPattern::Distinct { inner } => GraphPattern::Distinct {
                inner: b(self, inner),
            },
            GraphPattern::Reduced { inner } => GraphPattern::Reduced {
                inner: b(self, inner),
            },
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => GraphPattern::Slice {
                inner: b(self, inner),
                start: *start,
                length: *length,
            },
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => GraphPattern::Group {
                inner: b(self, inner),
                variables: variables.clone(),
                aggregates: aggregates.clone(),
            },
        }
    }

    /// `EXISTS` patterns inside an expression, rewritten.
    fn expression(&mut self, expression: &Expression) -> Expression {
        if !has_open_expr(expression) {
            return expression.clone();
        }
        let e = |r: &mut Self, x: &Expression| Box::new(r.expression(x));
        let list = |r: &mut Self, l: &[Expression]| l.iter().map(|x| r.expression(x)).collect();
        match expression {
            Expression::Exists(p) => Expression::Exists(Box::new(self.pattern(p))),
            Expression::Or(a, b) => Expression::Or(e(self, a), e(self, b)),
            Expression::And(a, b) => Expression::And(e(self, a), e(self, b)),
            Expression::Equal(a, b) => Expression::Equal(e(self, a), e(self, b)),
            Expression::SameTerm(a, b) => Expression::SameTerm(e(self, a), e(self, b)),
            Expression::Greater(a, b) => Expression::Greater(e(self, a), e(self, b)),
            Expression::GreaterOrEqual(a, b) => Expression::GreaterOrEqual(e(self, a), e(self, b)),
            Expression::Less(a, b) => Expression::Less(e(self, a), e(self, b)),
            Expression::LessOrEqual(a, b) => Expression::LessOrEqual(e(self, a), e(self, b)),
            Expression::Add(a, b) => Expression::Add(e(self, a), e(self, b)),
            Expression::Subtract(a, b) => Expression::Subtract(e(self, a), e(self, b)),
            Expression::Multiply(a, b) => Expression::Multiply(e(self, a), e(self, b)),
            Expression::Divide(a, b) => Expression::Divide(e(self, a), e(self, b)),
            Expression::UnaryPlus(a) => Expression::UnaryPlus(e(self, a)),
            Expression::UnaryMinus(a) => Expression::UnaryMinus(e(self, a)),
            Expression::Not(a) => Expression::Not(e(self, a)),
            Expression::In(a, l) => Expression::In(e(self, a), list(self, l)),
            Expression::If(a, b, c) => Expression::If(e(self, a), e(self, b), e(self, c)),
            Expression::Coalesce(l) => Expression::Coalesce(list(self, l)),
            Expression::FunctionCall(f, l) => Expression::FunctionCall(f.clone(), list(self, l)),
            other => other.clone(),
        }
    }

    /// A basic graph pattern: open triple terms replaced by fresh variables, then the
    /// `BIND`s of their parts' first occurrences, then one filter on the rest.
    fn bgp(&mut self, patterns: &[TriplePattern]) -> GraphPattern {
        if !patterns.iter().any(|t| open(&t.subject) || open(&t.object)) {
            return GraphPattern::Bgp {
                patterns: patterns.to_vec(),
            };
        }
        // What the plain positions of the pattern bind.
        let mut bound: HashSet<Variable> = HashSet::new();
        for t in patterns {
            for term in [&t.subject, &t.object] {
                match term {
                    TermPattern::Variable(v) => {
                        bound.insert(v.clone());
                    }
                    TermPattern::BlankNode(b) => {
                        bound.insert(blank_variable(b));
                    }
                    _ => {}
                }
            }
            if let NamedNodePattern::Variable(v) = &t.predicate {
                bound.insert(v.clone());
            }
        }
        let mut parts = Parts {
            bound,
            extends: Vec::new(),
            filters: Vec::new(),
        };
        let mut rewritten = Vec::with_capacity(patterns.len());
        for t in patterns {
            let mut lift = |r: &mut Self, term: &TermPattern| -> TermPattern {
                match term {
                    TermPattern::Triple(inner) if !ground(inner) => {
                        let v = r.fresh();
                        parts.bound.insert(v.clone());
                        r.take_apart(&v, inner, &mut parts);
                        v.into()
                    }
                    other => other.clone(),
                }
            };
            let subject = lift(self, &t.subject);
            let object = lift(self, &t.object);
            rewritten.push(TriplePattern {
                subject,
                predicate: t.predicate.clone(),
                object,
            });
        }
        let mut pattern = GraphPattern::Bgp {
            patterns: rewritten,
        };
        for (variable, expression) in parts.extends {
            pattern = GraphPattern::Extend {
                inner: Box::new(pattern),
                variable,
                expression,
            };
        }
        let filter = parts
            .filters
            .into_iter()
            .reduce(|a, b| Expression::And(Box::new(a), Box::new(b)));
        match filter {
            Some(expr) => GraphPattern::Filter {
                expr,
                inner: Box::new(pattern),
            },
            None => pattern,
        }
    }

    /// The conditions that make the term bound to `whole` match `pattern`.
    fn take_apart(&mut self, whole: &Variable, pattern: &TriplePattern, parts: &mut Parts) {
        let call = |f: Function| Expression::FunctionCall(f, vec![whole.clone().into()]);
        parts.filters.push(Expression::FunctionCall(
            Function::IsTriple,
            vec![whole.clone().into()],
        ));
        let predicate = match &pattern.predicate {
            NamedNodePattern::NamedNode(n) => TermPattern::NamedNode(n.clone()),
            NamedNodePattern::Variable(v) => TermPattern::Variable(v.clone()),
        };
        for (function, part) in [
            (Function::Subject, &pattern.subject),
            (Function::Predicate, &predicate),
            (Function::Object, &pattern.object),
        ] {
            let value = call(function);
            match part {
                TermPattern::Variable(v) => parts.bind(v.clone(), value),
                TermPattern::BlankNode(b) => parts.bind(blank_variable(b), value),
                TermPattern::NamedNode(n) => parts.same(value, n.clone().into()),
                TermPattern::Literal(l) => parts.same(value, l.clone().into()),
                TermPattern::Triple(inner) if ground(inner) => {
                    parts.same(value, constant_triple(inner));
                }
                TermPattern::Triple(inner) => {
                    let v = self.fresh();
                    parts.bind(v.clone(), value);
                    self.take_apart(&v, inner, parts);
                }
            }
        }
    }
}

/// The `BIND`s and filters a basic graph pattern's triple terms add.
struct Parts {
    bound: HashSet<Variable>,
    extends: Vec<(Variable, Expression)>,
    filters: Vec<Expression>,
}

impl Parts {
    /// `variable` must equal `value`: bound here if nothing binds it yet, else compared.
    fn bind(&mut self, variable: Variable, value: Expression) {
        if self.bound.insert(variable.clone()) {
            self.extends.push((variable, value));
        } else {
            self.same(value, variable.into());
        }
    }

    fn same(&mut self, a: Expression, b: Expression) {
        self.filters
            .push(Expression::SameTerm(Box::new(a), Box::new(b)));
    }
}

/// A ground triple term pattern as an expression: `TRIPLE(s, p, o)` of its constants.
fn constant_triple(t: &TriplePattern) -> Expression {
    let part = |p: &TermPattern| -> Expression {
        match p {
            TermPattern::NamedNode(n) => n.clone().into(),
            TermPattern::Literal(l) => l.clone().into(),
            TermPattern::Triple(t) => constant_triple(t),
            // Not reached: the pattern is ground.
            TermPattern::Variable(v) => v.clone().into(),
            TermPattern::BlankNode(b) => blank_variable(b).into(),
        }
    };
    let predicate: Expression = match &t.predicate {
        NamedNodePattern::NamedNode(n) => n.clone().into(),
        NamedNodePattern::Variable(v) => v.clone().into(),
    };
    Expression::FunctionCall(
        Function::Triple,
        vec![part(&t.subject), predicate, part(&t.object)],
    )
}
