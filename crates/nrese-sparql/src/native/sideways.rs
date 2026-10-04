//! Sideways information passing: a pattern joined to rows already computed is evaluated
//! from those rows (work package W3b).
//!
//! [`Context::eval_from`] computes `Join(seed, pattern)` without evaluating `pattern` on
//! its own where the operator allows it:
//!
//! | Pattern | Evaluated as |
//! |---|---|
//! | basic graph pattern (with its filters) | joined pattern by pattern, probing the index per seed row where the seed is small |
//! | property path | followed from the seed's values of its ends |
//! | `P1 . P2` | `eval_from(eval_from(seed, P1), P2)` |
//! | `P1 UNION P2` | the union of both, each from the seed |
//! | `OPTIONAL`, `BIND`, `FILTER` | inside, when every seed variable the part reads is certainly bound by the part it applies to (else the seed's value would change what it computes) |
//!
//! Anything else is evaluated alone and joined. A join of two groups starts with the one
//! the statistics say is smaller, unless an ORDER BY inside either fixes the row order.
//! The right side of an OPTIONAL is evaluated from the left side's distinct values of the
//! variables the right side certainly binds.

use std::collections::HashSet;

use nrese_exec::{IdTable, UNDEF};
use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};

use super::exists::deep_variables;
use super::exists::mentioned;
use super::{Context, NativeResult, PROBE_FACTOR, Solutions, as_path, bound_variables, pushdown};

/// Whether an ORDER BY inside `pattern` fixes the order of its rows.
fn ordered(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::OrderBy { .. } => true,
        GraphPattern::Join { left, right }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => ordered(left) || ordered(right),
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::Graph { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Group { inner, .. } => ordered(inner),
        _ => false,
    }
}

/// Whether the seed may go inside a part that reads `read` and whose own pattern
/// certainly binds `certain`: every seed variable read there is bound there.
fn reads_only_bound(read: &[Variable], certain: &[Variable], seed: &Solutions) -> bool {
    read.iter()
        .all(|v| seed.column(v).is_none() || certain.contains(v))
}

