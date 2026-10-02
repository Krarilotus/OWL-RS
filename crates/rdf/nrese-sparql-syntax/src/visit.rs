//! Walking the algebra: the patterns and expressions directly in a node ([`children`]),
//! and searches over a whole tree ([`GraphPattern::find`], [`Expression::find`]) that reach
//! every pattern, every expression attached to one (filters, extensions, left-join
//! conditions, order keys, aggregates) and the patterns of `EXISTS`.
//!
//! Each node kind is enumerated once, here: an analysis that only looks for something
//! states what, not how to descend, and a new variant of the algebra fails to compile in
//! this module rather than being skipped by a wildcard somewhere else.
//!
//! [`children`]: GraphPattern::children

use crate::algebra::{AggregateExpression, Expression, GraphPattern};

/// A node of the algebra.
#[derive(Debug, Clone, Copy)]
pub enum Node<'a> {
    Pattern(&'a GraphPattern),
    Expression(&'a Expression),
}

impl GraphPattern {
    /// The nodes directly in this pattern: its sub-patterns and the expressions attached
    /// to it, in the order they are written.
    pub fn children(&self) -> Vec<Node<'_>> {
        match self {
            Self::Bgp { .. } | Self::Path { .. } | Self::Values { .. } => Vec::new(),
            Self::Join { left, right }
            | Self::Lateral { left, right }
            | Self::Union { left, right }
            | Self::Minus { left, right } => vec![Node::Pattern(left), Node::Pattern(right)],
            Self::LeftJoin {
                left,
                right,
                expression,
            } => {
                let mut nodes = vec![Node::Pattern(left), Node::Pattern(right)];
                nodes.extend(expression.as_ref().map(Node::Expression));
                nodes
            }
            Self::Filter { expr, inner } => vec![Node::Pattern(inner), Node::Expression(expr)],
            Self::Extend {
                inner, expression, ..
            } => vec![Node::Pattern(inner), Node::Expression(expression)],
            Self::OrderBy { inner, expression } => std::iter::once(Node::Pattern(inner))
                .chain(
                    expression
                        .iter()
                        .map(|order| Node::Expression(order.expression())),
                )
                .collect(),
            Self::Group {
                inner, aggregates, ..
            } => std::iter::once(Node::Pattern(inner))
                .chain(
                    aggregates
                        .iter()
                        .filter_map(|(_, aggregate)| match aggregate {
                            AggregateExpression::CountSolutions { .. } => None,
                            AggregateExpression::FunctionCall { expr, .. } => {
                                Some(Node::Expression(expr))
                            }
                        }),
                )
                .collect(),
            Self::Graph { inner, .. }
            | Self::Service { inner, .. }
            | Self::Project { inner, .. }
            | Self::Distinct { inner }
            | Self::Reduced { inner }
            | Self::Slice { inner, .. } => vec![Node::Pattern(inner)],
        }
    }

    /// Whether `f` holds for this pattern or any node in it (depth first, `EXISTS`
    /// patterns included).
    pub fn find(&self, f: &mut impl FnMut(Node<'_>) -> bool) -> bool {
        find(Node::Pattern(self), f)
    }
}

impl Expression {
    /// The nodes directly in this expression: its operands, or an `EXISTS`'s pattern.
    pub fn children(&self) -> Vec<Node<'_>> {
        match self {
            Self::NamedNode(_) | Self::Literal(_) | Self::Variable(_) | Self::Bound(_) => {
                Vec::new()
            }
            Self::Or(a, b)
            | Self::And(a, b)
            | Self::Equal(a, b)
            | Self::SameTerm(a, b)
            | Self::Greater(a, b)
            | Self::GreaterOrEqual(a, b)
            | Self::Less(a, b)
            | Self::LessOrEqual(a, b)
            | Self::Add(a, b)
            | Self::Subtract(a, b)
            | Self::Multiply(a, b)
            | Self::Divide(a, b) => vec![Node::Expression(a), Node::Expression(b)],
            Self::In(a, list) => std::iter::once(Node::Expression(a))
                .chain(list.iter().map(Node::Expression))
                .collect(),
            Self::UnaryPlus(a) | Self::UnaryMinus(a) | Self::Not(a) => vec![Node::Expression(a)],
            Self::If(a, b, c) => vec![
                Node::Expression(a),
                Node::Expression(b),
                Node::Expression(c),
            ],
            Self::Coalesce(list) | Self::FunctionCall(_, list) => {
                list.iter().map(Node::Expression).collect()
            }
            Self::Exists(pattern) => vec![Node::Pattern(pattern)],
        }
    }

    /// Whether `f` holds for this expression or any node in it (depth first, the patterns
    /// of `EXISTS` included).
    pub fn find(&self, f: &mut impl FnMut(Node<'_>) -> bool) -> bool {
        find(Node::Expression(self), f)
    }
}

fn find(node: Node<'_>, f: &mut impl FnMut(Node<'_>) -> bool) -> bool {
    if f(node) {
        return true;
    }
    let children = match node {
        Node::Pattern(pattern) => pattern.children(),
        Node::Expression(expression) => expression.children(),
    };
    children.into_iter().any(|child| find(child, f))
}

#[cfg(test)]
mod tests {
    use super::Node;
    use crate::SparqlParser;
    use crate::algebra::{Expression, GraphPattern};
    use crate::query::Query;

    fn pattern(text: &str) -> GraphPattern {
        match SparqlParser::new().parse_query(text).unwrap() {
            Query::Select { pattern, .. } => pattern,
            _ => unreachable!(),
        }
    }

    #[test]
    fn find_reaches_expressions_and_exists_patterns() {
        let query = pattern(
            "SELECT ?x (SUM(?n * 2) AS ?s) WHERE {
               ?x <urn:p> ?n FILTER EXISTS { ?x <urn:q> << ?a <urn:r> ?b >> }
             } GROUP BY ?x ORDER BY DESC(STRLEN(STR(?x)))",
        );
        let mut patterns = 0;
        let mut multiplications = 0;
        query.find(&mut |node| {
            match node {
                Node::Pattern(_) => patterns += 1,
                Node::Expression(Expression::Multiply(..)) => multiplications += 1,
                Node::Expression(_) => {}
            }
            false
        });
        assert_eq!(multiplications, 1, "in the aggregate");
        assert!(patterns >= 5);
        // The BGP inside the EXISTS is reached.
        let inner = query.find(&mut |node| {
            matches!(node, Node::Pattern(GraphPattern::Bgp { patterns })
                if patterns.iter().any(|t| t.predicate.to_string() == "<urn:q>"))
        });
        assert!(inner);
        // The order key's function is reached.
        assert!(
            query.find(&mut |node| matches!(node, Node::Expression(Expression::FunctionCall(..))))
        );
    }
}
