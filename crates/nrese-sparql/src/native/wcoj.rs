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

use std::borrow::Cow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nrese_engine::quad::Permutation;
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
    /// Solutions allowed (the query's memory budget).
    pub max_rows: usize,
    /// The value lists of patterns with a single variable, read once: the same under any
    /// bindings (LUBM q2 intersected the 2007 departments for each of 1000 universities).
    lists: Vec<OnceLock<Option<Vec<u64>>>>,
}

/// Why a join stopped early.
pub(super) enum Stop {
    Cancelled,
    TooManyRows,
}

/// A pattern at most this many times larger than the smallest one is intersected as a
/// sorted list rather than checked by one lookup per candidate.
const LIST_FACTOR: u64 = 32;

/// Chunks of the first variable's candidates per thread, for balance.
const CHUNKS_PER_THREAD: usize = 4;

/// Solutions a thread produces between two checks of the shared row count.
const ROW_STEP: usize = 4096;

/// State shared by the threads of one join.
struct Shared<'t> {
    cancelled: AtomicBool,
    rows: AtomicUsize,
    token: &'t (dyn Fn() -> bool + Sync),
}

/// What a join did, for EXPLAIN.
#[derive(Debug, Default)]
pub(super) struct Stats {
    /// The variable order.
    pub order: Vec<usize>,
    /// Candidate values produced, per depth.
    pub candidates: Vec<AtomicUsize>,
    /// Index lookups (counts) made to check candidates.
    pub lookups: AtomicUsize,
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

impl<'a> Query<'a> {
    pub(super) fn new(
        snapshot: &'a Snapshot,
        model: ReadModel,
        patterns: Vec<[Pos; 3]>,
        variables: usize,
        max_rows: usize,
    ) -> Self {
        let lists = patterns.iter().map(|_| OnceLock::new()).collect();
        Self {
            snapshot,
            model,
            patterns,
            variables,
            max_rows,
            lists,
        }
    }

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

    /// The candidate values of `var` under `bindings`, sorted and distinct, and which
    /// patterns they already satisfy; `None` if some pattern with `var` can't match.
    ///
    /// The pattern with the fewest matches supplies the values. Every other pattern in
    /// which `var` is the last unbound position and that is at most [`LIST_FACTOR`] times
    /// larger is intersected as a sorted index range (leapfrog style): a sequential scan
    /// costs a few nanoseconds per value, a lookup per candidate far more. The remaining
    /// patterns are checked per candidate ([`consistent`](Self::consistent)).
    fn candidates(&self, var: usize, bindings: &[Option<u64>]) -> Option<(Vec<u64>, Vec<bool>)> {
        let mut counts: Vec<Option<u64>> = vec![None; self.patterns.len()];
        let mut best: Option<(u64, usize)> = None;
        for (i, pattern) in self.patterns.iter().enumerate() {
            if !pattern.contains(&Pos::Var(var)) {
                continue;
            }
            let count = self.count(pattern, bindings);
            if count == 0 {
                return None;
            }
            counts[i] = Some(count);
            if best.is_none_or(|(c, _)| count < c) {
                best = Some((count, i));
            }
        }
        let (smallest, driver) = best?;
        let mut enforced = vec![false; self.patterns.len()];
        enforced[driver] = true;
        let mut values = match self.sorted_values(driver, var, bindings) {
            Some(values) => values.into_owned(),
            None => {
                let pattern = &self.patterns[driver];
                let positions: Vec<usize> =
                    (0..3).filter(|&i| pattern[i] == Pos::Var(var)).collect();
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
                values
            }
        };
        let mut others: Vec<(u64, usize)> = counts
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.map(|c| (c, i)))
            .filter(|&(c, i)| i != driver && c <= smallest.saturating_mul(LIST_FACTOR))
            .collect();
        others.sort_unstable();
        for (_, i) in others {
            if values.is_empty() {
                break;
            }
            if let Some(list) = self.sorted_values(i, var, bindings) {
                values = intersect(&values, &list);
                enforced[i] = true;
            }
        }
        Some((values, enforced))
    }

