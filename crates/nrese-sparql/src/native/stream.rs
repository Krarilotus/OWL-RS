//! GROUP BY over a basic graph pattern whose rows don't fit the query's memory (W6): the
//! pattern is evaluated in morsels of its smallest triple pattern, each morsel joined
//! through the others ([`Context::bgp_from`]) and grouped into partial aggregates, and the
//! partials merged. Memory then holds the smallest pattern's matches, one morsel's rows
//! and the groups, not the join.
//!
//! The aggregates must merge: `COUNT` (partial counts summed), `SUM`, `MIN`, `MAX`,
//! `SAMPLE`, and `AVG` as a sum and a count divided at the end; not `DISTINCT` ones nor
//! `GROUP_CONCAT`. An error in a group's values leaves its partial unbound, and an unbound
//! partial makes the merged `SUM`, `AVG`, `MIN` and `MAX` unbound as the sequential
//! evaluation does. Floating-point sums may round differently: SPARQL fixes no order.
//!
//! It runs when the direct evaluation of the group runs out of memory (the attempt's
//! memory released first), or always with [`crate::QueryOptions::stream_rows`] (tests).

use super::*;

/// Rows of the morsels' results aimed at, at most; a morsel's size follows its fan-out,
/// and the memory left bounds it further.
const MORSEL_TARGET_ROWS: usize = 1 << 20;
/// Rows of the first pattern in the first morsel, before the fan-out is known.
const FIRST_MORSEL_ROWS: usize = 16;
/// Partial groups collected before they are merged.
const PARTIALS_BEFORE_MERGE: usize = 1 << 20;

/// A GROUP BY that streams: its pattern and filters, and its aggregates split.
pub(super) struct Plan<'a> {
    patterns: &'a [TriplePattern],
    filter: Option<&'a Expression>,
    /// What each morsel computes.
    partials: Vec<(Variable, AggregateExpression)>,
    /// How partials of the same group merge: over the partial columns, into them again.
    merges: Vec<(Variable, AggregateExpression)>,
    /// The aggregates' targets, each from its partial column, or (AVG) from a sum and a
    /// count.
    finals: Vec<Final>,
}

enum Final {
    Copy(Variable, Variable),
    Average {
        target: Variable,
        sum: Variable,
        count: Variable,
    },
}

/// A column no query variable can be named like.
fn partial(i: usize) -> Variable {
    Variable::new_unchecked(format!("partial {i}"))
}

fn of(name: AggregateFunction, expr: Expression) -> AggregateExpression {
    AggregateExpression::FunctionCall {
        name,
        expr,
        distinct: false,
    }
}

