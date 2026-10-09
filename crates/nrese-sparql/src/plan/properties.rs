//! Properties read from plan nodes, without constructing execution algebra.

use nrese_rdf::BlankNode;
use nrese_sparql_syntax::visit::Node;

use super::*;

impl Plan {
    /// Whether an ORDER BY fixes row order through the executor's sideways joins.
    pub(super) fn ordered(&self) -> bool {
        match self {
            Self::OrderBy { .. } => true,
            Self::Join(inputs) | Self::Union(inputs) => inputs.iter().any(Self::ordered),
            Self::LeftJoin { left, right, .. } | Self::Minus { left, right } => {
                left.ordered() || right.ordered()
            }
            Self::Filter { input, .. }
            | Self::Extend { input, .. }
            | Self::Graph { input, .. }
            | Self::Project { input, .. }
            | Self::Distinct(input)
            | Self::Reduced(input)
            | Self::Slice { input, .. }
            | Self::Group { input, .. } => input.ordered(),
            Self::Scan(_)
            | Self::Path { .. }
            | Self::Values { .. }
            | Self::Lateral { .. }
            | Self::Service { .. } => false,
        }
    }

    /// Variables in scope, in their execution-algebra order, without lowering the tree.
    /// Deduplication costs O(occurrences * variables); joins also inspect ordering.
    pub fn variables(&self) -> Vec<Variable> {
        let mut variables = Vec::new();
        self.in_scope(&mut |v| {
            if !variables.contains(v) {
                variables.push(v.clone());
            }
        });
        variables
    }

    fn in_scope(&self, add: &mut impl FnMut(&Variable)) {
        match self {
            Self::Scan(triple) => triple_variables(triple, add),
            Self::Path {
                subject, object, ..
            } => {
                term_variables(subject, add);
                term_variables(object, add);
            }
            Self::Join(inputs) => Self::join_scope(&inputs.iter().collect::<Vec<_>>(), add),
            Self::Union(inputs) => {
                for input in inputs {
                    input.in_scope(add);
                }
            }
            Self::LeftJoin { left, right, .. } | Self::Lateral { left, right } => {
                left.in_scope(add);
                right.in_scope(add);
            }
            Self::Minus { left, .. } => left.in_scope(add),
            Self::Graph { name, input } => {
                if let NamedNodePattern::Variable(v) = name {
                    add(v);
                }
                input.in_scope(add);
            }
            Self::Extend {
                variable, input, ..
            } => {
                add(variable);
                input.in_scope(add);
            }
            Self::Group {
                keys, aggregates, ..
            } => {
                for v in keys {
                    add(v);
                }
                for (v, _) in aggregates {
                    add(v);
                }
            }
            Self::Values { variables, .. } | Self::Project { variables, .. } => {
                for v in variables {
                    add(v);
                }
            }
            Self::Filter { input, .. }
            | Self::OrderBy { input, .. }
            | Self::Service { input, .. }
            | Self::Distinct(input)
            | Self::Reduced(input)
            | Self::Slice { input, .. } => input.in_scope(add),
        }
    }

    fn join_scope(inputs: &[&Self], add: &mut impl FnMut(&Variable)) {
        if !inputs.iter().any(|input| input.ordered()) {
            // Lowering gathers scans before other inputs, preserving each partition.
            for scans in [true, false] {
                for input in inputs {
                    if matches!(input, Self::Scan(_)) == scans {
                        input.in_scope(add);
                    }
                }
            }
            return;
        }
        let mut i = 0;
        while i < inputs.len() {
            let input = inputs[i];
            i += 1;
            if let Self::Join(inner) = input {
                // Ordered lowering appends adjacent scans to the preceding join run.
                // Borrow that combined run to retain exactly the same variable order.
                let start = i;
                while i < inputs.len() && matches!(inputs[i], Self::Scan(_)) {
                    i += 1;
                }
                if i > start {
                    let mut run: Vec<_> = inner.iter().collect();
                    run.extend_from_slice(&inputs[start..i]);
                    Self::join_scope(&run, add);
                    continue;
                }
            }
            input.in_scope(add);
        }
    }

    /// Blank nodes join like variables but are not in scope. Inspect expressions too:
    /// EXISTS can contain patterns even where a projection hides their variables.
    pub(super) fn blank_nodes(&self, out: &mut Vec<BlankNode>) {
        match self {
            Self::Scan(triple) => {
                add_blank(&triple.subject, out);
                add_blank(&triple.object, out);
            }
            Self::Path {
                subject, object, ..
            } => {
                add_blank(subject, out);
                add_blank(object, out);
            }
            Self::Join(inputs) | Self::Union(inputs) => {
                for input in inputs {
                    input.blank_nodes(out);
                }
            }
            Self::LeftJoin {
                left,
                right,
                condition,
            } => {
                left.blank_nodes(out);
                right.blank_nodes(out);
                if let Some(expr) = condition {
                    expression_blanks(expr, out);
                }
            }
            Self::Lateral { left, right } | Self::Minus { left, right } => {
                left.blank_nodes(out);
                right.blank_nodes(out);
            }
            Self::Filter {
                input,
                condition: expression,
            }
            | Self::Extend {
                input, expression, ..
            } => {
                input.blank_nodes(out);
                expression_blanks(expression, out);
            }
            Self::OrderBy { input, keys } => {
                input.blank_nodes(out);
                for key in keys {
                    expression_blanks(key.expression(), out);
                }
            }
            Self::Group {
                input, aggregates, ..
            } => {
                input.blank_nodes(out);
                for (_, aggregate) in aggregates {
                    if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
                        expression_blanks(expr, out);
                    }
                }
            }
            Self::Graph { input, .. }
            | Self::Service { input, .. }
            | Self::Project { input, .. }
            | Self::Distinct(input)
            | Self::Reduced(input)
            | Self::Slice { input, .. } => input.blank_nodes(out),
            Self::Values { .. } => {}
        }
    }
}

fn triple_variables(triple: &TriplePattern, add: &mut impl FnMut(&Variable)) {
    term_variables(&triple.subject, add);
    if let NamedNodePattern::Variable(v) = &triple.predicate {
        add(v);
    }
    term_variables(&triple.object, add);
}

fn term_variables(term: &TermPattern, add: &mut impl FnMut(&Variable)) {
    match term {
        TermPattern::Variable(v) => add(v),
        TermPattern::Triple(triple) => triple_variables(triple, add),
        TermPattern::NamedNode(_) | TermPattern::BlankNode(_) | TermPattern::Literal(_) => {}
    }
}

fn add_blank(term: &TermPattern, out: &mut Vec<BlankNode>) {
    if let TermPattern::BlankNode(b) = term
        && !out.contains(b)
    {
        out.push(b.clone());
    }
}

fn expression_blanks(expr: &Expression, out: &mut Vec<BlankNode>) {
    expr.find(&mut |node| {
        match node {
            Node::Pattern(GraphPattern::Bgp { patterns }) => {
                for triple in patterns {
                    add_blank(&triple.subject, out);
                    add_blank(&triple.object, out);
                }
            }
            Node::Pattern(GraphPattern::Path {
                subject, object, ..
            }) => {
                add_blank(subject, out);
                add_blank(object, out);
            }
            _ => {}
        }
        false
    });
}