    /// The values of `var` in pattern `index`'s matches, in id order, if `var` is its only
    /// unbound position (then an index range lists them sorted and distinct). A pattern
    /// with no other variable gives the same list under any bindings: read once.
    fn sorted_values(
        &self,
        index: usize,
        var: usize,
        bindings: &[Option<u64>],
    ) -> Option<Cow<'_, [u64]>> {
        let pattern = &self.patterns[index];
        let only_var = pattern
            .iter()
            .all(|&p| matches!(p, Pos::Const(_)) || p == Pos::Var(var));
        if only_var {
            return self.lists[index]
                .get_or_init(|| self.read_values(index, var, bindings))
                .as_deref()
                .map(Cow::Borrowed);
        }
        self.read_values(index, var, bindings).map(Cow::Owned)
    }

    /// [`Self::sorted_values`], read from the index.
    fn read_values(&self, index: usize, var: usize, bindings: &[Option<u64>]) -> Option<Vec<u64>> {
        let pattern = &self.patterns[index];
        let mut free = (0..3).filter(|&c| match pattern[c] {
            Pos::Var(v) => bindings[v].is_none(),
            Pos::Const(_) => false,
        });
        let position = free.next()?;
        if free.next().is_some() || pattern[position] != Pos::Var(var) {
            return None;
        }
        // A graph-first order with the free component last.
        let permutation = match position {
            0 => Permutation::Gpos,
            1 => Permutation::Gosp,
            _ => Permutation::Gspo,
        };
        let quads = self.snapshot.scan_sorted_in(
            self.model,
            &self.quad_pattern(pattern, bindings),
            permutation,
        )?;
        Some(
            quads
                .map(|quad| [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()][position])
                .collect(),
        )
    }

    /// Whether every pattern with `var` not in `enforced` still matches once `var` is
    /// bound (exact for the patterns this binding completes).
    fn consistent(
        &self,
        var: usize,
        bindings: &[Option<u64>],
        enforced: &[bool],
        stats: &Stats,
    ) -> bool {
        self.patterns
            .iter()
            .zip(enforced)
            .filter(|(p, done)| !**done && p.contains(&Pos::Var(var)))
            .all(|(p, _)| {
                if repeats_unbound(p, bindings) {
                    // Checked exactly once its last variable is bound.
                    return true;
                }
                stats.lookups.fetch_add(1, Ordering::Relaxed);
                self.snapshot
                    .exists_in(self.model, &self.quad_pattern(p, bindings))
            })
    }

    fn extend(
        &self,
        order: &[usize],
        depth: usize,
        bindings: &mut Vec<Option<u64>>,
        rows: &mut Vec<u64>,
        shared: &Shared<'_>,
        stats: &Stats,
    ) -> Result<(), Stop> {
        let Some(&var) = order.get(depth) else {
            rows.extend(bindings.iter().map(|b| b.expect("every variable is bound")));
            if (rows.len() / self.variables.max(1)).is_multiple_of(ROW_STEP) {
                let total = shared.rows.fetch_add(ROW_STEP, Ordering::Relaxed) + ROW_STEP;
                if total > self.max_rows {
                    return Err(Stop::TooManyRows);
                }
            }
            return Ok(());
        };
        let Some((values, enforced)) = self.candidates(var, bindings) else {
            return Ok(());
        };
        stats.candidates[depth].fetch_add(values.len(), Ordering::Relaxed);
        for (n, value) in values.into_iter().enumerate() {
            if n % 1024 == 0 && (shared.cancelled.load(Ordering::Relaxed) || (shared.token)()) {
                shared.cancelled.store(true, Ordering::Relaxed);
                return Err(Stop::Cancelled);
            }
            bindings[var] = Some(value);
            if self.consistent(var, bindings, &enforced, stats) {
                self.extend(order, depth + 1, bindings, rows, shared, stats)?;
            }
        }
        bindings[var] = None;
        Ok(())
    }

    /// Every solution, as rows of `variables` ids (row-major).
    pub(super) fn run(
        &self,
        token: &(dyn Fn() -> bool + Sync),
        stats: &mut Stats,
    ) -> Result<Vec<u64>, Stop> {
        let order = self.order();
        stats.order.clone_from(&order);
        stats.candidates = (0..order.len()).map(|_| AtomicUsize::new(0)).collect();
        let stats = &*stats;
        let bindings = vec![None; self.variables];
        let Some((first, enforced)) = self.candidates(order[0], &bindings) else {
            return Ok(Vec::new());
        };
        stats.candidates[0].fetch_add(first.len(), Ordering::Relaxed);
        let shared = Shared {
            cancelled: AtomicBool::new(false),
            rows: AtomicUsize::new(0),
            token,
        };
        // A few chunks per thread, at most 256 values each: 1000 universities (LUBM q2) in
        // chunks of 256 kept 4 of 32 threads busy.
        let chunk = first
            .len()
            .div_ceil(rayon::current_num_threads() * CHUNKS_PER_THREAD)
            .clamp(1, 256);
        let chunks: Vec<Result<Vec<u64>, Stop>> = first
            .par_chunks(chunk)
            .map(|chunk| {
                let mut bindings = vec![None; self.variables];
                let mut rows = Vec::new();
                for &value in chunk {
                    bindings[order[0]] = Some(value);
                    if self.consistent(order[0], &bindings, &enforced, stats) {
                        self.extend(&order, 1, &mut bindings, &mut rows, &shared, stats)?;
                    }
                }
                Ok(rows)
            })
            .collect();
        let mut rows = Vec::new();
        for chunk in chunks {
            rows.extend(chunk?);
        }
        if rows.len() / self.variables.max(1) > self.max_rows {
            return Err(Stop::TooManyRows);
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

/// The values in both sorted, distinct lists: a merge that gallops over the longer one.
fn intersect(small: &[u64], large: &[u64]) -> Vec<u64> {
    let (small, large) = if small.len() <= large.len() {
        (small, large)
    } else {
        (large, small)
    };
    let mut out = Vec::with_capacity(small.len());
    let mut rest = large;
    for &value in small {
        // Exponential then binary search for the first element >= value.
        let mut step = 1;
        while step < rest.len() && rest[step - 1] < value {
            step *= 2;
        }
        let skip = rest[..step.min(rest.len())].partition_point(|&x| x < value);
        rest = &rest[skip..];
        match rest.first() {
            Some(&x) if x == value => out.push(value),
            Some(_) => {}
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::intersect;

    #[test]
    fn intersect_matches_a_set_intersection() {
        let mut state = 9u64;
        let mut next = |n: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 33) % n
        };
        for _ in 0..500 {
            let mut a: Vec<u64> = (0..next(40)).map(|_| next(100)).collect();
            let mut b: Vec<u64> = (0..next(400)).map(|_| next(100)).collect();
            for v in [&mut a, &mut b] {
                v.sort_unstable();
                v.dedup();
            }
            let expected: Vec<u64> = a.iter().copied().filter(|x| b.contains(x)).collect();
            assert_eq!(intersect(&a, &b), expected);
            assert_eq!(intersect(&b, &a), expected);
        }
    }
}
