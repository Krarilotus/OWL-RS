//! Property paths planned with the triple patterns they are joined to (G4).
//!
//! A join of basic graph patterns and paths ([`PathJoin`]) is ordered as one: each path is
//! an input of the join orderer ([`super::plan`]) beside the triple patterns, with its
//! estimated rows alone ([`Context::path_size`]: few from a constant end) and the distinct
//! values of its ends (its starts and its ends), so a selective path goes first and the
//! patterns are probed from what it reaches, and a path after patterns is followed from
//! the values they bind ([`Context::path_from`]: from the end with fewer values where both
//! are bound). Before, the patterns were always joined first and the paths after them.
//!
//! The evaluation follows the order: each run of consecutive triple patterns is a basic
//! graph pattern (planned again on its own when it starts, else joined from the rows so
//! far), each path is evaluated alone (first) or from the rows so far. The filters of the
//! basic graph patterns run once their variables are bound.

use std::time::Instant;

use nrese_exec::IdTable;
use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};
use nrese_sparql_syntax::term::{TermPattern, TriplePattern};

use super::{
    Context, NativeResult, PathPattern, Solutions, as_path, contains_any_exists,
    expression_variables, plan, pushdown, ranges, search, triple_variables, variable_index,
};
use crate::plan::Plan as LogicalPlan;
use crate::query::PlannedStep;

#[cfg(test)]
mod tests;

/// A join of basic graph patterns (with filters on their own variables) and property
/// paths, at least one path and one other part.
pub(super) struct PathJoin<'q> {
    pub(super) triples: Vec<TriplePattern>,
    pub(super) paths: Vec<PathPattern<'q>>,
    /// The basic graph patterns' filters, as conjuncts.
    pub(super) conjuncts: Vec<&'q Expression>,
}

/// One input of a [`PathJoin`]'s order.
pub(super) enum Part<'j, 'q> {
    Triple(&'j TriplePattern),
    Path(&'j PathPattern<'q>),
}

impl<'q> PathJoin<'q> {
    /// `pattern` as a join of basic graph patterns and paths, if it is one.
    pub(super) fn of(pattern: &'q GraphPattern) -> Option<Self> {
        let mut join = Self {
            triples: Vec::new(),
            paths: Vec::new(),
            conjuncts: Vec::new(),
        };
        if !join.add(pattern) || join.paths.is_empty() {
            return None;
        }
        join.finish()
    }

    /// Reads the logical join directly for EXPLAIN and eager-aggregation costing.
    /// The algebra entry remains for execution until those callers consume plan nodes.
    pub(super) fn of_plan(plan: &'q LogicalPlan) -> Option<Self> {
        let mut join = Self {
            triples: Vec::new(),
            paths: Vec::new(),
            conjuncts: Vec::new(),
        };
        if !join.add_plan(plan) || join.paths.is_empty() {
            return None;
        }
        join.finish()
    }

    fn finish(self) -> Option<Self> {
        let join = self;
        if join.triples.len() + join.paths.len() < 2 {
            return None;
        }
        // Full-text searches and GeoSPARQL relations order themselves.
        if search::split(&join.triples).is_some() || super::spatial::split(&join.triples).is_some()
        {
            return None;
        }
        Some(join)
    }

    fn add(&mut self, pattern: &'q GraphPattern) -> bool {
        if let Some(path) = as_path(pattern) {
            self.paths.push(path);
            return true;
        }
        match pattern {
            GraphPattern::Bgp { patterns } => {
                self.triples.extend(patterns.iter().cloned());
                true
            }
            GraphPattern::Filter { expr, inner } => {
                let GraphPattern::Bgp { patterns } = &**inner else {
                    return false;
                };
                self.add_filtered(patterns.iter(), expr)
            }
            GraphPattern::Join { left, right } => self.add(left) && self.add(right),
            _ => false,
        }
    }

