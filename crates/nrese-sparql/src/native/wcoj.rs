//! Worst-case-optimal joins for cyclic BGPs (XC5): Generic Join over the indexes (Ngo, Ré,
//! Rudra, "Skew strikes back", 2014; the family Leapfrog Triejoin belongs to).
//!
//! Binary join plans materialise intermediate results that the last pattern of a cycle
//! then prunes: LUBM q9 (student, advisor, course triangle) builds every student-course
//! pair before checking the advisor. Generic Join binds one variable at a time instead.
//! For each variable, the pattern with the fewest matches under the current bindings (the
//! engine's exact counts) supplies the candidates. Every other pattern containing the
//! variable must then still match, which is an index lookup. The work is bounded by the
//! AGM bound of the query, not by the size of intermediate joins.
//!
//! The first variable's candidates are split across threads (rayon); each thread extends
//! its share depth-first and collects rows. The output is unordered.

use std::sync::atomic::{AtomicBool, Ordering};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use rayon::prelude::*;

/// A pattern position: a constant id, or a variable (index into the query's variables).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pos {
    Const(u64),
    Var(usize),
}

/// A conjunctive query over the default graph.
pub(super) struct Query<'a> {
    pub snapshot: &'a Snapshot,
    pub model: ReadModel,
    pub patterns: Vec<[Pos; 3]>,
    pub variables: usize,
}

/// Why a join stopped early.
pub(super) enum Stop {
    Cancelled,
}

/// Whether the variable graph of `patterns` has a cycle: patterns connect the variables
/// they share, and a cycle means binary join plans build intermediate results that later
/// patterns prune. A three-variable pattern counts as a chain, not a triangle.
pub(super) fn cyclic(patterns: &[[Pos; 3]], variables: usize) -> bool {
    let mut parent: Vec<usize> = (0..variables).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for pattern in patterns {
        let mut vars: Vec<usize> = pattern
            .iter()
            .filter_map(|p| match p {
                Pos::Var(v) => Some(*v),
                Pos::Const(_) => None,
            })
            .collect();
        vars.dedup();
        for pair in vars.windows(2) {
            let edge = (pair[0].min(pair[1]), pair[0].max(pair[1]));
            if edge.0 != edge.1 && !edges.contains(&edge) {
                edges.push(edge);
            }
        }
    }
    for (a, b) in edges {
        let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
        if ra == rb {
            return true;
        }
        parent[ra] = rb;
    }
    false
}

