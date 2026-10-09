//! Logical rewrites and their eligibility rules; no execution or resource ownership.

use super::*;

/// The rewrites of step 2 applied to `pattern`.
pub fn rewrite(pattern: &GraphPattern) -> GraphPattern {
    Plan::of(pattern).flatten_joins().into_pattern()
}

/// [`Plan::eager_aggregation`] applied to `pattern`.
pub fn eager_aggregation(pattern: &GraphPattern) -> GraphPattern {
    Plan::of(pattern).eager_aggregation().into_pattern()
}

/// [`Plan::eager_aggregation_where`] applied to `pattern`.
pub fn eager_aggregation_where(
    pattern: &GraphPattern,
    pays: &dyn Fn(&Plan, &[Variable]) -> bool,
) -> GraphPattern {
    Plan::of(pattern)
        .eager_aggregation_where(pays)
        .into_pattern()
}

impl Plan {
    /// `self` with `f` applied to each input (the plan's direct children).
    fn map_inputs(self, f: &mut impl FnMut(Self) -> Self) -> Self {
        let mut boxed = |plan: Box<Self>| Box::new(f(*plan));
        match self {
            Self::Join(inputs) => Self::Join(inputs.into_iter().map(&mut *f).collect()),
            Self::Union(inputs) => Self::Union(inputs.into_iter().map(&mut *f).collect()),
            Self::LeftJoin {
                left,
                right,
                condition,
            } => Self::LeftJoin {
                left: boxed(left),
                right: boxed(right),
                condition,
            },
            Self::Lateral { left, right } => Self::Lateral {
                left: boxed(left),
                right: boxed(right),
            },
            Self::Minus { left, right } => Self::Minus {
                left: boxed(left),
                right: boxed(right),
            },
            Self::Filter { condition, input } => Self::Filter {
                condition,
                input: boxed(input),
            },
            Self::Graph { name, input } => Self::Graph {
                name,
                input: boxed(input),
            },
            Self::Extend {
                input,
                variable,
                expression,
            } => Self::Extend {
                input: boxed(input),
                variable,
                expression,
            },
            Self::OrderBy { input, keys } => Self::OrderBy {
                input: boxed(input),
                keys,
            },
            Self::Project { input, variables } => Self::Project {
                input: boxed(input),
                variables,
            },
            Self::Distinct(input) => Self::Distinct(boxed(input)),
            Self::Reduced(input) => Self::Reduced(boxed(input)),
            Self::Slice {
                input,
                start,
                length,
            } => Self::Slice {
                input: boxed(input),
                start,
                length,
            },
            Self::Group {
                input,
                keys,
                aggregates,
            } => Self::Group {
                input: boxed(input),
                keys,
                aggregates,
            },
            Self::Service {
                name,
                input,
                silent,
            } => Self::Service {
                name,
                input: boxed(input),
                silent,
            },
            leaf @ (Self::Scan(_) | Self::Path { .. } | Self::Values { .. }) => leaf,
        }
    }

    /// Eager aggregation (Yan and Larson, VLDB 1994): `GROUP BY K` over a join whose
    /// aggregates read only the inputs without a `K` variable (`B`) aggregates `B` by the
    /// variables it shares with the others (`A`) first, and the group then adds up the
    /// partial results. `COUNT(x)` over the join is the sum over `A`'s rows of the counts
    /// of the `B` rows each meets, which is what the partial counts give; `SUM` likewise.
    /// So `GROUP BY ?feature` over products with their features and their offers counts
    /// offers per product once, instead of making a row for every feature and offer of a
    /// product (BSBM BI q4: 155 M rows).
    ///
    /// - `COUNT`, `COUNT(*)` and `SUM`: the group sums the partial results.
    /// - `MIN` and `MAX` (with or without `DISTINCT`, which changes no extreme): the
    ///   extreme of the partial extremes. `ORDER BY`'s order is total over distinct terms
    ///   (`value::order`), so the extreme is the same term either way.
    /// - `AVG`: a `SUM` and a `COUNT(*)` per partial group, summed by the group and divided
    ///   after it. SPARQL's division is `AVG`'s own: integers and decimals give a decimal,
    ///   a float or double total its type, a duration total a duration of its type. An
    ///   error, an unbound value or mixed kinds (numbers and durations) make the partial
    ///   sum unbound, and with it the sum of the partial sums and the quotient, as one
    ///   error makes `AVG` unbound. A group is never empty (it has a key), so the count is
    ///   at least one.
    ///
    /// Not with `DISTINCT` for the others, not over expressions with `EXISTS` or values
    /// drawn per row (`RAND`, `BNODE`: the partial rows would draw fewer), and only where
    /// `A` and `B` share a variable (with none, an empty `B` would still leave one partial
    /// row). Other aggregates (`SAMPLE`, `GROUP_CONCAT`) leave the group as it is.
    pub fn eager_aggregation(self) -> Self {
        self.eager_aggregation_where(&|_, _| true)
    }

