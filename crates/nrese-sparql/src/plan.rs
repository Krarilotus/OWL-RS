//! The logical plan of a query (docs/plan/2026-10-02-plan-ir.md, step 1): the algebra's
//! operators with joins and unions as lists, so that rewrites (step 2) can reorder and
//! merge them; a basic graph pattern is a join of its triple patterns.
//!
//! [`Plan::of`] builds it from the algebra and [`Plan::lower`] gives the algebra back,
//! which the executor runs. Until a rewrite changes a plan, lowering is the identity:
//! `Plan::of(p).lower() == p` for every pattern (a test checks it on the differential
//! tests' queries).

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
                if inputs.iter().all(|input| matches!(input, Self::Scan(_))) {
                    return GraphPattern::Bgp {
                        patterns: inputs
                            .iter()
                            .map(|input| match input {
                                Self::Scan(triple) => triple.clone(),
                                _ => unreachable!("all scans"),
                            })
                            .collect(),
                    };
                }
                nested(inputs, |left, right| GraphPattern::Join { left, right })
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
}
