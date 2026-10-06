//! The key of a plan part in the result cache ([`crate::cache`]): the part's algebra
//! written as bytes, its variables numbered in the order they first occur, so that parts
//! that differ only in their variables' names share one entry (`?s ?p ?o` and `?x ?y ?z`,
//! or two parses' generated names).
//!
//! A part that must not be cached has no key: one that reads a `SERVICE` (the remote data
//! changes without this store's revision), calls `RAND`, `NOW`, `UUID`, `STRUUID` or
//! `BNODE` (a new value per evaluation), or names a blank node put in by substitution
//! (an alias whose meaning is the query's own, [`super::substitute`]).
//!
//! The encoding is injective: every node writes a tag, every string its length first, so
//! two different parts never share bytes. Renaming variables consistently doesn't change
//! what a part computes, given the context in which it is evaluated, which the key
//! includes too ([`super::Context::cache_key`]).

use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{
    AggregateExpression, Expression, Function, GraphPattern, OrderExpression,
};
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

/// Writes a part's key; [`Encoder::vars`] are its variables by number.
#[derive(Default)]
pub(super) struct Encoder {
    pub(super) bytes: Vec<u8>,
    /// The part's variables in the order they first occur: a variable's number is its
    /// position.
    pub(super) vars: Vec<Variable>,
}

/// Why a part has no key.
pub(super) struct Uncacheable;

type Encoded = Result<(), Uncacheable>;

impl Encoder {
    pub(super) fn tag(&mut self, tag: u8) {
        self.bytes.push(tag);
    }

    pub(super) fn number(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn text(&mut self, text: &str) {
        self.number(text.len() as u64);
        self.bytes.extend_from_slice(text.as_bytes());
    }

    /// A variable by its number.
    pub(super) fn var(&mut self, variable: &Variable) {
        let number = match self.vars.iter().position(|v| v == variable) {
            Some(number) => number,
            None => {
                self.vars.push(variable.clone());
                self.vars.len() - 1
            }
        };
        self.tag(b'?');
        self.number(number as u64);
    }

    fn iri(&mut self, iri: &str) -> Encoded {
        if iri.starts_with(super::substitute::ALIAS) {
            return Err(Uncacheable);
        }
        self.tag(b'<');
        self.text(iri);
        Ok(())
    }

    fn display(&mut self, tag: u8, value: impl std::fmt::Display) {
        self.tag(tag);
        self.text(&value.to_string());
    }

    fn term(&mut self, term: &TermPattern) -> Encoded {
        match term {
            TermPattern::NamedNode(n) => self.iri(n.as_str())?,
            // A blank node of a pattern is a variable the executor names `_bnode_…`.
            TermPattern::BlankNode(b) => {
                self.var(&Variable::new_unchecked(format!("_bnode_{}", b.as_str())));
            }
            TermPattern::Literal(l) => self.display(b'"', l),
            TermPattern::Triple(t) => {
                self.tag(b'T');
                self.triple(t)?;
            }
            TermPattern::Variable(v) => self.var(v),
        }
        Ok(())
    }

    fn named(&mut self, name: &NamedNodePattern) -> Encoded {
        match name {
            NamedNodePattern::NamedNode(n) => self.iri(n.as_str()),
            NamedNodePattern::Variable(v) => {
                self.var(v);
                Ok(())
            }
        }
    }

    pub(super) fn triple(&mut self, triple: &TriplePattern) -> Encoded {
        self.term(&triple.subject)?;
        self.named(&triple.predicate)?;
        self.term(&triple.object)
    }

    fn ground(&mut self, term: &GroundTerm) -> Encoded {
        if let GroundTerm::NamedNode(n) = term {
            return self.iri(n.as_str());
        }
        self.display(b'g', term);
        Ok(())
    }

    fn vars<'v>(&mut self, variables: impl IntoIterator<Item = &'v Variable>) {
        let variables: Vec<_> = variables.into_iter().collect();
        self.number(variables.len() as u64);
        for v in variables {
            self.var(v);
        }
    }

    fn patterns(&mut self, tag: u8, patterns: &[&GraphPattern]) -> Encoded {
        self.tag(tag);
        for p in patterns {
            self.pattern(p)?;
        }
        Ok(())
    }