    /// [`Self::eager_aggregation`] where `pays(side, keys)` says that aggregating `side`
    /// by `keys` first reduces it: a decision for the store's statistics. When each key
    /// value has about one row on that side, the early group reduces nothing and the plan
    /// pays for it twice (an extra group, and a join with the other side's rows); the
    /// planner asks for at least two rows per key value.
    pub fn eager_aggregation_where(self, pays: &dyn Fn(&Plan, &[Variable]) -> bool) -> Self {
        let plan = self.map_inputs(&mut |p: Self| p.eager_aggregation_where(pays));
        let Self::Group {
            input,
            keys,
            aggregates,
        } = plan
        else {
            return plan;
        };
        match pre_aggregated(&input, &keys, &aggregates, pays) {
            Some(rewritten) => rewritten,
            None => Self::Group {
                input,
                keys,
                aggregates,
            },
        }
    }

    /// Joins inside joins become one join (joins are associative and commutative, and a
    /// blank node label never appears in two groups): the triple patterns of groups joined
    /// to each other are then one basic graph pattern, which the join order covers whole.
    /// Optionals, filters, unions and the like keep their place: only joins are merged.
    pub fn flatten_joins(self) -> Self {
        match self.map_inputs(&mut Self::flatten_joins) {
            Self::Join(inputs) => {
                let mut flattened = Vec::with_capacity(inputs.len());
                for input in inputs {
                    match input {
                        Self::Join(inner) => flattened.extend(inner),
                        other => flattened.push(other),
                    }
                }
                Self::Join(flattened)
            }
            other => other,
        }
    }
}

