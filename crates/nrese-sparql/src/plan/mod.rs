//! The logical plan of a query (docs/design/query-plan.md, step 1): the algebra's
//! operators with joins and unions as lists, so that rewrites (step 2) can reorder and
//! merge them; a basic graph pattern is a join of its triple patterns.
//!
//! [`Plan::of`] builds it from the algebra; [`Plan::into_pattern`] moves its owned
//! data back into the algebra that the executor still runs. Property inspection reads
//! plan nodes directly, without this execution adapter. Until a rewrite changes a plan, lowering is the identity:
//! `Plan::of(p).lower() == p` for every pattern (a test checks it on the differential
//! tests' queries).
//!
//! The rewrites (step 2), each a method of [`Plan`]:
//! - [`Plan::flatten_joins`]: joins inside joins become one join, so the triple patterns
//!   of groups joined to each other are one basic graph pattern, ordered together;
//! - [`Plan::eager_aggregation`]: a group over a join aggregates the side its aggregates
//!   read before the join, so that the join's product of rows is never made.

mod properties;
mod rewrite;
#[cfg(test)]
mod tests;

pub use rewrite::{eager_aggregation, eager_aggregation_where, rewrite};

#[cfg(test)]
thread_local! { pub(crate) static LOWERED_NODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression, PropertyPathExpression,
};
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

/// A node of the logical plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// One triple pattern.
    Scan(TriplePattern),
    /// A property path between two terms.
    Path {
        subject: TermPattern,
        path: PropertyPathExpression,
        object: TermPattern,
    },
    /// Its inputs joined: a basic graph pattern's triple patterns (all scans), or the two
    /// sides of a join of the algebra.
    Join(Vec<Plan>),
    LeftJoin {
        left: Box<Plan>,
        right: Box<Plan>,
        condition: Option<Expression>,
    },
    /// `LATERAL`: the right side evaluated for each solution of the left.
    Lateral {
        left: Box<Plan>,
        right: Box<Plan>,
    },
    Filter {
        condition: Expression,
        input: Box<Plan>,
    },
    /// Its inputs' solutions together.
    Union(Vec<Plan>),
    Minus {
        left: Box<Plan>,
        right: Box<Plan>,
    },
    Graph {
        name: NamedNodePattern,
        input: Box<Plan>,
    },
    Extend {
        input: Box<Plan>,
        variable: Variable,
        expression: Expression,
    },
    Values {
        variables: Vec<Variable>,
        rows: Vec<Vec<Option<GroundTerm>>>,
    },
    OrderBy {
        input: Box<Plan>,
        keys: Vec<OrderExpression>,
    },
    Project {
        input: Box<Plan>,
        variables: Vec<Variable>,
    },
    Distinct(Box<Plan>),
    Reduced(Box<Plan>),
    Slice {
        input: Box<Plan>,
        start: usize,
        length: Option<usize>,
    },
    Group {
        input: Box<Plan>,
        keys: Vec<Variable>,
        aggregates: Vec<(Variable, AggregateExpression)>,
    },
    Service {
        name: NamedNodePattern,
        input: Box<Plan>,
        silent: bool,
    },
}