    /// Writes `pattern`.
    pub(super) fn pattern(&mut self, pattern: &GraphPattern) -> Encoded {
        match pattern {
            GraphPattern::Bgp { patterns } => {
                self.tag(b'B');
                self.number(patterns.len() as u64);
                for triple in patterns {
                    self.triple(triple)?;
                }
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => {
                self.tag(b'P');
                self.term(subject)?;
                // Paths name no variables; their IRIs can't be aliases (only terms of
                // patterns and expressions are substituted).
                self.display(b'p', format_args!("{path:?}"));
                self.term(object)?;
            }
            GraphPattern::Join { left, right } => self.patterns(b'J', &[left, right])?,
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.patterns(b'L', &[left, right])?;
                self.optional_expression(expression.as_ref())?;
            }
            GraphPattern::Lateral { left, right } => self.patterns(b'A', &[left, right])?,
            GraphPattern::Filter { expr, inner } => {
                self.patterns(b'F', &[inner])?;
                self.expression(expr)?;
            }
            GraphPattern::Union { left, right } => self.patterns(b'U', &[left, right])?,
            GraphPattern::Graph { name, inner } => {
                self.tag(b'G');
                self.named(name)?;
                self.pattern(inner)?;
            }
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                self.patterns(b'E', &[inner])?;
                self.var(variable);
                self.expression(expression)?;
            }
            GraphPattern::Minus { left, right } => self.patterns(b'M', &[left, right])?,
            GraphPattern::Values {
                variables,
                bindings,
            } => {
                self.tag(b'V');
                self.vars(variables);
                self.number(bindings.len() as u64);
                for row in bindings {
                    for value in row {
                        match value {
                            Some(term) => self.ground(term)?,
                            None => self.tag(b'_'),
                        }
                    }
                }
            }
            GraphPattern::OrderBy { inner, expression } => {
                self.patterns(b'O', &[inner])?;
                self.number(expression.len() as u64);
                for key in expression {
                    let (tag, e) = match key {
                        OrderExpression::Asc(e) => (b'a', e),
                        OrderExpression::Desc(e) => (b'd', e),
                    };
                    self.tag(tag);
                    self.expression(e)?;
                }
            }
            GraphPattern::Project { inner, variables } => {
                self.patterns(b'R', &[inner])?;
                self.vars(variables);
            }
            GraphPattern::Distinct { inner } => self.patterns(b'D', &[inner])?,
            GraphPattern::Reduced { inner } => self.patterns(b'r', &[inner])?,
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => {
                self.patterns(b'S', &[inner])?;
                self.number(*start as u64);
                self.number(length.map_or(u64::MAX, |l| l as u64));
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => {
                self.patterns(b'Y', &[inner])?;
                self.vars(variables);
                self.number(aggregates.len() as u64);
                for (variable, aggregate) in aggregates {
                    self.var(variable);
                    match aggregate {
                        AggregateExpression::CountSolutions { distinct } => {
                            self.tag(b'#');
                            self.tag(u8::from(*distinct));
                        }
                        AggregateExpression::FunctionCall {
                            name,
                            expr,
                            distinct,
                        } => {
                            self.display(b'f', format_args!("{name:?}"));
                            self.tag(u8::from(*distinct));
                            self.expression(expr)?;
                        }
                    }
                }
            }
            GraphPattern::Service { .. } => return Err(Uncacheable),
        }
        Ok(())
    }

    fn optional_expression(&mut self, expression: Option<&Expression>) -> Encoded {
        match expression {
            Some(e) => {
                self.tag(1);
                self.expression(e)
            }
            None => {
                self.tag(0);
                Ok(())
            }
        }
    }

    fn expressions(&mut self, tag: u8, expressions: &[&Expression]) -> Encoded {
        self.tag(tag);
        for e in expressions {
            self.expression(e)?;
        }
        Ok(())
    }

    fn list(&mut self, expressions: &[Expression]) -> Encoded {
        self.number(expressions.len() as u64);
        for e in expressions {
            self.expression(e)?;
        }
        Ok(())
    }