impl<'a> Plan<'a> {
    /// The plan for a GROUP BY of `aggregates` over `inner`, if it can stream.
    pub(super) fn of(
        inner: &'a GraphPattern,
        aggregates: &[(Variable, AggregateExpression)],
    ) -> Option<Self> {
        let (patterns, filter) = match inner {
            GraphPattern::Bgp { patterns } => (patterns, None),
            GraphPattern::Filter { expr, inner } => match &**inner {
                GraphPattern::Bgp { patterns } => (patterns, Some(expr)),
                _ => return None,
            },
            _ => return None,
        };
        // One pattern is its own scan: nothing to gain. Text and spatial searches start
        // the joins of their own.
        if patterns.len() < 2
            || patterns
                .iter()
                .any(|t| search::is_search(t, patterns) || spatial::is_spatial(t))
        {
            return None;
        }
        let (mut partials, mut merges, mut finals) = (Vec::new(), Vec::new(), Vec::new());
        let mut next = 0;
        let mut column = || {
            next += 1;
            partial(next - 1)
        };
        for (target, aggregate) in aggregates {
            match aggregate {
                AggregateExpression::CountSolutions { distinct: false } => {
                    let p = column();
                    partials.push((p.clone(), aggregate.clone()));
                    merges.push((
                        p.clone(),
                        of(AggregateFunction::Sum, Expression::Variable(p.clone())),
                    ));
                    finals.push(Final::Copy(target.clone(), p));
                }
                AggregateExpression::FunctionCall {
                    name,
                    expr,
                    distinct: false,
                } => match name {
                    AggregateFunction::Count | AggregateFunction::Sum => {
                        let p = column();
                        partials.push((p.clone(), aggregate.clone()));
                        merges.push((
                            p.clone(),
                            of(AggregateFunction::Sum, Expression::Variable(p.clone())),
                        ));
                        finals.push(Final::Copy(target.clone(), p));
                    }
                    AggregateFunction::Min | AggregateFunction::Max | AggregateFunction::Sample => {
                        let p = column();
                        partials.push((p.clone(), aggregate.clone()));
                        merges.push((p.clone(), of(name.clone(), Expression::Variable(p.clone()))));
                        finals.push(Final::Copy(target.clone(), p));
                    }
                    AggregateFunction::Avg => {
                        let (sum, count) = (column(), column());
                        partials.push((sum.clone(), of(AggregateFunction::Sum, expr.clone())));
                        partials.push((count.clone(), of(AggregateFunction::Count, expr.clone())));
                        for p in [&sum, &count] {
                            merges.push((
                                p.clone(),
                                of(AggregateFunction::Sum, Expression::Variable(p.clone())),
                            ));
                        }
                        finals.push(Final::Average {
                            target: target.clone(),
                            sum,
                            count,
                        });
                    }
                    _ => return None,
                },
                _ => return None,
            }
        }
        Some(Self {
            patterns,
            filter,
            partials,
            merges,
            finals,
        })
    }
}

/// Whether `error` is a query running out of its memory budget.
pub(super) fn is_memory_limit(error: &NativeError) -> bool {
    matches!(
        error,
        NativeError::Evaluation(QueryEvaluationError::Dataset(inner))
            if inner.downcast_ref::<BudgetExceeded>().is_some()
    )
}