impl Context<'_> {
    /// A cost estimate of `pattern` alone: the rows of its most selective triple pattern,
    /// summed over union branches. Only its order of magnitude matters.
    pub(super) fn estimate(&self, pattern: &GraphPattern) -> f64 {
        let large = self.snapshot.len_in(self.model) as f64 + 1.0;
        match pattern {
            GraphPattern::Bgp { patterns } => {
                if patterns.is_empty() {
                    return 1.0;
                }
                patterns
                    .iter()
                    .map(|triple| match self.scan_pattern(triple) {
                        Some(scan) => {
                            self.snapshot.estimate_in(self.model, &scan.quad_pattern()) as f64
                        }
                        None => 0.0,
                    })
                    .fold(f64::INFINITY, f64::min)
            }
            GraphPattern::Join { left, right } => self.estimate(left).min(self.estimate(right)),
            GraphPattern::Union { left, right } => self.estimate(left) + self.estimate(right),
            GraphPattern::LeftJoin { left, .. } | GraphPattern::Minus { left, .. } => {
                self.estimate(left)
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Extend { inner, .. }
            | GraphPattern::Graph { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Group { inner, .. } => self.estimate(inner),
            GraphPattern::Slice { inner, length, .. } => {
                let inner = self.estimate(inner);
                length.map_or(inner, |l| inner.min(l as f64))
            }
            GraphPattern::Values { bindings, .. } => bindings.len() as f64,
            _ => large,
        }
    }

    /// `Join(left, right)`, the smaller side first and the other evaluated from it.
    ///
    /// A `VALUES` goes first whatever the other side's estimate: its rows are given, so
    /// starting from them costs nothing, and if probing from them doesn't pay the other
    /// side is evaluated alone as it would have been. A BGP's estimate is its smallest
    /// pattern's count, which says nothing of what its joins expand to: a one-row
    /// `VALUES` against a star whose smallest pattern had 7 statements stayed second, and
    /// the star was evaluated for all 673,884 provenance chains of the Zebratlas release
    /// before the one edge was joined (4 October 2026).
    pub(super) fn join_sideways(
        &self,
        left: &GraphPattern,
        right: &GraphPattern,
    ) -> NativeResult<Solutions> {
        let values = |p: &GraphPattern| matches!(p, GraphPattern::Values { .. });
        let swap = !ordered(left)
            && !ordered(right)
            && !values(left)
            && (values(right) || self.estimate(right) * PROBE_FACTOR as f64 <= self.estimate(left));
        let (first, second) = if swap { (right, left) } else { (left, right) };
        let bound = self.eval(first)?;
        self.eval_from(bound, second)
    }

    /// `Join(seed, pattern)`, with `pattern` evaluated from the seed where it can be.
    pub(super) fn eval_from(
        &self,
        seed: Solutions,
        pattern: &GraphPattern,
    ) -> NativeResult<Solutions> {
        if seed.vars.is_empty() && seed.table.len() == 1 {
            return self.eval(pattern);
        }
        if seed.table.is_empty() {
            let mut vars = seed.vars.clone();
            bound_variables(pattern, &mut vars);
            let width = vars.len();
            self.consumed(&seed);
            return Ok(Solutions {
                vars,
                table: IdTable::new(width),
                ordered: false,
            });
        }
        if let Some(path) = as_path(pattern) {
            let reached = self.path_from(&seed, &path)?;
            return self.join(seed, reached);
        }
        match pattern {
            GraphPattern::Bgp { patterns } if self.probes_pay(&seed, patterns) => {
                self.bgp_from(seed, patterns, &mut Vec::new())
            }
            GraphPattern::Filter { expr, inner } => {
                let read = deep_variables(expr);
                if !reads_only_bound(&read, &pushdown::certain(inner), &seed) {
                    return self.join_alone(seed, pattern);
                }
                if let GraphPattern::Bgp { patterns } = &**inner
                    && self.probes_pay(&seed, patterns)
                {
                    let mut all = Vec::new();
                    pushdown::conjuncts_of(expr, &mut all);
                    let (early, late): (Vec<_>, Vec<_>) =
                        all.into_iter().partition(|c| pushdown::movable(c));
                    let mut early: Vec<(&Expression, Vec<Variable>)> = early
                        .into_iter()
                        .map(|c| (c, super::expression_variables(c)))
                        .collect();
                    let mut solutions = self.bgp_from(seed, patterns, &mut early)?;
                    for conjunct in early.into_iter().map(|(c, _)| c).chain(late) {
                        solutions = self.filter(solutions, conjunct)?;
                    }
                    return Ok(solutions);
                }
                let solutions = self.eval_from(seed, inner)?;
                self.filter(solutions, expr)
            }
            GraphPattern::Join { left, right } => {
                let solutions = self.eval_from(seed, left)?;
                self.eval_from(solutions, right)
            }
            GraphPattern::Union { left, right } => {
                let copy = Solutions {
                    vars: seed.vars.clone(),
                    table: seed.table.clone(),
                    ordered: seed.ordered,
                };
                let copy = self.produced(copy)?;
                let (a, b) = (self.eval_from(copy, left)?, self.eval_from(seed, right)?);
                self.union(a, b)
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                let mut read = mentioned(right);
                if let Some(expression) = expression {
                    read.extend(deep_variables(expression));
                }
                if !reads_only_bound(&read, &pushdown::certain(left), &seed) {
                    return self.join_alone(seed, pattern);
                }
                let left = self.eval_from(seed, left)?;
                self.optional(left, right, expression.as_ref())
            }
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } if seed.column(variable).is_none()
                && reads_only_bound(
                    &deep_variables(expression),
                    &pushdown::certain(inner),
                    &seed,
                ) =>
            {
                let solutions = self.eval_from(seed, inner)?;
                self.extend(solutions, variable, expression)
            }
            _ => self.join_alone(seed, pattern),
        }
    }

    fn join_alone(&self, seed: Solutions, pattern: &GraphPattern) -> NativeResult<Solutions> {
        let alone = self.eval(pattern)?;
        self.join(seed, alone)
    }

    /// Whether joining `seed` pattern by pattern will probe the index: the seed is small
    /// against some triple pattern it shares a variable with.
    fn probes_pay(
        &self,
        seed: &Solutions,
        patterns: &[nrese_sparql_syntax::term::TriplePattern],
    ) -> bool {
        patterns.iter().any(|triple| {
            super::triple_variables(triple)
                .iter()
                .any(|v| seed.column(v).is_some())
                && self.scan_pattern(triple).is_some_and(|scan| {
                    (seed.table.len() as u64).saturating_mul(PROBE_FACTOR)
                        < self.snapshot.estimate_in(self.model, &scan.quad_pattern())
                })
        })
    }

    /// `OPTIONAL { right } FILTER(expression)` after `left`: the right side from the
    /// left side's distinct values of the variables it certainly binds.
    pub(super) fn optional(
        &self,
        left: Solutions,
        right: &GraphPattern,
        expression: Option<&Expression>,
    ) -> NativeResult<Solutions> {
        let found = match as_path(right) {
            Some(path) => self.path_from(&left, &path)?,
            None => match self.keys(&left, &pushdown::certain(right)) {
                Some(keys) => self.eval_from(keys, right)?,
                None => self.eval(right)?,
            },
        };
        self.left_join(left, found, expression)
    }

    /// The distinct rows of `solutions` over the variables of `variables` it has; `None`
    /// if there are none, or one is unbound somewhere.
    fn keys(&self, solutions: &Solutions, variables: &[Variable]) -> Option<Solutions> {
        let columns: Vec<(Variable, usize)> = variables
            .iter()
            .filter_map(|v| solutions.column(v).map(|c| (v.clone(), c)))
            .collect();
        if columns.is_empty() {
            return None;
        }
        let mut seen: HashSet<Vec<u64>> = HashSet::new();
        let mut table = IdTable::new(columns.len());
        for r in 0..solutions.table.len() {
            let key: Vec<u64> = columns
                .iter()
                .map(|&(_, c)| solutions.table.get(r, c))
                .collect();
            if key.contains(&UNDEF) {
                return None;
            }
            if seen.insert(key.clone()) {
                table.push_row(&key);
            }
        }
        self.produced(Solutions {
            vars: columns.into_iter().map(|(v, _)| v).collect(),
            table,
            ordered: false,
        })
        .ok()
    }
}
