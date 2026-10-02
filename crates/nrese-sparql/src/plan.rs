//! The logical plan of a query (docs/plan/2026-10-02-plan-ir.md, step 1): the algebra's
//! operators with joins and unions as lists, so that rewrites (step 2) can reorder and
//! merge them; a basic graph pattern is a join of its triple patterns.
//!
//! [`Plan::of`] builds it from the algebra and [`Plan::lower`] gives the algebra back,
//! which the executor runs. Until a rewrite changes a plan, lowering is the identity:
//! `Plan::of(p).lower() == p` for every pattern (a test checks it on the differential
//! tests' queries).
//!
//! The rewrites (step 2), each a method of [`Plan`]; [`rewrite`] applies them all:
//! - [`Plan::flatten_joins`]: joins inside joins become one join, so the triple patterns
//!   of groups joined to each other are one basic graph pattern, ordered together.

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

    /// The algebra the executor runs: a join of scans only is a basic graph pattern, a
    /// join of other inputs nests binary joins from the left, a union likewise.
    pub fn lower(&self) -> GraphPattern {
        let lower = |p: &Self| Box::new(p.lower());
        match self {
            Self::Scan(triple) => GraphPattern::Bgp {
                patterns: vec![triple.clone()],
            },
            Self::Path {
                subject,
                path,
                object,
            } => GraphPattern::Path {
                subject: subject.clone(),
                path: path.clone(),
                object: object.clone(),
            },
            Self::Join(inputs) => {
                let scans: Vec<TriplePattern> = inputs
                    .iter()
                    .filter_map(|input| match input {
                        Self::Scan(triple) => Some(triple.clone()),
                        _ => None,
                    })
                    .collect();
                if scans.len() == inputs.len() {
                    return GraphPattern::Bgp { patterns: scans };
                }
                let inputs: Vec<Self> = if inputs.iter().any(Self::ordered) {
                    // An input whose rows an ORDER BY orders keeps its place among the
                    // others (the executor keeps that order through joins): only runs of
                    // adjacent scans become one basic graph pattern each.
                    let mut runs: Vec<Self> = Vec::new();
                    for input in inputs {
                        match (input, runs.last_mut()) {
                            (Self::Scan(triple), Some(Self::Join(run))) => {
                                run.push(Self::Scan(triple.clone()));
                            }
                            (Self::Scan(triple), _) => {
                                runs.push(Self::Join(vec![Self::Scan(triple.clone())]));
                            }
                            (other, _) => runs.push(other.clone()),
                        }
                    }
                    runs
                } else {
                    // The scans as one basic graph pattern first, then the other inputs in
                    // their order (a path after it is followed from the values it binds).
                    let others = inputs
                        .iter()
                        .filter(|input| !matches!(input, Self::Scan(_)));
                    let first = (!scans.is_empty())
                        .then(|| Self::Join(scans.into_iter().map(Self::Scan).collect()));
                    first.into_iter().chain(others.cloned()).collect()
                };
                nested(&inputs, |left, right| GraphPattern::Join { left, right })
            }
            Self::LeftJoin {
                left,
                right,
                condition,
            } => GraphPattern::LeftJoin {
                left: lower(left),
                right: lower(right),
                expression: condition.clone(),
            },
            Self::Lateral { left, right } => GraphPattern::Lateral {
                left: lower(left),
                right: lower(right),
            },
            Self::Filter { condition, input } => GraphPattern::Filter {
                expr: condition.clone(),
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
                name: name.clone(),
                inner: lower(input),
            },
            Self::Extend {
                input,
                variable,
                expression,
            } => GraphPattern::Extend {
                inner: lower(input),
                variable: variable.clone(),
                expression: expression.clone(),
            },
            Self::Values { variables, rows } => GraphPattern::Values {
                variables: variables.clone(),
                bindings: rows.clone(),
            },
            Self::OrderBy { input, keys } => GraphPattern::OrderBy {
                inner: lower(input),
                expression: keys.clone(),
            },
            Self::Project { input, variables } => GraphPattern::Project {
                inner: lower(input),
                variables: variables.clone(),
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
                start: *start,
                length: *length,
            },
            Self::Group {
                input,
                keys,
                aggregates,
            } => GraphPattern::Group {
                inner: lower(input),
                variables: keys.clone(),
                aggregates: aggregates.clone(),
            },
            Self::Service {
                name,
                input,
                silent,
            } => GraphPattern::Service {
                name: name.clone(),
                inner: lower(input),
                silent: *silent,
            },
        }
    }
}

/// The rewrites of step 2 applied to `pattern`.
pub fn rewrite(pattern: &GraphPattern) -> GraphPattern {
    Plan::of(pattern).flatten_joins().lower()
}