impl Context<'_> {
    /// The GROUP BY of `plan` with keys `variables` and `aggregates` (as [`Plan::of`] took
    /// them), in morsels.
    pub(super) fn group_streamed(
        &self,
        plan: &Plan<'_>,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
    ) -> NativeResult<Solutions> {
        let mut scans = Vec::with_capacity(plan.patterns.len());
        for triple in plan.patterns {
            match self.scan_pattern(triple) {
                Some(scan) => scans.push(scan),
                // A constant the store doesn't know: no rows.
                None => {
                    return self.group_solutions(
                        self.empty_of(plan.patterns),
                        variables,
                        aggregates,
                    );
                }
            }
        }
        // The smallest pattern starts every morsel.
        let first = (0..scans.len())
            .min_by_key(|&i| self.snapshot.estimate_in(self.model, &scans[i].quad_pattern()))
            .expect("two patterns or more");
        let rest: Vec<TriplePattern> = plan
            .patterns
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != first)
            .map(|(_, t)| t.clone())
            .collect();
        let seeds = self.scan(&scans[first], None)?;
        // Conjuncts placed in the joins as their variables are bound; the others (EXISTS,
        // per-row draws) after each morsel.
        let mut conjuncts = Vec::new();
        if let Some(filter) = plan.filter {
            pushdown::conjuncts_of(filter, &mut conjuncts);
        }
        let (early, late): (Vec<&Expression>, Vec<&Expression>) =
            conjuncts.into_iter().partition(|c| pushdown::movable(c));
        let early: Vec<(&Expression, Vec<Variable>)> = early
            .into_iter()
            .map(|c| (c, expression_variables(c)))
            .collect();
        let mut partials: Vec<Solutions> = Vec::new();
        let mut collected = 0;
        let (mut from, mut size) = (0, self.stream_rows.unwrap_or(FIRST_MORSEL_ROWS).max(1));
        // A morsel's rows at a quarter of the memory left (they are copied, filtered and
        // grouped), each row a word per variable.
        let width = self.empty_of(plan.patterns).vars.len().max(1);
        let target = (self.budget.remaining() / (width * 8 * 4)).clamp(1024, MORSEL_TARGET_ROWS);
        let total = seeds.table.len();
        while from < total {
            self.check()?;
            let to = (from + size).min(total);
            let columns: Vec<Vec<u64>> = seeds
                .table
                .columns()
                .iter()
                .map(|column| column[from..to].to_vec())
                .collect();
            let seed = self.produced(Solutions {
                vars: seeds.vars.clone(),
                table: IdTable::from_columns(columns),
                ordered: false,
            })?;
            let mut placed = early.clone();
            let mut rows = self.bgp_from(seed, &rest, &mut placed)?;
            for (conjunct, _) in placed {
                rows = self.filter(rows, conjunct)?;
            }
            for conjunct in &late {
                rows = self.filter(rows, conjunct)?;
            }
            let fan_out = rows.table.len() as f64 / (to - from) as f64;
            // A morsel without rows adds no group (without keys, grouping nothing would
            // make one, whose unbound MIN would spoil the merge).
            if rows.table.is_empty() {
                self.consumed(&rows);
            } else {
                let grouped = self.group_solutions(rows, variables, &plan.partials)?;
                collected += grouped.table.len();
                partials.push(grouped);
            }
            if collected >= PARTIALS_BEFORE_MERGE {
                let merged = self.merge_partials(std::mem::take(&mut partials), variables, plan)?;
                collected = merged.table.len();
                partials.push(merged);
            }
            from = to;
            // The next morsel sized for the target rows at this fan-out.
            if self.stream_rows.is_none() {
                size = match fan_out {
                    f if f > 0.0 => ((target as f64 / f) as usize).clamp(1, 1 << 22),
                    _ => (size * 2).min(1 << 22),
                };
            }
        }
        self.consumed(&seeds);
        if partials.is_empty() {
            // No rows at all: the GROUP BY of nothing (one empty group without keys).
            return self.group_solutions(self.empty_of(plan.patterns), variables, aggregates);
        }
        let merged = self.merge_partials(partials, variables, plan)?;
        // The aggregates' columns, by their targets.
        let mut solutions = merged;
        for final_ in &plan.finals {
            solutions = match final_ {
                Final::Copy(target, column) => {
                    self.extend(solutions, target, &Expression::Variable(column.clone()))?
                }
                Final::Average { target, sum, count } => self.extend(
                    solutions,
                    target,
                    &Expression::Divide(
                        Box::new(Expression::Variable(sum.clone())),
                        Box::new(Expression::Variable(count.clone())),
                    ),
                )?,
            };
        }
        let mut wanted = variables.to_vec();
        wanted.extend(aggregates.iter().map(|(target, _)| target.clone()));
        let projected = self.project(solutions, &wanted);
        self.produced(projected)
    }

    /// The partial groups of several morsels merged into one partial per group.
    fn merge_partials(
        &self,
        partials: Vec<Solutions>,
        variables: &[Variable],
        plan: &Plan<'_>,
    ) -> NativeResult<Solutions> {
        let vars = partials[0].vars.clone();
        let tables = partials
            .into_iter()
            .map(|part| {
                self.consumed(&part);
                part.table
            })
            .collect();
        let all = self.produced(Solutions {
            table: IdTable::concat(vars.len(), tables),
            vars,
            ordered: false,
        })?;
        self.group_solutions(all, variables, &plan.merges)
    }

    /// No rows, with the columns of `patterns`' variables.
    fn empty_of(&self, patterns: &[TriplePattern]) -> Solutions {
        let mut vars = Vec::new();
        for triple in patterns {
            for v in triple_variables(triple) {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
        }
        Solutions {
            table: IdTable::new(vars.len()),
            vars,
            ordered: false,
        }
    }
}