    pub(super) fn expression(&mut self, expression: &Expression) -> Encoded {
        match expression {
            Expression::NamedNode(n) => self.iri(n.as_str())?,
            Expression::Literal(l) => self.display(b'"', l),
            Expression::Variable(v) => self.var(v),
            Expression::Or(a, b) => self.expressions(b'|', &[a, b])?,
            Expression::And(a, b) => self.expressions(b'&', &[a, b])?,
            Expression::Equal(a, b) => self.expressions(b'=', &[a, b])?,
            Expression::SameTerm(a, b) => self.expressions(b's', &[a, b])?,
            Expression::Greater(a, b) => self.expressions(b'>', &[a, b])?,
            Expression::GreaterOrEqual(a, b) => self.expressions(b'}', &[a, b])?,
            Expression::Less(a, b) => self.expressions(b'<', &[a, b])?,
            Expression::LessOrEqual(a, b) => self.expressions(b'{', &[a, b])?,
            Expression::In(a, list) => {
                self.expressions(b'i', &[a])?;
                self.list(list)?;
            }
            Expression::Add(a, b) => self.expressions(b'+', &[a, b])?,
            Expression::Subtract(a, b) => self.expressions(b'-', &[a, b])?,
            Expression::Multiply(a, b) => self.expressions(b'*', &[a, b])?,
            Expression::Divide(a, b) => self.expressions(b'/', &[a, b])?,
            Expression::UnaryPlus(a) => self.expressions(b'u', &[a])?,
            Expression::UnaryMinus(a) => self.expressions(b'n', &[a])?,
            Expression::Not(a) => self.expressions(b'!', &[a])?,
            Expression::Exists(pattern) => {
                self.tag(b'X');
                self.pattern(pattern)?;
            }
            Expression::Bound(v) => {
                self.tag(b'b');
                self.var(v);
            }
            Expression::If(a, b, c) => self.expressions(b'?', &[a, b, c])?,
            Expression::Coalesce(list) => {
                self.tag(b'c');
                self.list(list)?;
            }
            Expression::FunctionCall(function, args) => {
                if volatile(function) {
                    return Err(Uncacheable);
                }
                match function {
                    Function::Custom(iri) => {
                        self.tag(b'C');
                        self.iri(iri.as_str())?;
                    }
                    other => self.display(b'(', other),
                }
                self.list(args)?;
            }
        }
        Ok(())
    }
}

/// Functions with a new value per evaluation (`NOW` is one per query, not per data).
fn volatile(function: &Function) -> bool {
    matches!(
        function,
        Function::Rand | Function::Uuid | Function::StrUuid | Function::BNode | Function::Now
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_sparql_syntax::Query;

    fn key(query: &str) -> Option<Vec<u8>> {
        let Query::Select { pattern, .. } = crate::compat::parse_query(query, None).unwrap() else {
            panic!("a SELECT")
        };
        let mut encoder = Encoder::default();
        encoder.pattern(&pattern).ok().map(|()| encoder.bytes)
    }

    #[test]
    fn parts_differing_in_variable_names_share_a_key() {
        let a = key("SELECT ?s ?o ?x { ?s <http://e/p> ?o . ?o <http://e/q> ?x }");
        assert!(a.is_some());
        assert_eq!(
            a,
            key("SELECT ?a ?b ?c { ?a <http://e/p> ?b . ?b <http://e/q> ?c }")
        );
        // The join structure counts: ?o joins here, not there.
        let other = key("SELECT ?s ?o ?x { ?s <http://e/p> ?o . ?x <http://e/q> ?o }");
        assert_ne!(a, other);
        // And the order of the columns (`SELECT *` orders them by name).
        assert_ne!(
            a,
            key("SELECT ?s ?x ?o { ?s <http://e/p> ?o . ?o <http://e/q> ?x }")
        );
        // Aggregates' generated names are numbered like any other.
        assert_eq!(
            key("SELECT (COUNT(*) AS ?n) { ?s ?p ?o }"),
            key("SELECT (COUNT(*) AS ?m) { ?x ?y ?z }")
        );
        assert_ne!(
            key("SELECT (COUNT(*) AS ?n) { ?s ?p ?o }"),
            key("SELECT (COUNT(DISTINCT ?s) AS ?n) { ?s ?p ?o }")
        );
    }

    #[test]
    fn volatile_and_remote_parts_have_no_key() {
        for query in [
            "SELECT ?r { BIND(RAND() AS ?r) }",
            "SELECT ?r { BIND(NOW() AS ?r) }",
            "SELECT ?r { BIND(UUID() AS ?r) }",
            "SELECT ?r { BIND(STRUUID() AS ?r) }",
            "SELECT ?r { BIND(BNODE() AS ?r) }",
            "SELECT * { ?s ?p ?o FILTER EXISTS { ?s ?p ?x FILTER(?x < RAND()) } }",
            "SELECT * { SERVICE <http://example.com/sparql> { ?s ?p ?o } }",
            "SELECT * { ?s ?p <urn:nrese:substituted-blank-node:b1> }",
        ] {
            assert!(key(query).is_none(), "{query}");
        }
        assert!(key("SELECT * { ?s ?p \"RAND()\" }").is_some());
    }
}