impl Plan {
    /// Whether an ORDER BY inside fixes the order of its rows (as the executor's sideways
    /// join sees it: it keeps that order through joins).
    fn ordered(&self) -> bool {
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

    /// Joins inside joins become one join (joins are associative and commutative, and a
    /// blank node label never appears in two groups): the triple patterns of groups joined
    /// to each other are then one basic graph pattern, which the join order covers whole.
    /// Optionals, filters, unions and the like keep their place: only joins are merged.
    pub fn flatten_joins(self) -> Self {
        let flat = |plan: Box<Self>| Box::new(plan.flatten_joins());
        match self {
            Self::Join(inputs) => {
                let mut flattened = Vec::with_capacity(inputs.len());
                for input in inputs {
                    match input.flatten_joins() {
                        Self::Join(inner) => flattened.extend(inner),
                        other => flattened.push(other),
                    }
                }
                Self::Join(flattened)
            }
            Self::LeftJoin {
                left,
                right,
                condition,
            } => Self::LeftJoin {
                left: flat(left),
                right: flat(right),
                condition,
            },
            Self::Lateral { left, right } => Self::Lateral {
                left: flat(left),
                right: flat(right),
            },
            Self::Filter { condition, input } => Self::Filter {
                condition,
                input: flat(input),
            },
            Self::Union(inputs) => {
                Self::Union(inputs.into_iter().map(Self::flatten_joins).collect())
            }
            Self::Minus { left, right } => Self::Minus {
                left: flat(left),
                right: flat(right),
            },
            Self::Graph { name, input } => Self::Graph {
                name,
                input: flat(input),
            },
            Self::Extend {
                input,
                variable,
                expression,
            } => Self::Extend {
                input: flat(input),
                variable,
                expression,
            },
            Self::OrderBy { input, keys } => Self::OrderBy {
                input: flat(input),
                keys,
            },
            Self::Project { input, variables } => Self::Project {
                input: flat(input),
                variables,
            },
            Self::Distinct(input) => Self::Distinct(flat(input)),
            Self::Reduced(input) => Self::Reduced(flat(input)),
            Self::Slice {
                input,
                start,
                length,
            } => Self::Slice {
                input: flat(input),
                start,
                length,
            },
            Self::Group {
                input,
                keys,
                aggregates,
            } => Self::Group {
                input: flat(input),
                keys,
                aggregates,
            },
            Self::Service {
                name,
                input,
                silent,
            } => Self::Service {
                name,
                input: flat(input),
                silent,
            },
            leaf @ (Self::Scan(_) | Self::Path { .. } | Self::Values { .. }) => leaf,
        }
    }
}

/// `inputs` lowered and combined from the left by `combine`; one input is itself, none an
/// empty basic graph pattern (the join's identity; an empty union never arises).
fn nested(
    inputs: &[Plan],
    combine: impl Fn(Box<GraphPattern>, Box<GraphPattern>) -> GraphPattern,
) -> GraphPattern {
    let mut lowered = inputs.iter().map(Plan::lower);
    let Some(first) = lowered.next() else {
        return GraphPattern::Bgp {
            patterns: Vec::new(),
        };
    };
    lowered.fold(first, |left, right| {
        combine(Box::new(left), Box::new(right))
    })
}

#[cfg(test)]
mod tests {
    use nrese_sparql_syntax::Query;
    use nrese_sparql_syntax::SparqlParser;

    use super::Plan;

    #[test]
    fn lowering_a_plan_gives_its_algebra_back() {
        for text in [
            "SELECT * WHERE { ?a <urn:p> ?b . ?b <urn:q> ?c }",
            "SELECT * WHERE { { ?a <urn:p> ?b } { ?b <urn:q> ?c } }",
            "SELECT * WHERE { ?a <urn:p> ?b OPTIONAL { ?b <urn:q> ?c FILTER(?c > 1) } ?c <urn:r>+ ?d }",
            "SELECT * WHERE { { ?a <urn:p> ?b } UNION { ?a <urn:q> ?b } UNION { ?a <urn:r> ?b } MINUS { ?a <urn:s> 1 } }",
            "SELECT ?a (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?a ?p ?o } VALUES ?a { <urn:x> } BIND(1 AS ?k) } GROUP BY ?a ORDER BY DESC(?n) LIMIT 5 OFFSET 2",
            "SELECT DISTINCT * WHERE { SERVICE SILENT <http://e.example/sparql> { ?a ?b ?c } }",
            "SELECT * WHERE { }",
            "SELECT REDUCED * WHERE { ?a <urn:p> ?b LATERAL { SELECT ?c WHERE { ?b <urn:q> ?c } LIMIT 1 } }",
        ] {
            let Query::Select { pattern, .. } = SparqlParser::new().parse_query(text).unwrap()
            else {
                unreachable!("selects")
            };
            assert_eq!(Plan::of(&pattern).lower(), pattern, "{text}");
        }
    }

    #[test]
    fn groups_joined_to_each_other_become_one_basic_graph_pattern() {
        let pattern = |text: &str| match SparqlParser::new().parse_query(text).unwrap() {
            Query::Select { pattern, .. } => pattern,
            _ => unreachable!("selects"),
        };
        let rewritten = super::rewrite(&pattern(
            "SELECT * WHERE { { ?a <urn:p> ?b } { ?b <urn:q> ?c } ?c <urn:r>+ ?d }",
        ));
        assert_eq!(
            rewritten,
            pattern("SELECT * WHERE { ?a <urn:p> ?b . ?b <urn:q> ?c . ?c <urn:r>+ ?d }"),
            "one pattern, then the path"
        );
        // An OPTIONAL keeps its left side; what is joined to it moves into one pattern
        // before it (a join commutes).
        let rewritten = super::rewrite(&pattern(
            "SELECT * WHERE { ?a <urn:p> ?b OPTIONAL { ?b <urn:q> ?c } ?c <urn:r> ?d }",
        ));
        assert_eq!(
            rewritten,
            pattern(
                "SELECT * WHERE { ?c <urn:r> ?d { ?a <urn:p> ?b OPTIONAL { ?b <urn:q> ?c } } }"
            )
        );
        // A subquery with an ORDER BY keeps its place (the executor keeps its order through
        // the join); only adjacent triple patterns merge.
        let ordered = "SELECT * WHERE { { SELECT ?x WHERE { ?x <urn:k> ?y } ORDER BY ?y LIMIT 2 } ?x <urn:a> ?a }";
        assert_eq!(super::rewrite(&pattern(ordered)), pattern(ordered));
    }
}
