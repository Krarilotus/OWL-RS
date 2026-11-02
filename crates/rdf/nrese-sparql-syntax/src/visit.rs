//! Walking the algebra: the patterns and expressions directly in a node ([`children`], and
//! [`children_mut`] to change them in place), and searches over a whole tree
//! ([`GraphPattern::find`], [`Expression::find`]) that reach every pattern, every expression
//! attached to one (filters, extensions, left-join conditions, order keys, aggregates) and
//! the patterns of `EXISTS`.
//!
//! Each node kind is enumerated here, by reference and by mutable reference alike: an
//! analysis that only looks for something states what, not how to descend, and a new variant
//! of the algebra fails to compile in this module rather than being skipped by a wildcard
//! somewhere else.
//!
//! [`children`]: GraphPattern::children
//! [`children_mut`]: GraphPattern::children_mut

use crate::algebra::{AggregateExpression, Expression, GraphPattern, PropertyPathExpression};

/// A node of the algebra.
#[derive(Debug, Clone, Copy)]
pub enum Node<'a> {
    Pattern(&'a GraphPattern),
    Expression(&'a Expression),
}

/// A node of the algebra, to change in place.
#[derive(Debug)]
pub enum NodeMut<'a> {
    Pattern(&'a mut GraphPattern),
    Expression(&'a mut Expression),
}

impl GraphPattern {
    /// The nodes directly in this pattern: its sub-patterns and the expressions attached
    /// to it, in the order they are written.
    pub fn children(&self) -> Vec<Node<'_>> {
        match self {
            Self::Bgp { .. } | Self::Path { .. } | Self::Values { .. } => Vec::new(),
            Self::Join { inputs } | Self::Union { inputs } => {
                inputs.iter().map(Node::Pattern).collect()
            }
            Self::Lateral { left, right } | Self::Minus { left, right } => {
                vec![Node::Pattern(left), Node::Pattern(right)]
            }
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
            Self::Extend { inner, bindings } => std::iter::once(Node::Pattern(inner))
                .chain(bindings.iter().map(|(_, e)| Node::Expression(e)))
                .collect(),
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

    /// [`Self::children`], to change in place.
    pub fn children_mut(&mut self) -> Vec<NodeMut<'_>> {
        match self {
            Self::Bgp { .. } | Self::Path { .. } | Self::Values { .. } => Vec::new(),
            Self::Join { inputs } | Self::Union { inputs } => {
                inputs.iter_mut().map(NodeMut::Pattern).collect()
            }
            Self::Lateral { left, right } | Self::Minus { left, right } => {
                vec![NodeMut::Pattern(left), NodeMut::Pattern(right)]
            }
            Self::LeftJoin {
                left,
                right,
                expression,
            } => {
                let mut nodes = vec![NodeMut::Pattern(left), NodeMut::Pattern(right)];
                nodes.extend(expression.as_mut().map(NodeMut::Expression));
                nodes
            }
            Self::Filter { expr, inner } => {
                vec![NodeMut::Pattern(inner), NodeMut::Expression(expr)]
            }
            Self::Extend { inner, bindings } => std::iter::once(NodeMut::Pattern(inner))
                .chain(bindings.iter_mut().map(|(_, e)| NodeMut::Expression(e)))
                .collect(),
            Self::OrderBy { inner, expression } => std::iter::once(NodeMut::Pattern(inner))
                .chain(
                    expression
                        .iter_mut()
                        .map(|order| NodeMut::Expression(order.expression_mut())),
                )
                .collect(),
            Self::Group {
                inner, aggregates, ..
            } => std::iter::once(NodeMut::Pattern(inner))
                .chain(
                    aggregates
                        .iter_mut()
                        .filter_map(|(_, aggregate)| match aggregate {
                            AggregateExpression::CountSolutions { .. } => None,
                            AggregateExpression::FunctionCall { expr, .. } => {
                                Some(NodeMut::Expression(expr))
                            }
                        }),
                )
                .collect(),
            Self::Graph { inner, .. }
            | Self::Service { inner, .. }
            | Self::Project { inner, .. }
            | Self::Distinct { inner }
            | Self::Reduced { inner }
            | Self::Slice { inner, .. } => vec![NodeMut::Pattern(inner)],
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

    /// [`Self::children`], to change in place.
    pub fn children_mut(&mut self) -> Vec<NodeMut<'_>> {
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
            | Self::Divide(a, b) => vec![NodeMut::Expression(a), NodeMut::Expression(b)],
            Self::In(a, list) => std::iter::once(NodeMut::Expression(a))
                .chain(list.iter_mut().map(NodeMut::Expression))
                .collect(),
            Self::UnaryPlus(a) | Self::UnaryMinus(a) | Self::Not(a) => {
                vec![NodeMut::Expression(a)]
            }
            Self::If(a, b, c) => vec![
                NodeMut::Expression(a),
                NodeMut::Expression(b),
                NodeMut::Expression(c),
            ],
            Self::Coalesce(list) | Self::FunctionCall(_, list) => {
                list.iter_mut().map(NodeMut::Expression).collect()
            }
            Self::Exists(pattern) => vec![NodeMut::Pattern(pattern)],
        }
    }

    /// Whether `f` holds for this expression or any node in it (depth first, the patterns
    /// of `EXISTS` included).
    pub fn find(&self, f: &mut impl FnMut(Node<'_>) -> bool) -> bool {
        find(Node::Expression(self), f)
    }
}

/// Depth first and left to right, with a stack of its own: any tree can be searched.
fn find(node: Node<'_>, f: &mut impl FnMut(Node<'_>) -> bool) -> bool {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if f(node) {
            return true;
        }
        stack.extend(node.children().into_iter().rev());
    }
    false
}

impl<'a> Node<'a> {
    fn children(self) -> Vec<Node<'a>> {
        match self {
            Node::Pattern(pattern) => pattern.children(),
            Node::Expression(expression) => expression.children(),
        }
    }
}

