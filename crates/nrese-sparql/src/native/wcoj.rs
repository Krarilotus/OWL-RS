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
use nrese_engine::{GraphSelector, ProbeCursor, QuadPattern, ReadModel, Seek, Snapshot, TermId};
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

/// Candidates of the first variable a sampled estimate joins ([`Query::estimate`]).
const SAMPLES: usize = 128;

/// Lookups after which a sampled estimate scales what it has.
const SAMPLE_LOOKUPS: usize = 100_000;

/// Solutions after which a sampled estimate stops ([`Query::max_rows`] of its query).
pub(super) const SAMPLE_ROWS: usize = 1 << 20;

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
    /// Checks of candidates (leapfrog seeks).
    pub lookups: AtomicUsize,
    /// The runs' seeks those checks made, and how many of them searched from the root.
    pub seeks: AtomicUsize,
    pub roots: AtomicUsize,
    /// Candidates skipped: a check named a next value past them.
    pub skipped: AtomicUsize,
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
            .estimate_in(self.model, &self.quad_pattern(pattern, bindings))
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
    fn candidates(
        &self,
        var: usize,
        depth: usize,
        work: &mut Work<'a>,
    ) -> Option<(Vec<u64>, Vec<bool>)> {
        let mut counts: Vec<Option<u64>> = vec![None; self.patterns.len()];
        let mut best: Option<(u64, usize)> = None;
        for (i, pattern) in self.patterns.iter().enumerate() {
            if !pattern.contains(&Pos::Var(var)) {
                continue;
            }
            let quad = self.quad_pattern(pattern, &work.bindings);
            let count = work.cursor(depth, i, Use::Count, self).count(&quad);
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
        let mut values = match self.sorted_values(driver, var, depth, work) {
            Some(values) => values.into_owned(),
            None => {
                let pattern = &self.patterns[driver];
                let positions: Vec<usize> =
                    (0..3).filter(|&i| pattern[i] == Pos::Var(var)).collect();
                let mut values: Vec<u64> = Vec::new();
                for quad in self
                    .snapshot
                    .quads_for_pattern_in(self.model, &self.quad_pattern(pattern, &work.bindings))
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
            if let Some(list) = self.sorted_values(i, var, depth, work) {
                values = intersect(&values, &list);
                enforced[i] = true;
            }
        }
        Some((values, enforced))
    }

    /// The values of `var` in pattern `index`'s matches, in id order, if `var` is its only
    /// unbound position (then an index range lists them sorted and distinct), read by the
    /// pattern's listing cursor at `depth`. A pattern with no other variable gives the
    /// same list under any bindings: read once.
    fn sorted_values(
        &self,
        index: usize,
        var: usize,
        depth: usize,
        work: &mut Work<'a>,
    ) -> Option<Cow<'_, [u64]>> {
        let pattern = &self.patterns[index];
        let only_var = pattern
            .iter()
            .all(|&p| matches!(p, Pos::Const(_)) || p == Pos::Var(var));
        if only_var {
            return self.lists[index]
                .get_or_init(|| self.read_values(index, var, &work.bindings))
                .as_deref()
                .map(Cow::Borrowed);
        }
        let position = self.free_position(index, var, &work.bindings)?;
        let quad = self.quad_pattern(pattern, &work.bindings);
        let mut values = Vec::new();
        if work
            .cursor(depth, index, Use::Values, self)
            .values(&quad, position, &mut values)
        {
            return Some(Cow::Owned(values));
        }
        self.read_values(index, var, &work.bindings).map(Cow::Owned)
    }

    /// The position of `var` in pattern `index` if it is its only unbound position.
    fn free_position(&self, index: usize, var: usize, bindings: &[Option<u64>]) -> Option<usize> {
        let pattern = &self.patterns[index];
        let mut free = (0..3).filter(|&c| match pattern[c] {
            Pos::Var(v) => bindings[v].is_none(),
            Pos::Const(_) => false,
        });
        let position = free.next()?;
        (free.next().is_none() && pattern[position] == Pos::Var(var)).then_some(position)
    }

    /// [`Self::sorted_values`], read from the index.
    fn read_values(&self, index: usize, var: usize, bindings: &[Option<u64>]) -> Option<Vec<u64>> {
        let pattern = &self.patterns[index];
        let position = self.free_position(index, var, bindings)?;
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

    /// Calls `extend` deeper for each of `values` (ascending) of `var` with which every
    /// pattern containing `var` and not in `enforced` still matches (exact for the
    /// patterns the binding completes). Each check is a leapfrog seek of the pattern's
    /// cursor ([`ProbeCursor::seek`]): the candidates ascend, so it reads forward, and a
    /// pattern that fails names the next value of `var` that can match it, so the
    /// candidates below are skipped (or all the rest, where none can).
    #[allow(clippy::too_many_arguments)]
    fn extend_values(
        &self,
        order: &[usize],
        depth: usize,
        values: &[u64],
        enforced: &[bool],
        work: &mut Work<'a>,
        rows: &mut Vec<u64>,
        shared: &Shared<'_>,
        stats: &Stats,
    ) -> Result<(), Stop> {
        let var = order[depth];
        let mut i = 0;
        let mut seen = 0usize;
        while i < values.len() {
            seen += 1;
            if seen.is_multiple_of(1024)
                && (shared.cancelled.load(Ordering::Relaxed) || (shared.token)())
            {
                shared.cancelled.store(true, Ordering::Relaxed);
                return Err(Stop::Cancelled);
            }
            let value = values[i];
            work.bindings[var] = Some(value);
            let mut skip_to = 0u64;
            let mut passed = true;
            let mut exhausted = false;
            for (p, pattern) in self.patterns.iter().enumerate() {
                // The patterns with `var` not enforced, and where `var` sits in each
                // (`None`: more than once, so no next value can be named).
                if enforced[p] {
                    continue;
                }
                let mut places = (0..3).filter(|&c| pattern[c] == Pos::Var(var));
                let Some(first) = places.next() else {
                    continue;
                };
                let at = places.next().is_none().then_some(first);
                if repeats_unbound(pattern, &work.bindings) {
                    // Checked exactly once its last variable is bound.
                    continue;
                }
                stats.lookups.fetch_add(1, Ordering::Relaxed);
                let quad = self.quad_pattern(pattern, &work.bindings);
                let cursor = work.cursor(depth, p, Use::Seek, self);
                let seek = match at {
                    Some(c) => cursor.seek(&quad, c),
                    None => match cursor.exists(&quad) {
                        true => Seek::Found,
                        false => Seek::Missing,
                    },
                };
                match seek {
                    Seek::Found => {}
                    Seek::Next(next) => {
                        passed = false;
                        skip_to = skip_to.max(next);
                    }
                    Seek::Missing => passed = false,
                    Seek::Exhausted => {
                        // No larger value of `var` matches this pattern.
                        exhausted = true;
                        break;
                    }
                }
            }
            if exhausted {
                stats
                    .skipped
                    .fetch_add(values.len() - i - 1, Ordering::Relaxed);
                break;
            }
            if passed {
                self.extend(order, depth + 1, work, rows, shared, stats)?;
                i += 1;
            } else if skip_to > value {
                let next = i + 1 + values[i + 1..].partition_point(|&v| v < skip_to);
                stats.skipped.fetch_add(next - i - 1, Ordering::Relaxed);
                i = next;
            } else {
                i += 1;
            }
        }
        work.bindings[var] = None;
        Ok(())
    }

    fn extend(
        &self,
        order: &[usize],
        depth: usize,
        work: &mut Work<'a>,
        rows: &mut Vec<u64>,
        shared: &Shared<'_>,
        stats: &Stats,
    ) -> Result<(), Stop> {
        let Some(&var) = order.get(depth) else {
            rows.extend(
                work.bindings
                    .iter()
                    .map(|b| b.expect("every variable is bound")),
            );
            if (rows.len() / self.variables.max(1)).is_multiple_of(ROW_STEP) {
                let total = shared.rows.fetch_add(ROW_STEP, Ordering::Relaxed) + ROW_STEP;
                if total > self.max_rows {
                    return Err(Stop::TooManyRows);
                }
            }
            return Ok(());
        };
        let Some((values, enforced)) = self.candidates(var, depth, work) else {
            return Ok(());
        };
        stats.candidates[depth].fetch_add(values.len(), Ordering::Relaxed);
        self.extend_values(order, depth, &values, &enforced, work, rows, shared, stats)
    }

    /// The solutions estimated by sampling (index-based join sampling, Leis et al., CIDR
    /// 2017): the join run for up to [`SAMPLES`] candidates of the first variable, evenly
    /// spaced over them, scaled by their number. Deterministic; stops early past
    /// [`SAMPLE_LOOKUPS`] lookups or the query's `max_rows`, scaling what it has.
    pub(super) fn estimate(&self) -> f64 {
        let order = self.order();
        let mut work = Work::new(self, order.len());
        let Some((first, enforced)) = self.candidates(order[0], 0, &mut work) else {
            return 0.0;
        };
        if first.is_empty() {
            return 0.0;
        }
        let stats = Stats {
            candidates: (0..order.len()).map(|_| AtomicUsize::new(0)).collect(),
            ..Stats::default()
        };
        let never = || false;
        let shared = Shared {
            cancelled: AtomicBool::new(false),
            rows: AtomicUsize::new(0),
            token: &never,
        };
        let picked = first.len().min(SAMPLES);
        let (mut rows, mut sampled) = (Vec::new(), 0);
        for k in 0..picked {
            let value = [first[k * first.len() / picked]];
            sampled += 1;
            let done = self.extend_values(
                &order, 0, &value, &enforced, &mut work, &mut rows, &shared, &stats,
            );
            if done.is_err() || stats.lookups.load(Ordering::Relaxed) > SAMPLE_LOOKUPS {
                break;
            }
        }
        let found = rows.len() / self.variables.max(1);
        found as f64 * first.len() as f64 / sampled as f64
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
        let mut work = Work::new(self, order.len());
        let first = self.candidates(order[0], 0, &mut work);
        work.finish(stats);
        let Some((first, enforced)) = first else {
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
                let mut work = Work::new(self, order.len());
                let mut rows = Vec::new();
                let done = self.extend_values(
                    &order, 0, chunk, &enforced, &mut work, &mut rows, &shared, stats,
                );
                work.finish(stats);
                done.map(|()| rows)
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

/// A thread's state in a join: its bindings, and a probe cursor per depth and pattern,
/// kept across all the candidates it checks (each reads its runs forward from where the
/// last check of the same pattern at the same depth landed).
struct Work<'a> {
    bindings: Vec<Option<u64>>,
    /// Per depth, pattern and use: counting, listing and checking read different
    /// orders and ranges, each ascending in its own sequence.
    cursors: Vec<[Option<ProbeCursor<'a>>; 3]>,
    patterns: usize,
}

/// What a cursor of a [`Work`] is for.
#[derive(Clone, Copy)]
enum Use {
    Count = 0,
    Values = 1,
    Seek = 2,
}

impl<'a> Work<'a> {
    fn new(query: &Query<'a>, depths: usize) -> Self {
        Self {
            bindings: vec![None; query.variables],
            cursors: (0..depths * query.patterns.len())
                .map(|_| [None, None, None])
                .collect(),
            patterns: query.patterns.len(),
        }
    }

    fn cursor(
        &mut self,
        depth: usize,
        pattern: usize,
        what: Use,
        query: &Query<'a>,
    ) -> &mut ProbeCursor<'a> {
        self.cursors[depth * self.patterns + pattern][what as usize]
            .get_or_insert_with(|| query.snapshot.probe_cursor(query.model))
    }

    /// Adds the cursors' seeks to `stats`.
    fn finish(&self, stats: &Stats) {
        for cursor in self.cursors.iter().flatten().flatten() {
            let seeks = cursor.stats();
            stats
                .seeks
                .fetch_add(seeks.seeks as usize, Ordering::Relaxed);
            stats
                .roots
                .fetch_add(seeks.root_searches as usize, Ordering::Relaxed);
        }
    }
}

/// Whether `pattern` has a variable at two positions that `bindings` leave unbound: its
/// count then over-approximates (the positions must bind one term).
fn repeats_unbound(pattern: &[Pos; 3], bindings: &[Option<u64>]) -> bool {
    let unbound = |c: usize| match pattern[c] {
        Pos::Var(v) if bindings[v].is_none() => Some(v),
        _ => None,
    };
    [(0, 1), (0, 2), (1, 2)]
        .into_iter()
        .any(|(a, b)| unbound(a).is_some() && unbound(a) == unbound(b))
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
