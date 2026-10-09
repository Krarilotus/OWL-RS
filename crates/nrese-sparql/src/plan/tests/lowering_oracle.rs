//! The pre-migration borrowed lowering, retained only as a differential oracle.

use super::super::Plan;
use nrese_sparql_syntax::algebra::GraphPattern;
use nrese_sparql_syntax::term::TriplePattern;

pub(super) fn lower(plan: &Plan) -> GraphPattern {
    reference_lower(plan)
}

fn reference_lower(plan: &Plan) -> GraphPattern {
    let lower = |p: &Plan| Box::new(reference_lower(p));
    match plan {
        Plan::Scan(triple) => GraphPattern::Bgp {
            patterns: vec![triple.clone()],
        },
        Plan::Path {
            subject,
            path,
            object,
        } => GraphPattern::Path {
            subject: subject.clone(),
            path: path.clone(),
            object: object.clone(),
        },
        Plan::Join(inputs) => {
            let scans: Vec<TriplePattern> = inputs
                .iter()
                .filter_map(|input| match input {
                    Plan::Scan(triple) => Some(triple.clone()),
                    _ => None,
                })
                .collect();
            if scans.len() == inputs.len() {
                return GraphPattern::Bgp { patterns: scans };
            }
            let inputs: Vec<Plan> = if inputs.iter().any(Plan::ordered) {
                // An input whose rows an ORDER BY orders keeps its place among the
                // others (the executor keeps that order through joins): only runs of
                // adjacent scans become one basic graph pattern each.
                let mut runs: Vec<Plan> = Vec::new();
                for input in inputs {
                    match (input, runs.last_mut()) {
                        (Plan::Scan(triple), Some(Plan::Join(run))) => {
                            run.push(Plan::Scan(triple.clone()));
                        }
                        (Plan::Scan(triple), _) => {
                            runs.push(Plan::Join(vec![Plan::Scan(triple.clone())]));
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
                    .filter(|input| !matches!(input, Plan::Scan(_)));
                let first = (!scans.is_empty())
                    .then(|| Plan::Join(scans.into_iter().map(Plan::Scan).collect()));
                first.into_iter().chain(others.cloned()).collect()
            };
            reference_nested(&inputs, |left, right| GraphPattern::Join { left, right })
        }
        Plan::LeftJoin {
            left,
            right,
            condition,
        } => GraphPattern::LeftJoin {
            left: lower(left),
            right: lower(right),
            expression: condition.clone(),
        },
        Plan::Lateral { left, right } => GraphPattern::Lateral {
            left: lower(left),
            right: lower(right),
        },
        Plan::Filter { condition, input } => GraphPattern::Filter {
            expr: condition.clone(),
            inner: lower(input),
        },
        Plan::Union(inputs) => {
            reference_nested(inputs, |left, right| GraphPattern::Union { left, right })
        }
        Plan::Minus { left, right } => GraphPattern::Minus {
            left: lower(left),
            right: lower(right),
        },
        Plan::Graph { name, input } => GraphPattern::Graph {
            name: name.clone(),
            inner: lower(input),
        },
        Plan::Extend {
            input,
            variable,
            expression,
        } => GraphPattern::Extend {
            inner: lower(input),
            variable: variable.clone(),
            expression: expression.clone(),
        },
        Plan::Values { variables, rows } => GraphPattern::Values {
            variables: variables.clone(),
            bindings: rows.clone(),
        },
        Plan::OrderBy { input, keys } => GraphPattern::OrderBy {
            inner: lower(input),
            expression: keys.clone(),
        },
        Plan::Project { input, variables } => GraphPattern::Project {
            inner: lower(input),
            variables: variables.clone(),
        },
        Plan::Distinct(input) => GraphPattern::Distinct {
            inner: lower(input),
        },
        Plan::Reduced(input) => GraphPattern::Reduced {
            inner: lower(input),
        },
        Plan::Slice {
            input,
            start,
            length,
        } => GraphPattern::Slice {
            inner: lower(input),
            start: *start,
            length: *length,
        },
        Plan::Group {
            input,
            keys,
            aggregates,
        } => GraphPattern::Group {
            inner: lower(input),
            variables: keys.clone(),
            aggregates: aggregates.clone(),
        },
        Plan::Service {
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

fn reference_nested(
    inputs: &[Plan],
    combine: impl Fn(Box<GraphPattern>, Box<GraphPattern>) -> GraphPattern,
) -> GraphPattern {
    let mut lowered = inputs.iter().map(reference_lower);
    let Some(first) = lowered.next() else {
        return GraphPattern::Bgp {
            patterns: Vec::new(),
        };
    };
    lowered.fold(first, |left, right| {
        combine(Box::new(left), Box::new(right))
    })
}