impl GraphPattern {
    /// How deep the algebra nests: patterns, the expressions attached to them, the
    /// patterns of `EXISTS`, and the steps of property paths, one level each. The passes
    /// over the algebra recurse this deep, so the parser refuses a query deeper than its
    /// nesting limit ([`crate::SparqlParser::with_max_nesting`]). Measured with a stack of
    /// its own: any tree can be measured.
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack = vec![(Node::Pattern(self), 1)];
        while let Some((node, level)) = stack.pop() {
            let path = match node {
                Node::Pattern(Self::Path { path, .. }) => path_depth(path),
                _ => 0,
            };
            deepest = deepest.max(level + path);
            stack.extend(node.children().into_iter().map(|child| (child, level + 1)));
        }
        deepest
    }
}

/// How deep a property path nests.
fn path_depth(path: &PropertyPathExpression) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(path, 1)];
    while let Some((path, level)) = stack.pop() {
        deepest = deepest.max(level);
        match path {
            PropertyPathExpression::NamedNode(_)
            | PropertyPathExpression::NegatedPropertySet(_) => {}
            PropertyPathExpression::Reverse(p)
            | PropertyPathExpression::ZeroOrMore(p)
            | PropertyPathExpression::OneOrMore(p)
            | PropertyPathExpression::ZeroOrOne(p) => stack.push((p, level + 1)),
            PropertyPathExpression::Sequence(paths)
            | PropertyPathExpression::Alternative(paths) => {
                stack.extend(paths.iter().map(|p| (p, level + 1)));
            }
        }
    }
    deepest
}

#[cfg(test)]
mod tests {
    use super::{Node, NodeMut};
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

    /// A node by its kind and address.
    fn place(node: &Node<'_>) -> (bool, *const ()) {
        match node {
            Node::Pattern(p) => (true, *p as *const GraphPattern as *const ()),
            Node::Expression(e) => (false, *e as *const Expression as *const ()),
        }
    }

    #[test]
    fn children_mut_gives_children_s_nodes_in_order() {
        // Every pattern kind with nodes in it, and every expression kind with operands.
        let mut query = pattern(
            "SELECT DISTINCT ?x (SUM(?n) AS ?s) (COUNT(*) AS ?c) WHERE {
               { ?x <urn:p> ?n } UNION { GRAPH ?g { ?x <urn:q> ?n } }
               OPTIONAL { ?x <urn:r> ?m FILTER(?m > 1 && ?m < 9 || ?m = 0) }
               MINUS { ?x <urn:s> ?n }
               LATERAL { SELECT REDUCED ?y WHERE { ?x <urn:t> ?y } }
               SERVICE <urn:e> { ?x <urn:u> ?n }
               BIND(IF(?n IN (1, 2), COALESCE(-?n + +?n, ?n * 2 / 3), !BOUND(?m)) AS ?b)
               VALUES ?n { 1 2 }
               FILTER EXISTS { ?x <urn:v> ?n FILTER(STRLEN(STR(?x)) >= 0 && sameTerm(?x, ?x)) }
             } GROUP BY ?x ORDER BY DESC(?s - 1) LIMIT 5",
        );
        let mut stack = vec![NodeMut::Pattern(&mut query)];
        let mut nodes = 0;
        while let Some(node) = stack.pop() {
            let shared: Vec<_> = match &node {
                NodeMut::Pattern(p) => p.children(),
                NodeMut::Expression(e) => e.children(),
            }
            .iter()
            .map(place)
            .collect();
            let children = match node {
                NodeMut::Pattern(p) => p.children_mut(),
                NodeMut::Expression(e) => e.children_mut(),
            };
            let changing: Vec<_> = children
                .iter()
                .map(|child| match child {
                    NodeMut::Pattern(p) => place(&Node::Pattern(p)),
                    NodeMut::Expression(e) => place(&Node::Expression(e)),
                })
                .collect();
            assert_eq!(
                changing, shared,
                "SPEC children_mut: children's nodes, in order"
            );
            nodes += children.len();
            stack.extend(children);
        }
        assert!(nodes > 40, "the whole tree was walked: {nodes} nodes");
    }
}