impl Query<'_> {
    fn quad_pattern(&self, pattern: &[Pos; 3], bindings: &[Option<u64>]) -> QuadPattern {
        let value = |p: Pos| match p {
            Pos::Const(id) => Some(TermId::from_raw(id)),
            Pos::Var(v) => bindings[v].map(TermId::from_raw),
        };
        QuadPattern {
            subject: value(pattern[0]),
            predicate: value(pattern[1]),
            object: value(pattern[2]),
            graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        }
    }

    fn count(&self, pattern: &[Pos; 3], bindings: &[Option<u64>]) -> u64 {
        self.snapshot
            .count_in(self.model, &self.quad_pattern(pattern, bindings))
    }

    /// A variable order: start with the variable whose most selective pattern is smallest,
    /// then keep to variables connected to the bound ones, most selective first.
    fn order(&self) -> Vec<usize> {
        let none = vec![None; self.variables];
        let counts: Vec<u64> = self.patterns.iter().map(|p| self.count(p, &none)).collect();
        let selectivity = |v: usize| {
            self.patterns
                .iter()
                .zip(&counts)
                .filter(|(p, _)| p.contains(&Pos::Var(v)))
                .map(|(_, &c)| c)
                .min()
                .unwrap_or(u64::MAX)
        };
        let mut order: Vec<usize> = Vec::with_capacity(self.variables);
        let mut left: Vec<usize> = (0..self.variables).collect();
        while !left.is_empty() {
            let connected = |v: usize| {
                self.patterns.iter().any(|p| {
                    p.contains(&Pos::Var(v)) && order.iter().any(|&o| p.contains(&Pos::Var(o)))
                })
            };
            let (k, _) = left
                .iter()
                .enumerate()
                .min_by_key(|&(_, &v)| (!connected(v), selectivity(v)))
                .expect("left is not empty");
            order.push(left.remove(k));
        }
        order
    }

    /// The candidate values of `var` under `bindings`, from the pattern with the fewest
    /// matches, sorted and distinct; `None` if some pattern with `var` can't match.
    fn candidates(&self, var: usize, bindings: &[Option<u64>]) -> Option<Vec<u64>> {
        let mut best: Option<(u64, &[Pos; 3])> = None;
        for pattern in self.patterns.iter().filter(|p| p.contains(&Pos::Var(var))) {
            let count = self.count(pattern, bindings);
            if count == 0 {
                return None;
            }
            if best.is_none_or(|(c, _)| count < c) {
                best = Some((count, pattern));
            }
        }
        let (_, pattern) = best?;
        let positions: Vec<usize> = (0..3).filter(|&i| pattern[i] == Pos::Var(var)).collect();
        let mut values: Vec<u64> = Vec::new();
        for quad in self
            .snapshot
            .quads_for_pattern_in(self.model, &self.quad_pattern(pattern, bindings))
        {
            let components = [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()];
            let value = components[positions[0]];
            // A variable used twice in the pattern must bind one term.
            if positions[1..].iter().all(|&p| components[p] == value) {
                values.push(value);
            }
        }
        values.sort_unstable();
        values.dedup();
        Some(values)
    }

    /// Whether every pattern with `var` still matches once `var` is bound (exact for the
    /// patterns this binding completes).
    fn consistent(&self, var: usize, bindings: &[Option<u64>]) -> bool {
        self.patterns
            .iter()
            .filter(|p| p.contains(&Pos::Var(var)))
            .all(|p| {
                if repeats_unbound(p, bindings) {
                    // Checked exactly once its last variable is bound.
                    return true;
                }
                self.count(p, bindings) > 0
            })
    }

    fn extend(
        &self,
        order: &[usize],
        depth: usize,
        bindings: &mut Vec<Option<u64>>,
        rows: &mut Vec<u64>,
        cancelled: &AtomicBool,
        token: &dyn Fn() -> bool,
    ) -> Result<(), Stop> {
        let Some(&var) = order.get(depth) else {
            rows.extend(bindings.iter().map(|b| b.expect("every variable is bound")));
            return Ok(());
        };
        let Some(values) = self.candidates(var, bindings) else {
            return Ok(());
        };
        for (n, value) in values.into_iter().enumerate() {
            if n % 1024 == 0 && (cancelled.load(Ordering::Relaxed) || token()) {
                cancelled.store(true, Ordering::Relaxed);
                return Err(Stop::Cancelled);
            }
            bindings[var] = Some(value);
            if self.consistent(var, bindings) {
                self.extend(order, depth + 1, bindings, rows, cancelled, token)?;
            }
        }
        bindings[var] = None;
        Ok(())
    }

    /// Every solution, as rows of `variables` ids (row-major).
    pub(super) fn run(&self, token: &(dyn Fn() -> bool + Sync)) -> Result<Vec<u64>, Stop> {
        let order = self.order();
        let bindings = vec![None; self.variables];
        let Some(first) = self.candidates(order[0], &bindings) else {
            return Ok(Vec::new());
        };
        let cancelled = AtomicBool::new(false);
        let chunks: Vec<Result<Vec<u64>, Stop>> = first
            .par_chunks(256)
            .map(|chunk| {
                let mut bindings = vec![None; self.variables];
                let mut rows = Vec::new();
                for &value in chunk {
                    bindings[order[0]] = Some(value);
                    if self.consistent(order[0], &bindings) {
                        self.extend(&order, 1, &mut bindings, &mut rows, &cancelled, token)?;
                    }
                }
                Ok(rows)
            })
            .collect();
        let mut rows = Vec::new();
        for chunk in chunks {
            rows.extend(chunk?);
        }
        Ok(rows)
    }
}

/// Whether `pattern` has a variable at two positions that `bindings` leave unbound: its
/// count then over-approximates (the positions must bind one term).
fn repeats_unbound(pattern: &[Pos; 3], bindings: &[Option<u64>]) -> bool {
    let unbound: Vec<usize> = pattern
        .iter()
        .filter_map(|p| match p {
            Pos::Var(v) if bindings[*v].is_none() => Some(*v),
            _ => None,
        })
        .collect();
    unbound
        .iter()
        .enumerate()
        .any(|(i, v)| unbound[i + 1..].contains(v))
}
