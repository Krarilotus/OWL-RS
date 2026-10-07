//! Join ordering for BGPs (XC4): dynamic programming over left-deep orders, costed by
//! estimated intermediate sizes (C_out) plus pattern access.
//!
//! Inputs are exact per-pattern counts and, per variable, the number of distinct values it
//! takes in the pattern (engine statistics). A join on shared variables is estimated under
//! independence: `|R ⋈ P| = |R| · |P| / Π max(d_R(v), d_P(v))`. Accessing a pattern costs
//! its size when scanned, or about one index probe per row of the running result when that
//! result is much smaller (the executor's rule). BGPs with more than [`DP_PATTERNS`]
//! patterns are ordered greedily by the same model, from every start, or from the
//! [`GREEDY_STARTS`] cheapest starts beyond [`GREEDY_ALL_STARTS`] patterns (each start
//! costs n² extensions).
//!
//! Stars are estimated from the default graph's characteristic sets where they are known
//! (`nrese_engine::engine::characteristic`): a pattern `?s p ?o` joined to patterns that
//! already constrain `?s` by their predicates `P` multiplies the rows by
//! `star(P ∪ {p}) / star(P)`, the statements of `p` per subject among the subjects that
//! have `P`, instead of assuming every subject might have `p`.
//!
//! Chains are estimated from characteristic pairs where they are known: a star pattern on
//! `?y` joined after an edge `?x p ?y` multiplies the rows by
//! `pair(X, p, Y ∪ {q}) / pair(X, p, Y)` (`X`, `Y` the predicates the state already puts
//! on `?x` and `?y`): the share of `p`'s links from such subjects whose object has `q`,
//! instead of assuming every object of `p` might.
//!
//! Bounds (degree constraints, as in AGM/PANDA): a pattern whose variables are all bound
//! keeps at most the rows it checks, and a step estimated above 0 is at least one row. Patterns with a constant object are estimated by containment, not by the
//! characteristic sets, which know no objects; two single-variable patterns on one
//! variable by the values they share, counted where both are small. A variable's
//! distinct values are the fewest any pattern binding it has (its domain), not cut to
//! the running rows. Exponential backoff over several variables' divisors (SQL Server's
//! rule) was measured and rejected: LUBM-100 p90 27.8 against 1.24 for their product.

use nrese_engine::engine::characteristic::CharacteristicSets;

/// Largest BGP ordered exhaustively (2^n subsets).
const DP_PATTERNS: usize = 12;

/// Largest BGP whose greedy order is tried from every pattern.
const GREEDY_ALL_STARTS: usize = 64;

/// Starts tried, the cheapest first, for BGPs larger than [`GREEDY_ALL_STARTS`].
const GREEDY_STARTS: usize = 16;

/// Cost of one index probe relative to reading one scanned row.
const PROBE_COST: f64 = 4.0;

/// One triple pattern for planning.
pub(super) struct Input {
    /// Exact number of matches.
    pub count: u64,
    /// Per variable (index into the BGP's variables): its distinct values among the matches.
    pub vars: Vec<(usize, u64)>,
    /// The pattern as part of a star, if it is one: a variable subject and a constant
    /// predicate, in the default graph.
    pub star: Option<Star>,
    /// The pattern as an edge `?x p ?y` between two variables, if it is one (a star
    /// pattern with a variable object): the object variable.
    pub edge_object: Option<usize>,
    /// For a pattern with one variable: per other such pattern on the same variable (its
    /// index), the values both have, counted exactly where both lists are small
    /// ([`OVERLAP_VALUES`]). Statistics know predicates, not objects: `?y a Department`
    /// and `?y subOrganizationOf <U0>` (LUBM-10: 189 and 239 values) share 15.
    pub overlaps: Vec<(usize, u64)>,
}

/// The largest sum of two single-variable patterns' matches whose common values the
/// planner counts by reading both sorted lists (microseconds for a few thousand ids).
pub(super) const OVERLAP_VALUES: u64 = 8192;

/// A pattern `?s p o` of a star on `?s`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Star {
    /// The subject variable (index into the BGP's variables).
    pub subject: usize,
    /// The predicate (raw id).
    pub predicate: u64,
    /// The share of `p`'s statements the pattern's object keeps (1 for a variable).
    pub selectivity: f64,
}

/// A join order with the estimated rows after each step.
#[derive(Debug, Clone)]
pub(super) struct Plan {
    pub order: Vec<usize>,
    pub rows: Vec<f64>,
}