impl Plan {
    /// The plan of `pattern`.
    pub fn of(pattern: &GraphPattern) -> Self {
        let of = |p: &GraphPattern| Box::new(Self::of(p));
        match pattern {
            GraphPattern::Bgp { patterns } => {
                Self::Join(patterns.iter().cloned().map(Self::Scan).collect())
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => Self::Path {
                subject: subject.clone(),
                path: path.clone(),
                object: object.clone(),
            },
            GraphPattern::Join { left, right } => Self::Join(vec![Self::of(left), Self::of(right)]),
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => Self::LeftJoin {
                left: of(left),
                right: of(right),
                condition: expression.clone(),
            },
            GraphPattern::Lateral { left, right } => Self::Lateral {
                left: of(left),
                right: of(right),
            },
            GraphPattern::Filter { expr, inner } => Self::Filter {
                condition: expr.clone(),
                input: of(inner),
            },
            GraphPattern::Union { left, right } => {
                Self::Union(vec![Self::of(left), Self::of(right)])
            }
            GraphPattern::Minus { left, right } => Self::Minus {
                left: of(left),
                right: of(right),
            },
            GraphPattern::Graph { name, inner } => Self::Graph {
                name: name.clone(),
                input: of(inner),
            },
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => Self::Extend {
                input: of(inner),
                variable: variable.clone(),
                expression: expression.clone(),
            },
            GraphPattern::Values {
                variables,
                bindings,
            } => Self::Values {
                variables: variables.clone(),
                rows: bindings.clone(),
            },
            GraphPattern::OrderBy { inner, expression } => Self::OrderBy {
                input: of(inner),
                keys: expression.clone(),
            },
            GraphPattern::Project { inner, variables } => Self::Project {
                input: of(inner),
                variables: variables.clone(),
            },
            GraphPattern::Distinct { inner } => Self::Distinct(of(inner)),
            GraphPattern::Reduced { inner } => Self::Reduced(of(inner)),
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => Self::Slice {
                input: of(inner),
                start: *start,
                length: *length,
            },
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => Self::Group {
                input: of(inner),
                keys: variables.clone(),
                aggregates: aggregates.clone(),
            },
            GraphPattern::Service {
                name,
                inner,
                silent,
            } => Self::Service {
                name: name.clone(),
                input: of(inner),
                silent: *silent,
            },
        }
    }

    /// The algebra for this plan. Use [`Self::into_pattern`] when the plan is no longer needed.
    pub fn lower(&self) -> GraphPattern {
        self.clone().into_pattern()
    }

    /// The algebra the executor runs: a join of scans only is a basic graph pattern, a
    /// join of other inputs nests binary joins from the left, a union likewise.
    pub fn into_pattern(self) -> GraphPattern {
        #[cfg(test)]
        LOWERED_NODES.with(|count| count.set(count.get() + 1));
        let lower = |p: Box<Self>| Box::new(p.into_pattern());
        match self {
            Self::Scan(triple) => GraphPattern::Bgp {
                patterns: vec![triple],
            },
            Self::Path {
                subject,
                path,
                object,
            } => GraphPattern::Path {
                subject,
                path,
                object,
            },
            Self::Join(inputs) => {
                if inputs.iter().all(|input| matches!(input, Self::Scan(_))) {
                    return GraphPattern::Bgp {
                        patterns: inputs
                            .into_iter()
                            .map(|input| {
                                let Self::Scan(triple) = input else {
                                    unreachable!("scans only")
                                };
                                triple
                            })
                            .collect(),
                    };
                }
                let ordered = inputs.iter().any(Self::ordered);
                let mut runs = Vec::new();
                if ordered {
                    // Preserve ordered inputs' positions, merging only adjacent scans.
                    for input in inputs {
                        match (input, runs.last_mut()) {
                            (Self::Scan(triple), Some(Self::Join(run))) => {
                                run.push(Self::Scan(triple))
                            }
                            (Self::Scan(triple), _) => {
                                runs.push(Self::Join(vec![Self::Scan(triple)]))
                            }
                            (other, _) => runs.push(other),
                        }
                    }
                } else {
                    let (scans, others): (Vec<_>, Vec<_>) = inputs
                        .into_iter()
                        .partition(|input| matches!(input, Self::Scan(_)));
                    if !scans.is_empty() {
                        runs.push(Self::Join(scans));
                    }
                    runs.extend(others);
                }
                nested(runs, |left, right| GraphPattern::Join { left, right })
            }
            Self::LeftJoin {
                left,
                right,
                condition,
            } => GraphPattern::LeftJoin {
                left: lower(left),
                right: lower(right),
                expression: condition,
            },
            Self::Lateral { left, right } => GraphPattern::Lateral {
                left: lower(left),
                right: lower(right),
            },
            Self::Filter { condition, input } => GraphPattern::Filter {
                expr: condition,
                inner: lower(input),
            },
            Self::Union(inputs) => {
                nested(inputs, |left, right| GraphPattern::Union { left, right })
            }
            Self::Minus { left, right } => GraphPattern::Minus {
                left: lower(left),
                right: lower(right),
            },
            Self::Graph { name, input } => GraphPattern::Graph {
                name,
                inner: lower(input),
            },
            Self::Extend {
                input,
                variable,
                expression,
            } => GraphPattern::Extend {
                inner: lower(input),
                variable,
                expression,
            },
            Self::Values { variables, rows } => GraphPattern::Values {
                variables,
                bindings: rows,
            },
            Self::OrderBy { input, keys } => GraphPattern::OrderBy {
                inner: lower(input),
                expression: keys,
            },
            Self::Project { input, variables } => GraphPattern::Project {
                inner: lower(input),
                variables,
            },
            Self::Distinct(input) => GraphPattern::Distinct {
                inner: lower(input),
            },
            Self::Reduced(input) => GraphPattern::Reduced {
                inner: lower(input),
            },
            Self::Slice {
                input,
                start,
                length,
            } => GraphPattern::Slice {
                inner: lower(input),
                start,
                length,
            },
            Self::Group {
                input,
                keys,
                aggregates,
            } => GraphPattern::Group {
                inner: lower(input),
                variables: keys,
                aggregates,
            },
            Self::Service {
                name,
                input,
                silent,
            } => GraphPattern::Service {
                name,
                inner: lower(input),
                silent,
            },
        }
    }
}

/// `inputs` lowered and combined from the left by `combine`; one input is itself, none an
/// empty basic graph pattern (the join's identity; an empty union never arises).
fn nested(
    inputs: Vec<Plan>,
    combine: impl Fn(Box<GraphPattern>, Box<GraphPattern>) -> GraphPattern,
) -> GraphPattern {
    let mut lowered = inputs.into_iter().map(Plan::into_pattern);
    let Some(first) = lowered.next() else {
        return GraphPattern::Bgp {
            patterns: Vec::new(),
        };
    };
    lowered.fold(first, |left, right| {
        combine(Box::new(left), Box::new(right))
    })
}
