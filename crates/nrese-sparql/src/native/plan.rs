//! Join ordering for BGPs (XC4): dynamic programming over left-deep orders, costed by
//! estimated intermediate sizes (C_out) plus pattern access.
//!
//! Inputs are exact per-pattern counts and, per variable, the number of distinct values it
//! takes in the pattern (engine statistics). A join on shared variables is estimated under
//! independence: `|R ⋈ P| = |R| · |P| / Π max(d_R(v), d_P(v))`. Accessing a pattern costs
//! its size when scanned, or about one index probe per row of the running result when that
//! result is much smaller (the executor's rule). BGPs with more than [`DP_PATTERNS`]
//! patterns are ordered greedily by the same model.

/// Largest BGP ordered exhaustively (2^n subsets).
const DP_PATTERNS: usize = 12;

/// Cost of one index probe relative to reading one scanned row.
const PROBE_COST: f64 = 4.0;

/// One triple pattern for planning.
pub(super) struct Input {
    /// Exact number of matches.
    pub count: u64,
    /// Per variable (index into the BGP's variables): its distinct values among the matches.
    pub vars: Vec<(usize, u64)>,
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
pub(super) fn order(inputs: &[Input], variables: usize, probe_factor: u64) -> Plan {
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
        let mut rows = state.rows * count;
        let mut shared = false;
        for &(v, d) in &input.vars {
            if state.distinct[v] > 0.0 {
                shared = true;
                rows /= state.distinct[v].max(d as f64).max(1.0);
            }
        }
        let probe = shared && state.rows * (probe_factor as f64) < count;
        let access = if probe {
            state.rows * PROBE_COST
        } else {
            count
        };
        let mut distinct = state.distinct.clone();
        for &(v, d) in &input.vars {
            let d = (d as f64).max(1.0);
            distinct[v] = if distinct[v] > 0.0 {
                distinct[v].min(d)
            } else {
                d
            };
        }
        for d in &mut distinct {
            if *d > 0.0 {
                *d = d.min(rows.max(1.0));
            }
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
    let candidates = |state: &State, used: u64| -> Vec<usize> {
        let free: Vec<usize> = (0..inputs.len())
            .filter(|&j| used & (1 << j) == 0)
            .collect();
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
    if n > DP_PATTERNS {
        // Greedy: the cheapest start, then the cheapest extension each time.
        let mut best: Option<State> = None;
        for j in 0..n {
            let mut state = start(j);
            let mut used = 1u64 << j;
            while state.order.len() < n {
                let next = candidates(&state, used)
                    .into_iter()
                    .map(|j| extend(&state, j))
                    .reduce(|a, b| if b.better_than(&a) { b } else { a })
                    .expect("a pattern is left");
                used |= 1 << next.order[next.order.len() - 1];
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
        for j in candidates(&state, used) {
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
        let plan = order(&inputs, 2, 32);
        assert_eq!(plan.order, vec![0, 2, 1]);
        assert_eq!(plan.rows.len(), 3);
    }

    #[test]
    fn disconnected_patterns_still_get_an_order() {
        let inputs = [input(10, &[(0, 10)]), input(3, &[(1, 3)])];
        let plan = order(&inputs, 2, 32);
        assert_eq!(plan.order.len(), 2);
        assert!((plan.rows[1] - 30.0).abs() < 1e-9);
    }

    #[test]
    fn large_bgps_are_ordered_greedily() {
        let inputs: Vec<Input> = (0..14)
            .map(|i| input(100 + i, &[(0, 100), (i as usize + 1, 50)]))
            .collect();
        let plan = order(&inputs, 15, 32);
        let mut sorted = plan.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..14).collect::<Vec<_>>());
    }
}