#[derive(Clone)]
struct State {
    cost: f64,
    rows: f64,
    /// Distinct values per variable; 0 for unbound ones.
    distinct: Vec<f64>,
    order: Vec<usize>,
    estimates: Vec<f64>,
}

impl State {
    /// Cheaper, or as cheap and starting with fewer rows.
    fn better_than(&self, other: &State) -> bool {
        (self.cost, self.estimates[0]) < (other.cost, other.estimates[0])
    }
}

/// The cheapest order for `inputs`; `probe_factor` is the executor's threshold for index
/// nested loops (the pattern is at least this many times larger than the running result).
pub(super) fn order(
    inputs: &[Input],
    variables: usize,
    probe_factor: u64,
    sets: Option<&CharacteristicSets>,
) -> Plan {
    search(inputs, variables, probe_factor, sets, None)
}

/// The estimates of joining `inputs` in the given `order` (all of them, each once), by the
/// same model as [`order`]: for BGPs whose order the executor fixes itself.
pub(super) fn along(
    inputs: &[Input],
    variables: usize,
    probe_factor: u64,
    sets: Option<&CharacteristicSets>,
    order: &[usize],
) -> Plan {
    search(inputs, variables, probe_factor, sets, Some(order))
}

fn search(
    inputs: &[Input],
    variables: usize,
    probe_factor: u64,
    sets: Option<&CharacteristicSets>,
    fixed: Option<&[usize]>,
) -> Plan {
    let start = |j: usize| {
        let input = &inputs[j];
        let rows = input.count as f64;
        let mut distinct = vec![0.0; variables];
        for &(v, d) in &input.vars {
            distinct[v] = (d as f64).clamp(1.0, rows.max(1.0));
        }
        State {
            cost: rows,
            rows,
            distinct,
            order: vec![j],
            estimates: vec![rows],
        }
    };
    let extend = |state: &State, j: usize| -> State {
        let input = &inputs[j];
        let count = input.count as f64;
        // The characteristic sets know which predicates subjects have, not their objects.
        // A constant object (`?x a C`) is estimated by containment below, not as a share
        // of the predicate's statements independent of the subject's other predicates
        // (on materialised data a type follows from them: LUBM-10 q4 estimated 2 for
        // 34), and doesn't stand for "has the predicate" in a later star either: its
        // subjects have it, but read so (any subject with a type) LUBM-10's p90 was 30.
        let open = |star: &Star| star.selectivity >= 1.0;
        let star_input = input.star.filter(open);
        // The predicates of the state's stars on `subject` (all but pattern `skip`).
        let on = |subject: usize, skip: Option<usize>| -> Vec<u64> {
            state
                .order
                .iter()
                .filter(|&&i| Some(i) != skip)
                .filter_map(|&i| inputs[i].star)
                .filter(|s| s.subject == subject && open(s))
                .map(|s| s.predicate)
                .collect()
        };
        // A star pattern on `?y` after an edge `?x p ?y`: its rows from the
        // characteristic pairs.
        let chained = sets.zip(star_input).and_then(|(sets, star)| {
            if state.distinct[star.subject] == 0.0 || !sets.has_pairs() {
                return None;
            }
            let edge = state.order.iter().copied().find(|&i| {
                inputs[i].edge_object == Some(star.subject)
                    && inputs[i].star.is_some_and(|e| e.subject != star.subject)
            })?;
            let edge_star = inputs[edge].star?;
            let from = on(edge_star.subject, Some(edge));
            let mut to = on(star.subject, None);
            let before = sets.pair(&from, edge_star.predicate, &to)?;
            to.push(star.predicate);
            let after = sets.pair(&from, edge_star.predicate, &to)?;
            let factor = if before > 0.0 { after / before } else { 0.0 };
            Some((star.subject, factor * star.selectivity))
        });
        // The other way round, an edge `?x p ?y` after a star on `?y` (the smaller
        // pattern first): the pairs' links into such objects over the star's rows.
        let reversed = || {
            let (sets, edge) = sets.zip(star_input)?;
            let object = input.edge_object?;
            if !sets.has_pairs()
                || state.distinct[object] == 0.0
                || state.distinct[edge.subject] > 0.0
            {
                return None;
            }
            let to = on(object, None);
            if to.is_empty() {
                return None;
            }
            let before = sets.star(&to);
            let after = sets.pair(&[], edge.predicate, &to)?;
            let factor = if before > 0.0 { after / before } else { 0.0 };
            Some((object, factor))
        };
        // A star extended: its rows from the characteristic sets.
        let star = chained.or_else(reversed).or_else(|| {
            sets.zip(star_input).and_then(|(sets, star)| {
                if state.distinct[star.subject] == 0.0 {
                    return None;
                }
                let mut predicates: Vec<u64> = state
                    .order
                    .iter()
                    .filter_map(|&i| inputs[i].star)
                    .filter(|s| s.subject == star.subject && open(s))
                    .map(|s| s.predicate)
                    .collect();
                if predicates.is_empty() {
                    return None;
                }
                let before = sets.star(&predicates);
                predicates.push(star.predicate);
                let after = sets.star(&predicates);
                let factor = if before > 0.0 { after / before } else { 0.0 };
                Some((star.subject, factor * star.selectivity))
            })
        });
        let mut rows = match star {
            Some((_, factor)) => state.rows * factor,
            None => state.rows * count,
        };
        let mut shared = false;
        let mut check = true;
        for &(v, d) in &input.vars {
            if state.distinct[v] > 0.0 {
                shared = true;
                if star.is_none_or(|(subject, _)| subject != v) {
                    rows /= state.distinct[v].max(d as f64).max(1.0);
                }
            } else {
                check = false;
            }
        }
        // Degree bounds: a pattern whose variables are all bound checks each row and
        // keeps at most the rows it is given. A step estimated above 0 is at least one
        // row: fractions rounded to 0 hid joins of thousands of rows (an exact 0 from the
        // characteristic sets or the shared values stays).
        if check {
            rows = rows.min(state.rows);
            // A single-variable pattern whose common values with one already joined are
            // counted keeps the share of that pattern's values it has too.
            let kept = input
                .overlaps
                .iter()
                .filter(|(i, _)| state.order.contains(i))
                .map(|&(i, both)| both as f64 / (inputs[i].count as f64).max(1.0))
                .reduce(f64::min);
            if let Some(kept) = kept {
                rows = state.rows * kept.min(1.0);
            }
        }
        if rows > 0.0 {
            rows = rows.max(1.0);
        }
        let probe = shared && state.rows * (probe_factor as f64) < count;
        let access = if probe {
            state.rows * PROBE_COST
        } else {
            count
        };
        // A variable's distinct values: the fewest any pattern binding it has, the domain
        // its values are drawn from. Not cut to the rows: 543 `worksFor` rows hold
        // values from its 7 k subjects, of which `?x a Chair`'s 189 keep a share, not all.
        let mut distinct = state.distinct.clone();
        for &(v, d) in &input.vars {
            let d = (d as f64).max(1.0);
            distinct[v] = if distinct[v] > 0.0 {
                distinct[v].min(d)
            } else {
                d
            };
        }
        let mut order = state.order.clone();
        order.push(j);
        let mut estimates = state.estimates.clone();
        estimates.push(rows);
        State {
            cost: state.cost + access + rows,
            rows,
            distinct,
            order,
            estimates,
        }
    };
    // Patterns sharing a bound variable with `state`; all others if none does (a cross
    // product is then unavoidable).
    // `used(j)`: whether pattern j is in `state` already (a bit mask for the exhaustive
    // search, a vector for the greedy one, which has no limit on the patterns).
    let candidates = |state: &State, used: &dyn Fn(usize) -> bool| -> Vec<usize> {
        let free: Vec<usize> = (0..inputs.len()).filter(|&j| !used(j)).collect();
        let connected: Vec<usize> = free
            .iter()
            .copied()
            .filter(|&j| inputs[j].vars.iter().any(|&(v, _)| state.distinct[v] > 0.0))
            .collect();
        if connected.is_empty() {
            free
        } else {
            connected
        }
    };
    let n = inputs.len();
    let finish = |state: State| Plan {
        order: state.order,
        rows: state.estimates,
    };
    if n == 0 {
        return Plan {
            order: Vec::new(),
            rows: Vec::new(),
        };
    }
    if let Some(order) = fixed {
        let mut state = start(order[0]);
        for &j in &order[1..] {
            state = extend(&state, j);
        }
        return finish(state);
    }
    if n > DP_PATTERNS {
        // Greedy: the cheapest start, then the cheapest extension each time.
        let mut starts: Vec<State> = (0..n).map(start).collect();
        if n > GREEDY_ALL_STARTS {
            starts.sort_by(|a, b| a.cost.total_cmp(&b.cost).then(a.order.cmp(&b.order)));
            starts.truncate(GREEDY_STARTS);
        }
        let mut best: Option<State> = None;
        for mut state in starts {
            let mut used = vec![false; n];
            used[state.order[0]] = true;
            while state.order.len() < n {
                let next = candidates(&state, &|j| used[j])
                    .into_iter()
                    .map(|j| extend(&state, j))
                    .reduce(|a, b| if b.better_than(&a) { b } else { a })
                    .expect("a pattern is left");
                used[next.order[next.order.len() - 1]] = true;
                state = next;
            }
            if best.as_ref().is_none_or(|b| state.better_than(b)) {
                best = Some(state);
            }
        }
        return finish(best.expect("inputs are not empty"));
    }
    let mut best: Vec<Option<State>> = vec![None; 1 << n];
    for j in 0..n {
        best[1 << j] = Some(start(j));
    }
    for used in 1u64..(1 << n) {
        let Some(state) = best[used as usize].take() else {
            continue;
        };
        for j in candidates(&state, &|j| used & (1 << j) != 0) {
            let next = extend(&state, j);
            let slot = &mut best[(used | (1 << j)) as usize];
            if slot.as_ref().is_none_or(|s| next.better_than(s)) {
                *slot = Some(next);
            }
        }
        best[used as usize] = Some(state);
    }
    finish(
        best[(1 << n) - 1]
            .take()
            .expect("every pattern is reachable"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(count: u64, vars: &[(usize, u64)]) -> Input {
        Input {
            count,
            vars: vars.to_vec(),
            star: None,
            edge_object: None,
            overlaps: Vec::new(),
        }
    }

    #[test]
    fn a_selective_filter_joins_before_a_fan_out() {
        // ?x memberOf <d> (50) . ?x takesCourse ?c (300k, 3 per x) . ?x a Grad (5k)
        let inputs = [
            input(50, &[(0, 50)]),
            input(300_000, &[(0, 100_000), (1, 2_000)]),
            input(5_000, &[(0, 5_000)]),
        ];
        let plan = order(&inputs, 2, 32, None);
        assert_eq!(plan.order, vec![0, 2, 1]);
        assert_eq!(plan.rows.len(), 3);
    }

    #[test]
    fn checks_keep_at_most_their_input_and_steps_at_least_a_row() {
        // ?x p <c> (40) . ?x a C (100k, every x distinct): a check of 40 rows.
        let inputs = [input(40, &[(0, 40)]), input(100_000, &[(0, 100_000)])];
        let plan = along(&inputs, 1, 32, None, &[0, 1]);
        assert!(plan.rows[1] <= 40.0, "{:?}", plan.rows);
        // ?x p ?y (10) . ?y q ?z (5, 5 distinct ?y among 1M): 0.00005 rows, shown as one.
        let inputs = [
            input(10, &[(0, 10), (1, 10)]),
            input(5, &[(1, 1_000_000), (2, 5)]),
        ];
        let plan = along(&inputs, 3, 32, None, &[0, 1]);
        assert_eq!(plan.rows[1], 1.0);
        // Two single-variable patterns sharing 15 of the first's 189 values.
        let mut inputs = [input(189, &[(0, 189)]), input(239, &[(0, 239)])];
        inputs[1].overlaps = vec![(0, 15)];
        let plan = along(&inputs, 1, 32, None, &[0, 1]);
        assert!((plan.rows[1] - 15.0).abs() < 1e-9, "{:?}", plan.rows);
    }

    #[test]
    fn disconnected_patterns_still_get_an_order() {
        let inputs = [input(10, &[(0, 10)]), input(3, &[(1, 3)])];
        let plan = order(&inputs, 2, 32, None);
        assert_eq!(plan.order.len(), 2);
        assert!((plan.rows[1] - 30.0).abs() < 1e-9);
    }

    /// BGPs too large for the exact order get a greedy one: a star of 14 patterns, and
    /// chains beyond 64 (a 64-bit membership mask overflowed at 65 patterns: the review
    /// of 3 October 2026, P2), up to 200 patterns over 201 variables.
    #[test]
    fn large_bgps_are_ordered_greedily() {
        let inputs: Vec<Input> = (0..14)
            .map(|i| input(100 + i, &[(0, 100), (i as usize + 1, 50)]))
            .collect();
        let plan = order(&inputs, 15, 32, None);
        let mut sorted = plan.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..14).collect::<Vec<_>>());
        for n in [64usize, 65, 200] {
            let inputs: Vec<Input> = (0..n)
                .map(|i| input(1_000 + i as u64, &[(i, 500), (i + 1, 500)]))
                .collect();
            let plan = order(&inputs, n + 1, 32, None);
            let mut sorted = plan.order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..n).collect::<Vec<_>>());
            assert_eq!(plan.rows.len(), n);
        }
    }
}