/// The group of [`Plan::eager_aggregation`] over `input`, if it applies.
fn pre_aggregated(
    input: &Plan,
    keys: &[Variable],
    aggregates: &[(Variable, AggregateExpression)],
    pays: &dyn Fn(&Plan, &[Variable]) -> bool,
) -> Option<Plan> {
    use nrese_sparql_syntax::algebra::AggregateFunction;
    let Plan::Join(inputs) = input else {
        return None;
    };
    if keys.is_empty() || aggregates.is_empty() || input.ordered() {
        return None;
    }
    // EXISTS reads the whole solution, which the partial rows don't have; RAND and the
    // like are drawn once per row, and the partial rows are fewer.
    let per_row = |expr: &Expression| crate::native::per_solution(expr);
    let decomposable = aggregates.iter().all(|(_, aggregate)| match aggregate {
        AggregateExpression::CountSolutions { distinct } => !distinct,
        AggregateExpression::FunctionCall {
            name: AggregateFunction::Count | AggregateFunction::Sum | AggregateFunction::Avg,
            expr,
            distinct,
        } => !distinct && !per_row(expr),
        AggregateExpression::FunctionCall {
            name: AggregateFunction::Min | AggregateFunction::Max,
            expr,
            ..
        } => !per_row(expr),
        _ => false,
    });
    if !decomposable {
        return None;
    }
    let (with_keys, without): (Vec<&Plan>, Vec<&Plan>) = inputs
        .iter()
        .partition(|plan| plan.variables().iter().any(|v| keys.contains(v)));
    if with_keys.is_empty() || without.is_empty() {
        return None;
    }
    // A blank node joins like a variable but isn't one in scope (a path's steps are joined
    // through one): the two sides must not share any (found by the differential tests:
    // `?b :p/:p ?d` split across them lost the join).
    let mut a_blanks = Vec::new();
    with_keys.iter().for_each(|p| p.blank_nodes(&mut a_blanks));
    let mut b_blanks = Vec::new();
    without.iter().for_each(|p| p.blank_nodes(&mut b_blanks));
    if a_blanks.iter().any(|b| b_blanks.contains(b)) {
        return None;
    }
    let a_vars: Vec<Variable> = with_keys.iter().flat_map(|p| p.variables()).collect();
    let b_vars: Vec<Variable> = without.iter().flat_map(|p| p.variables()).collect();
    // The aggregates read only B's variables.
    let mut reads_a = false;
    for (_, aggregate) in aggregates {
        if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
            expr.on_used_variable(&mut |v| reads_a |= !b_vars.contains(v));
        }
    }
    if reads_a {
        return None;
    }
    let mut shared: Vec<Variable> = Vec::new();
    for v in &b_vars {
        if a_vars.contains(v) && !shared.contains(v) {
            shared.push(v.clone());
        }
    }
    if shared.is_empty() {
        return None;
    }
    let side = Plan::Join(without.iter().map(|p| (*p).clone()).collect());
    if !pays(&side, &shared) {
        return None;
    }
    // Names for the partial results that nothing in the group uses.
    let taken = |name: &str| {
        a_vars
            .iter()
            .chain(&b_vars)
            .chain(keys)
            .any(|v| v.as_str() == name)
            || aggregates.iter().any(|(v, _)| v.as_str() == name)
    };
    let mut n = 0;
    let mut fresh = || loop {
        let name = format!("__eager{n}");
        n += 1;
        if !taken(&name) {
            break Variable::new_unchecked(name);
        }
    };
    let over = |name: AggregateFunction, variable: &Variable| AggregateExpression::FunctionCall {
        name,
        expr: Expression::Variable(variable.clone()),
        distinct: false,
    };
    let mut partials = Vec::with_capacity(aggregates.len());
    let mut finals = Vec::with_capacity(aggregates.len());
    // AVG's quotients, computed after the group: (target, total, count).
    let mut quotients: Vec<(Variable, Variable, Variable)> = Vec::new();
    for (variable, aggregate) in aggregates {
        let partial = fresh();
        match aggregate {
            AggregateExpression::FunctionCall {
                name: name @ (AggregateFunction::Min | AggregateFunction::Max),
                expr,
                ..
            } => {
                partials.push((
                    partial.clone(),
                    AggregateExpression::FunctionCall {
                        name: name.clone(),
                        expr: expr.clone(),
                        distinct: false,
                    },
                ));
                finals.push((variable.clone(), over(name.clone(), &partial)));
            }
            AggregateExpression::FunctionCall {
                name: AggregateFunction::Avg,
                expr,
                ..
            } => {
                let partial_count = fresh();
                partials.push((
                    partial.clone(),
                    AggregateExpression::FunctionCall {
                        name: AggregateFunction::Sum,
                        expr: expr.clone(),
                        distinct: false,
                    },
                ));
                partials.push((
                    partial_count.clone(),
                    AggregateExpression::CountSolutions { distinct: false },
                ));
                let (total, count) = (fresh(), fresh());
                finals.push((total.clone(), over(AggregateFunction::Sum, &partial)));
                finals.push((count.clone(), over(AggregateFunction::Sum, &partial_count)));
                quotients.push((variable.clone(), total, count));
            }
            _ => {
                partials.push((partial.clone(), aggregate.clone()));
                finals.push((variable.clone(), over(AggregateFunction::Sum, &partial)));
            }
        }
    }
    let mut joined: Vec<Plan> = with_keys.into_iter().cloned().collect();
    joined.push(Plan::Group {
        input: Box::new(Plan::Join(without.into_iter().cloned().collect())),
        keys: shared,
        aggregates: partials,
    });
    let group = Plan::Group {
        input: Box::new(Plan::Join(joined)),
        keys: keys.to_vec(),
        aggregates: finals,
    };
    if quotients.is_empty() {
        return Some(group);
    }
    // The averages from the totals and counts, and the group's own variables only.
    let averaged = quotients
        .into_iter()
        .fold(group, |input, (variable, total, count)| Plan::Extend {
            input: Box::new(input),
            variable,
            expression: Expression::Divide(
                Box::new(Expression::Variable(total)),
                Box::new(Expression::Variable(count)),
            ),
        });
    let mut variables = keys.to_vec();
    variables.extend(aggregates.iter().map(|(variable, _)| variable.clone()));
    Some(Plan::Project {
        input: Box::new(averaged),
        variables,
    })
}