    fn add_plan(&mut self, plan: &'q LogicalPlan) -> bool {
        match unwrapped(plan) {
            LogicalPlan::Scan(triple) => {
                self.triples.push(triple.clone());
                true
            }
            LogicalPlan::Path {
                subject,
                path,
                object,
            } => {
                self.paths.push(PathPattern {
                    subject,
                    path,
                    object,
                    filter: None,
                });
                true
            }
            LogicalPlan::Join(inputs) => {
                // Lowering puts scans before other inputs; keep identical tie-breaking.
                // Ordered inputs are ineligible for a PathJoin, so cannot reach finish.
                [true, false].into_iter().all(|scans| {
                    inputs
                        .iter()
                        .filter(|input| matches!(input, LogicalPlan::Scan(_)) == scans)
                        .all(|input| self.add_plan(input))
                })
            }
            LogicalPlan::Union(inputs) if inputs.is_empty() => true,
            LogicalPlan::Filter { condition, input } => match unwrapped(input) {
                LogicalPlan::Path {
                    subject,
                    path,
                    object,
                } if !contains_any_exists(condition) => {
                    self.paths.push(PathPattern {
                        subject,
                        path,
                        object,
                        filter: Some(condition),
                    });
                    true
                }
                LogicalPlan::Scan(triple) => self.add_filtered(std::iter::once(triple), condition),
                LogicalPlan::Join(inputs)
                    if inputs.iter().all(|p| matches!(p, LogicalPlan::Scan(_))) =>
                {
                    self.add_filtered(
                        inputs.iter().map(|p| {
                            let LogicalPlan::Scan(triple) = p else {
                                unreachable!("scans only")
                            };
                            triple
                        }),
                        condition,
                    )
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn add_filtered(
        &mut self,
        patterns: impl Iterator<Item = &'q TriplePattern> + Clone,
        expr: &'q Expression,
    ) -> bool {
        // A filter may move only within its own scope: a variable bound by another
        // join input was unbound here and can change errors/BOUND results.
        let own: Vec<Variable> = patterns.clone().flat_map(triple_variables).collect();
        let mut conjuncts = Vec::new();
        pushdown::conjuncts_of(expr, &mut conjuncts);
        if contains_any_exists(expr)
            || !conjuncts.iter().all(|c| {
                pushdown::movable(c) && expression_variables(c).iter().all(|v| own.contains(v))
            })
        {
            return false;
        }
        self.triples.extend(patterns.cloned());
        self.conjuncts.extend(conjuncts);
        true
    }

    /// The `i`-th input: the triple patterns first, then the paths.
    pub(super) fn part(&self, i: usize) -> Part<'_, 'q> {
        match self.triples.get(i) {
            Some(triple) => Part::Triple(triple),
            None => Part::Path(&self.paths[i - self.triples.len()]),
        }
    }

    /// Every variable the join binds.
    fn variables(&self) -> Vec<Variable> {
        let mut vars = Vec::new();
        let ends = self
            .paths
            .iter()
            .flat_map(|p| [end_variable(p.subject), end_variable(p.object)])
            .flatten();
        for v in self.triples.iter().flat_map(triple_variables).chain(ends) {
            if !vars.contains(&v) {
                vars.push(v);
            }
        }
        vars
    }
}

/// Lowering removes one-input joins/unions before path eligibility is inspected.
fn unwrapped(mut plan: &LogicalPlan) -> &LogicalPlan {
    while let LogicalPlan::Join(inputs) | LogicalPlan::Union(inputs) = plan {
        let [inner] = inputs.as_slice() else { break };
        plan = inner;
    }
    plan
}

/// The variable at a path's end (a blank node is one, as in the executor), if it isn't a
/// constant.
fn end_variable(term: &TermPattern) -> Option<Variable> {
    match term {
        TermPattern::Variable(v) => Some(v.clone()),
        TermPattern::BlankNode(b) => {
            Some(Variable::new_unchecked(format!("_bnode_{}", b.as_str())))
        }
        _ => None,
    }
}

impl Context<'_> {
    /// The order of `join`'s inputs (triple patterns, then paths) by the join orderer, with
    /// the estimated rows after each; `None` if a triple pattern names a term the store
    /// doesn't have (no matches).
    pub(super) fn path_join_order(&self, join: &PathJoin<'_>) -> Option<plan::Plan> {
        let scans: Option<Vec<_>> = join.triples.iter().map(|t| self.scan_pattern(t)).collect();
        let scans = scans?;
        let counts: Vec<u64> = scans
            .iter()
            .map(|scan| self.snapshot.estimate_in(self.model, &scan.quad_pattern()))
            .collect();
        let mut vars: Vec<Variable> = Vec::new();
        let mut inputs = self.plan_inputs(&scans, &counts, &mut vars);
        for path in &join.paths {
            let rows = self.path_size(path) * path.filter.map_or(1.0, pushdown::selectivity);
            let count = rows.max(0.0).round() as u64;
            // A variable end has as many values as the path has starts (its rows over the
            // fan-out from each) or ends; with a constant at the other end, one per row.
            let values = |forward: bool, other: &TermPattern| {
                let fanout = self.path_fanout(path.path, forward);
                let values = match end_variable(other) {
                    Some(_) if fanout > 0.0 => rows / fanout,
                    _ => rows,
                };
                (values.round() as u64).clamp(1, count.max(1))
            };
            let mut ends: Vec<(usize, u64)> = Vec::new();
            for (end, other, forward) in [
                (path.subject, path.object, true),
                (path.object, path.subject, false),
            ] {
                if let Some(v) = end_variable(end) {
                    let index = variable_index(&mut vars, v);
                    if !ends.iter().any(|&(i, _)| i == index) {
                        ends.push((index, values(forward, other)));
                    }
                }
            }
            inputs.push(plan::Input {
                count,
                vars: ends,
                star: None,
                edge_object: None,
                overlaps: Vec::new(),
            });
        }
        Some(self.order_inputs(&inputs, vars.len()))
    }

    /// `join` evaluated in the order of [`Self::path_join_order`].
    pub(super) fn join_with_paths(&self, join: &PathJoin<'_>) -> NativeResult<Solutions> {
        let start = Instant::now();
        let Some(order) = self.path_join_order(join) else {
            let vars = join.variables();
            let width = vars.len();
            return Ok(Solutions {
                vars,
                table: IdTable::new(width),
                ordered: false,
            });
        };
        let mut filters: Vec<(&Expression, Vec<Variable>)> = join
            .conjuncts
            .iter()
            .map(|&c| (c, expression_variables(c)))
            .collect();
        let hints: Vec<ranges::Hint> = join
            .conjuncts
            .iter()
            .flat_map(|c| ranges::hints(c, &self.snapshot))
            .collect();
        let mut result: Option<Solutions> = None;
        let mut at = 0;
        while at < order.order.len() {
            match join.part(order.order[at]) {
                Part::Triple(first) => {
                    // A run of triple patterns is one basic graph pattern.
                    let mut run = vec![first.clone()];
                    while let Some(&next) = order.order.get(at + 1)
                        && let Part::Triple(triple) = join.part(next)
                    {
                        run.push(triple.clone());
                        at += 1;
                    }
                    result = Some(match result.take() {
                        None => self.bgp(&run, &hints, &mut filters)?,
                        Some(seed) => self.bgp_from(seed, &run, &mut filters)?,
                    });
                }
                Part::Path(path) => {
                    let joined = match result.take() {
                        None => self.filtered_path(path, None, Instant::now())?,
                        Some(seed) => {
                            let reached = self.path_from(&seed, path)?;
                            self.join(seed, reached)?
                        }
                    };
                    result = Some(self.filter_bound(joined, &mut filters)?);
                }
            }
            at += 1;
        }
        let mut result = result.expect("a join has inputs");
        for (conjunct, _) in filters {
            result = self.filter(result, conjunct)?;
        }
        if self.trace.is_some() {
            let detail = order
                .order
                .iter()
                .map(|&i| match join.part(i) {
                    Part::Triple(triple) => triple.to_string(),
                    Part::Path(path) => format!("{} {} {}", path.subject, path.path, path.object),
                })
                .collect::<Vec<_>>()
                .join(", then ");
            let estimate = order.rows.last().map(|r| r.max(0.0).round() as u64);
            let rows = result.table.len();
            self.note("paths ordered with patterns", detail, estimate, rows, start);
        }
        Ok(result)
    }

    /// EXPLAIN before running for a join of patterns and paths: its inputs in the planned
    /// order, each with its estimate alone, at `depth`; the estimate of the whole.
    pub(super) fn path_join_steps(
        &self,
        join: &PathJoin<'_>,
        depth: usize,
        steps: &mut Vec<PlannedStep>,
    ) -> f64 {
        let Some(order) = self.path_join_order(join) else {
            return 0.0;
        };
        for &i in &order.order {
            let (operator, detail, rows) = match join.part(i) {
                Part::Triple(triple) => {
                    let rows = self.scan_pattern(triple).map_or(0, |scan| {
                        self.snapshot.estimate_in(self.model, &scan.quad_pattern())
                    });
                    ("scan", triple.to_string(), rows)
                }
                Part::Path(path) => {
                    let rows = self.path_size(path).max(0.0).round() as u64;
                    let detail = format!("{} {} {}", path.subject, path.path, path.object);
                    ("path", detail, rows)
                }
            };
            steps.push(PlannedStep {
                depth,
                operator: operator.to_owned(),
                detail,
                estimated_rows: Some(rows),
            });
        }
        order.rows.last().copied().unwrap_or(0.0)
    }
}
