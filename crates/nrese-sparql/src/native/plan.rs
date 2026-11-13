//! Join ordering for BGPs (XC4): dynamic programming over left-deep orders, costed by
//! estimated intermediate sizes (C_out) plus pattern access.
//!
//! Inputs are exact per-pattern counts and, per variable, the number of distinct values it
//! takes in the pattern (engine statistics). A join on shared variables is estimated under
//! independence: `|R ⋈ P| = |R| · |P| / Π max(d_R(v), d_P(v))`. Accessing a pattern costs
//! its size when scanned, or about one index probe per row of the running result when that
//! result is much smaller (the executor's rule). BGPs with more than [`DP_PATTERNS`]
//! patterns are ordered greedily by the same model, from every start, or from the
//! [`GREEDY_STARTS`] cheapest starts beyond [`GREEDY_ALL_STARTS`] patterns, and fewer where
//! their estimates would exceed [`GREEDY_EFFORT`] (each start estimates up to n²/2
//! extensions from the state as it is, and builds only the n it takes). The search asks
//! for the query's cancellation at every step.
//!
//! Stars are estimated from the default graph's characteristic sets where they are known
//! (`nrese_engine::engine::characteristic`): a pattern `?s p ?o` joined to patterns that
//! already constrain `?s` by their predicates `P` multiplies the rows by
//! `star(P ∪ {p}) / star(P)`, the statements of `p` per subject among the subjects that
//! have `P`, instead of assuming every subject might have `p`. The state keeps each star's
//! matching sets with their rows (`StarRows`), so that `star(P ∪ {p})` reads each of them
//! once, not every predicate of `P` again.
//!
//! Chains are estimated from characteristic pairs where they are known: a star pattern on
//! `?y` joined after an edge `?x p ?y` multiplies the rows by
//! `pair(X, p, Y ∪ {q}) / pair(X, p, Y)` (`X`, `Y` the predicates the state already puts
//! on `?x` and `?y`): the share of `p`'s links from such subjects whose object has `q`,
//! instead of assuming every object of `p` might.
//!
//! Both read whole lists of sets or links per estimate, so one planning's reads and copies of
//! them are counted, each before it is made, against a budget ([`budget_for`]: [`SET_BUDGET`],
//! or more for a BGP whose plain plan expects much work). They are read at all only where
//! the reads the search makes at its starts are few, or that plain plan expects much work
//! ([`admits`]). A planning that would pass its budget stops ([`Stopped::Budget`]) and the
//! BGP keeps the plain plan; one within it plans as it would without the bound. Where the
//! reads every start makes would pass it on their own, the search stops before making any of
//! them.
//!
//! Bounds (degree constraints, as in AGM/PANDA): a pattern whose variables are all bound
//! keeps at most the rows it checks, and a step estimated above 0 is at least one row. Patterns with a constant object are estimated by containment, not by the
//! characteristic sets, which know no objects; two single-variable patterns on one
//! variable by the values they share, counted where both are small. A variable's
//! distinct values are the fewest any pattern binding it has (its domain), not cut to
//! the running rows. Exponential backoff over several variables' divisors (SQL Server's
//! rule) was measured and rejected: LUBM-100 p90 27.8 against 1.24 for their product.

use std::cell::Cell;
use std::collections::HashMap;

use nrese_engine::engine::characteristic::{CharacteristicSets, StarRows};

/// Largest BGP ordered exhaustively (2^n subsets).
const DP_PATTERNS: usize = 12;

/// Largest BGP whose greedy order is tried from every pattern.
const GREEDY_ALL_STARTS: usize = 64;

/// Starts tried, the cheapest first, for BGPs larger than [`GREEDY_ALL_STARTS`].
const GREEDY_STARTS: usize = 16;

/// The candidates the greedy search estimates over all its starts, about: a start estimates
/// up to n²/2, so beyond about 250 patterns fewer starts are tried, down to one. A star of
/// 1,000 patterns took 16 starts of 500,000 estimates each, twice per query (4.9 s in the
/// test profile, 12 October 2026).
const GREEDY_EFFORT: usize = 1 << 19;

/// The characteristic-set work one planning may do, in entries: the sets its star estimates
/// read (a predicate's sets for a star's first pattern, then the sets the star still
/// matches), the links its chain estimates read times the predicates looked up per link, and
/// the sets the exhaustive search copies with its states. Each estimate reads whole lists, so
/// without a bound a planning grows with the sets per predicate: on WatDiv-10m, with tens of
/// thousands of sets per predicate, these reads were 93 % of F2-4's 133 M instructions per
/// query (12 November 2026). 2^16 entries are about 0.4 ms of lookups, scaled from the
/// 0.1 ms estimated for 2^14, not measured. On this count, the
/// join-order identity fixtures do up to 35,860 (the 12-pattern star) and the fast
/// catalogue's plans up to 348, so both keep their plans. Like the planner's other limits ([`GREEDY_EFFORT`],
/// [`OVERLAP_VALUES`], `MAX_SETS`), it is a constant, not a setting with an "unlimited" value
/// as AGENTS.md asks of every budget; one setting for all four is a follow-up.
pub(super) const SET_BUDGET: usize = 1 << 16;

/// The rows a BGP's plain plan (planned without the characteristic sets) must expect for the
/// sets to be read where its predicates are in many ([`admits`]). Planning effort follows the
/// work the query expects: a set search on WatDiv's long lists costs about 0.5 ms whether or
/// not it pays, so only a query that expects millions of rows can repay it. On WatDiv
/// (12 November 2026), the one query whose set plan beats main's, watdiv-100m C3-1 (106 ms
/// against 394), expects 3.1 M rows; the others whose set plan differs from main's expect at
/// most 210 k (S2-8, slower with its sets: 5.04 ms against 4.19 without), and every F, S and
/// L query at most 210 k. Between them, watdiv-100m C1-1 expects 1.53 M and keeps main's plan
/// whatever its budget, after 7.5 ms of set search at half an entry per row: the threshold
/// sits between it and C3-1, two points only. A tuning signal, not a bound on execution; like
/// [`SET_BUDGET`], a constant.
pub(super) const SET_GATE_ROWS: f64 = 2_000_000.0;

/// Set entries any planning may read at its starts, whatever it expects ([`admits`]): where
/// its predicates are in few sets, the sets are read as before the gate. About 7 µs of
/// lookups. The join-order identity fixtures' graphs are in few sets, so they keep their plans;
/// WatDiv's S1 reads 98 and keeps the plan its sets give; WatDiv's other queries read 2,625 to
/// 96,518 (12 November 2026). It bounds the starts only: a search it admits may read long
/// lists after them (a chain's links), up to its budget, [`SET_BUDGET`] for a query that
/// expects little, before the BGP keeps its plain plan. A constant, as [`SET_BUDGET`] is.
pub(super) const SET_START_ALLOWANCE: usize = 1 << 10;

/// Set entries a planning may spend per row its plain plan expects, above [`SET_BUDGET`]
/// ([`budget_for`]): a query that expects much work may plan longer. watdiv-100m C3-1's set
/// search reads 1.16 M entries and its plain plan expects 3.12 M rows (12 November 2026).
/// Half an entry is about 35 instructions per expected row. A tuning signal, a constant.
pub(super) const SET_ENTRIES_PER_ROW: f64 = 0.5;

#[cfg(test)]
thread_local! {
    /// A test's budget for every planning, in place of [`budget_for`]'s: one that passes it on
    /// a small graph, or `usize::MAX` to plan without it. `None` for the planner's own.
    pub(super) static BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
    /// [`budget_for`]'s least budget, [`SET_BUDGET`] unless a test lowers it to its graph.
    pub(super) static FLOOR: Cell<usize> = const { Cell::new(SET_BUDGET) };
    /// A test's decision for [`admits`], in place of the planner's: `None` for the planner's
    /// own, which every other test plans with.
    pub(super) static ADMIT: Cell<Option<bool>> = const { Cell::new(None) };
}

/// The work a plan expects: the rows its steps produce, summed. An estimate that is no number
/// (a two-pattern plan's join has none) is left out; one that overflowed to infinity makes the
/// sum infinite.
pub(super) fn expected_work(plan: &Plan) -> f64 {
    plan.rows.iter().filter(|rows| !rows.is_nan()).sum()
}

/// The open stars whose predicates' sets a search reads at its starts, in input order: every
/// input's, or only `first`'s for a fixed order that starts there. A start reads only where
/// another input follows it, as [`search`]'s steps decide.
fn start_stars(inputs: &[Input], first: Option<usize>) -> impl Iterator<Item = &Star> {
    let n = inputs.len();
    inputs
        .iter()
        .enumerate()
        .filter(move |&(j, _)| n >= 2 && first.is_none_or(|f| f == j))
        .filter_map(|(_, input)| input.star.as_ref())
        .filter(|star| star.open())
}

/// The sets a search reads at its starts ([`start_stars`]: all of them, or a fixed order's
/// `first`), by the sets' index, saturating.
pub(super) fn start_reads(
    inputs: &[Input],
    first: Option<usize>,
    sets: &CharacteristicSets,
) -> usize {
    start_stars(inputs, first).fold(0usize, |reads, star| {
        reads.saturating_add(sets.sets_having(star.predicate))
    })
}

/// Whether a BGP whose plain plan expects `expected` rows, and whose set search would read
/// `reads` entries at its starts, is planned with the sets too: where those reads are within
/// [`SET_START_ALLOWANCE`], or it expects at least [`SET_GATE_ROWS`] rows (NaN doesn't). A
/// test's decision ([`ADMIT`]) instead.
pub(super) fn admits(expected: f64, reads: usize) -> bool {
    #[cfg(test)]
    if let Some(admit) = ADMIT.with(Cell::get) {
        return admit;
    }
    reads <= SET_START_ALLOWANCE || expected >= SET_GATE_ROWS
}

/// [`SET_ENTRIES_PER_ROW`] per expected row, in entries: a float cast saturates, NaN to 0 and
/// beyond the range (infinity included) to `usize::MAX`.
fn per_row(expected: f64) -> usize {
    (expected * SET_ENTRIES_PER_ROW) as usize
}

/// The characteristic-set work the planning of a BGP whose plain plan expects `expected` rows
/// may do: [`SET_BUDGET`], or [`SET_ENTRIES_PER_ROW`] per expected row where that is more. A
/// test's ([`BUDGET`]) instead.
pub(super) fn budget_for(expected: f64) -> usize {
    #[cfg(test)]
    if let Some(budget) = BUDGET.with(Cell::get) {
        return budget;
    }
    #[cfg(test)]
    let floor = FLOOR.with(Cell::get);
    #[cfg(not(test))]
    let floor = SET_BUDGET;
    floor.max(per_row(expected))
}

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
    /// and `?y subOrganizationOf <U0>` (LUBM-10: 189 and 239 values) share 15. Symmetric:
    /// each pattern of a pair lists the other.
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

impl Star {
    /// Whether the characteristic sets estimate the pattern: its object keeps all of `p`'s
    /// statements (a variable). The sets know which predicates subjects have, not their
    /// objects. A constant object (`?x a C`) is estimated by containment, not as a share of
    /// the predicate's statements independent of the subject's other predicates (on
    /// materialised data a type follows from them: LUBM-10 q4 estimated 2 for 34), and
    /// doesn't stand for "has the predicate" in a later star either: its subjects have it,
    /// but read so (any subject with a type) LUBM-10's p90 was 30.
    pub(super) fn open(&self) -> bool {
        self.selectivity >= 1.0
    }
}

/// A join order with the estimated rows after each step.
#[derive(Debug, Clone)]
pub(super) struct Plan {
    pub order: Vec<usize>,
    pub rows: Vec<f64>,
    /// The characteristic-set work its planning counted ([`SET_BUDGET`]): all of it for a plan
    /// made within the budget; for one planned again without the sets, what was done before
    /// the next read or copy would have passed it.
    pub(super) work: usize,
}

/// Why the orderer gave no plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stopped {
    /// The query was cancelled (`stop`): it fails, and isn't planned again.
    Cancelled,
    /// The next read or copy of the characteristic sets would have passed the budget
    /// ([`SET_BUDGET`]), after `work` of it: planned again without the sets. The search asks
    /// for the cancellation at its steps only, so its caller asks before planning again.
    Budget { work: usize },
}

/// One planning's characteristic-set work, each read or copy counted before it is made. Once
/// one would pass the budget, none is made any more, and the search stops at its next check.
struct Work {
    done: Cell<usize>,
    budget: usize,
    passed: Cell<bool>,
}

impl Work {
    /// Whether `n` more is within the budget; counted if it is.
    fn take(&self, n: usize) -> bool {
        let done = self.done.get().saturating_add(n);
        if self.passed.get() || done > self.budget {
            self.passed.set(true);
            return false;
        }
        self.done.set(done);
        true
    }

    /// `n` more, before it is copied: counted, or the search's stop.
    fn charge(&self, n: usize) -> Result<(), Stopped> {
        match self.take(n) {
            true => Ok(()),
            false => self.check(),
        }
    }

    /// The search's stop once a read or copy would have passed the budget.
    fn check(&self) -> Result<(), Stopped> {
        match self.passed.get() {
            true => Err(Stopped::Budget {
                work: self.done.get(),
            }),
            false => Ok(()),
        }
    }
}

#[derive(Clone)]
struct State {
    cost: f64,
    rows: f64,
    /// Distinct values per variable; 0 for unbound ones.
    distinct: Vec<f64>,
    order: Vec<usize>,
    estimates: Vec<f64>,
    /// Per subject variable of the state's open star patterns: their predicates and the
    /// star's rows so far, extended as patterns join (one read of each matching set per step,
    /// not the star from scratch per candidate). Empty in a search without the sets.
    stars: HashMap<usize, Subject>,
    /// Per variable: the first edge `?x p ?y` in the order whose object it is. Empty in a
    /// search without the sets.
    edges_into: HashMap<usize, usize>,
    /// Per pattern with counted common values ([`Input::overlaps`]): the least share of
    /// a joined partner's values it has, kept up to date as partners join (a lookup per
    /// candidate, not a search of the order per partner).
    kept: HashMap<usize, f64>,
}

#[derive(Clone, Default)]
struct Subject {
    predicates: Vec<u64>,
    rows: Option<StarRows>,
}

impl State {
    /// Cheaper, or as cheap and starting with fewer rows.
    fn better_than(&self, other: &State) -> bool {
        Self::beats(self.cost, self.estimates[0], other)
    }

    /// Whether a state that costs `cost` and starts with `first` rows is
    /// [`Self::better_than`] `other`: a step's state decided before it is built.
    fn beats(cost: f64, first: f64, other: &State) -> bool {
        (cost, first) < (other.cost, other.estimates[0])
    }

    /// The set entries a copy of the state copies: its stars' sets ([`StarRows::len`]).
    fn copied(&self) -> usize {
        self.stars
            .values()
            .filter_map(|s| s.rows.as_ref())
            .map(StarRows::len)
            .sum()
    }

    /// The predicates of the state's open stars on `subject`, but for pattern `skip`'s.
    fn on(&self, subject: usize, skip: Option<&Star>) -> Vec<u64> {
        let mut predicates = self
            .stars
            .get(&subject)
            .map_or_else(Vec::new, |s| s.predicates.clone());
        if let Some(skip) = skip.filter(|s| s.subject == subject)
            && let Some(at) = predicates.iter().position(|&p| p == skip.predicate)
        {
            predicates.remove(at);
        }
        predicates
    }
}

/// A pattern's step after a state: the rows after it and the cost so far.
#[derive(Clone, Copy)]
struct Step {
    rows: f64,
    cost: f64,
}

#[cfg(test)]
thread_local! {
    /// Characteristic-set entries the orderer read (`StarRows::len` per step), and the steps
    /// it estimated: the guard that a step's statistics don't grow with the star.
    pub(super) static STATISTICS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
thread_local! {
    /// Run at every step the orderer asks whether to stop, before it asks: a test's measure
    /// of planning work, and its way to cancel a query after a chosen amount of it.
    pub(super) static STEP: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Star predicates and edges the orderer's states took on: the guard that a search
    /// without the characteristic sets keeps none.
    pub(super) static STAR_STATE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn note_star_state() {
    #[cfg(test)]
    STAR_STATE.with(|c| c.set(c.get() + 1));
}

fn note_statistics(_reads: usize, _steps: usize) {
    #[cfg(test)]
    STATISTICS.with(|c| {
        let (reads, steps) = c.get();
        c.set((reads + _reads, steps + _steps));
    });
}

/// The cheapest order for `inputs`; `probe_factor` is the executor's threshold for index
/// nested loops (the pattern is at least this many times larger than the running result).
/// Stopped once `stop` says so (the query's cancellation, asked at every step), or where the
/// characteristic sets would pass `budget` ([`budget_for`]).
pub(super) fn order(
    inputs: &[Input],
    variables: usize,
    probe_factor: u64,
    sets: Option<&CharacteristicSets>,
    budget: usize,
    stop: &dyn Fn() -> bool,
) -> Result<Plan, Stopped> {
    #[cfg(test)]
    let stop = &|| {
        STEP.with(|hook| {
            if let Some(hook) = &*hook.borrow() {
                hook();
            }
        });
        stop()
    };
    search(inputs, variables, probe_factor, sets, budget, None, stop)
}

/// The estimates of joining `inputs` in the given `order` (all of them, each once), by the
/// same model as [`order`]: for BGPs whose order the executor fixes itself. Stopped only
/// where the characteristic sets would pass `budget` ([`budget_for`]).
pub(super) fn along(
    inputs: &[Input],
    variables: usize,
    probe_factor: u64,
    sets: Option<&CharacteristicSets>,
    budget: usize,
    order: &[usize],
) -> Result<Plan, Stopped> {
    search(
        inputs,
        variables,
        probe_factor,
        sets,
        budget,
        Some(order),
        &|| false,
    )
}

/// O(starts · n² · s) for n patterns and s characteristic sets a star still matches: each
/// step estimates every candidate from the state as it is, and builds only the chosen one.
fn search(
    inputs: &[Input],
    variables: usize,
    probe_factor: u64,
    sets: Option<&CharacteristicSets>,
    budget: usize,
    fixed: Option<&[usize]>,
    stop: &dyn Fn() -> bool,
) -> Result<Plan, Stopped> {
    let work = Work {
        done: Cell::new(0),
        budget,
        passed: Cell::new(false),
    };
    // `p`'s links read for a chain, each looking up the predicates of `from` and `to`.
    let pair = |sets: &CharacteristicSets, from: &[u64], p: u64, to: &[u64]| {
        let reads = sets.links_of(p) * (from.len() + to.len()).max(1);
        work.take(reads).then(|| sets.pair(from, p, to)).flatten()
    };
    let open = Star::open;
    // `state` with pattern `j` joined, its `step` estimated already.
    let apply = |mut state: State, j: usize, step: Step| -> State {
        let input = &inputs[j];
        // The step that joins the last input (paths included: they are inputs too) makes a
        // state nothing is estimated from: only its order, estimates and cost are read, so
        // what a later step would read (values, stars, edges, shares) isn't built.
        if state.order.len() + 1 < inputs.len() {
            // A variable's distinct values: the fewest any pattern binding it has, the
            // domain its values are drawn from. Not cut to the rows: 543 `worksFor` rows
            // hold values from its 7 k subjects, of which `?x a Chair`'s 189 keep a share,
            // not all.
            for &(v, d) in &input.vars {
                let d = (d as f64).max(1.0);
                let known = &mut state.distinct[v];
                *known = if *known > 0.0 { known.min(d) } else { d };
            }
            // Stars and edges are read only by the estimates from the sets: a search without
            // them builds and copies neither.
            if let Some(sets) = sets {
                if let Some(star) = input.star.filter(open) {
                    let subject = state.stars.entry(star.subject).or_default();
                    subject.rows = match &subject.rows {
                        Some(rows) => work.take(rows.len()).then(|| {
                            note_statistics(rows.len(), 0);
                            sets.star_with(rows, star.predicate)
                        }),
                        None => work.take(sets.sets_having(star.predicate)).then(|| {
                            let rows = sets.star_of(star.predicate);
                            note_statistics(rows.len(), 0);
                            rows
                        }),
                    };
                    subject.predicates.push(star.predicate);
                    note_star_state();
                }
                if let (Some(object), Some(star)) = (input.edge_object, input.star)
                    && star.subject != object
                {
                    state.edges_into.entry(object).or_insert(j);
                    note_star_state();
                }
            }
            let partner = (input.count as f64).max(1.0);
            for &(i, both) in &input.overlaps {
                let share = both as f64 / partner;
                let kept = state.kept.entry(i).or_insert(share);
                *kept = kept.min(share);
            }
        }
        state.order.push(j);
        state.estimates.push(step.rows);
        state.rows = step.rows;
        state.cost = step.cost;
        state
    };
    let start = |j: usize| {
        let rows = inputs[j].count as f64;
        let mut state = State {
            cost: rows,
            rows,
            distinct: vec![0.0; variables],
            order: Vec::new(),
            estimates: Vec::new(),
            stars: HashMap::new(),
            edges_into: HashMap::new(),
            kept: HashMap::new(),
        };
        state = apply(state, j, Step { rows, cost: rows });
        for &(v, d) in &inputs[j].vars {
            state.distinct[v] = (d as f64).clamp(1.0, rows.max(1.0));
        }
        state
    };
    let estimate = |state: &State, j: usize| -> Step {
        note_statistics(0, 1);
        let input = &inputs[j];
        let count = input.count as f64;
        let star_input = input.star.filter(open);
        // A star pattern on `?y` after an edge `?x p ?y`: its rows from the
        // characteristic pairs.
        let chained = sets.zip(star_input).and_then(|(sets, star)| {
            if state.distinct[star.subject] == 0.0 || !sets.has_pairs() {
                return None;
            }
            let edge = *state.edges_into.get(&star.subject)?;
            let edge_star = inputs[edge].star?;
            let from = state.on(edge_star.subject, Some(&edge_star));
            let mut to = state.on(star.subject, None);
            let before = pair(sets, &from, edge_star.predicate, &to)?;
            to.push(star.predicate);
            let after = pair(sets, &from, edge_star.predicate, &to)?;
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
            let star = state.stars.get(&object)?;
            let before = star.rows.as_ref()?.rows();
            let after = pair(sets, &[], edge.predicate, &star.predicates)?;
            let factor = if before > 0.0 { after / before } else { 0.0 };
            Some((object, factor))
        };
        // A star extended: its rows from the characteristic sets.
        let star = chained.or_else(reversed).or_else(|| {
            let star = star_input?;
            if state.distinct[star.subject] == 0.0 {
                return None;
            }
            let (sets, rows) = sets.zip(state.stars.get(&star.subject)?.rows.as_ref())?;
            if !work.take(rows.len()) {
                return None;
            }
            note_statistics(rows.len(), 0);
            let (before, after) = (rows.rows(), sets.star_rows_with(rows, star.predicate));
            let factor = if before > 0.0 { after / before } else { 0.0 };
            Some((star.subject, factor * star.selectivity))
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
            if let Some(&kept) = state.kept.get(&j) {
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
        Step {
            rows,
            cost: state.cost + access + rows,
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
        work: work.done.get(),
    };
    if n == 0 {
        return Ok(Plan {
            order: Vec::new(),
            rows: Vec::new(),
            work: 0,
        });
    }
    // Every start state of an open star reads its predicate's sets (`star_of`) before the
    // search first checks its work: the exhaustive and greedy searches build all their
    // starts in input order, a fixed order its first ([`start_stars`]). Where those reads
    // would pass the budget, the search stops as it would after making them, with the work
    // counted before the one that passes, having made none: their lengths come from the
    // sets' index.
    if let Some(sets) = sets {
        let mut done = 0usize;
        for star in start_stars(inputs, fixed.map(|order| order[0])) {
            let next = done.saturating_add(sets.sets_having(star.predicate));
            if next > budget {
                return Err(Stopped::Budget { work: done });
            }
            done = next;
        }
    }
    // Every read and copy of the sets is counted before it is made; past the budget none is
    // made, and the search stops after the estimate or step that would have made it.
    if let Some(order) = fixed {
        let mut state = start(order[0]);
        work.check()?;
        for &j in &order[1..] {
            let step = estimate(&state, j);
            state = apply(state, j, step);
            work.check()?;
        }
        return Ok(finish(state));
    }
    if n > DP_PATTERNS {
        // Greedy: the cheapest start, then the cheapest extension each time.
        let mut starts: Vec<State> = (0..n).map(start).collect();
        work.check()?;
        let kept = match n > GREEDY_ALL_STARTS {
            true => GREEDY_STARTS,
            false => n,
        };
        let kept = kept.min((GREEDY_EFFORT / (n * n / 2)).max(1));
        if kept < n {
            starts.sort_by(|a, b| a.cost.total_cmp(&b.cost).then(a.order.cmp(&b.order)));
            starts.truncate(kept);
        }
        let mut best: Option<State> = None;
        for mut state in starts {
            let mut used = vec![false; n];
            used[state.order[0]] = true;
            while state.order.len() < n {
                if stop() {
                    return Err(Stopped::Cancelled);
                }
                // The cheapest candidate, the first of equally cheap ones.
                let (j, step) = candidates(&state, &|j| used[j])
                    .into_iter()
                    .map(|j| (j, estimate(&state, j)))
                    .reduce(|a, b| if b.1.cost < a.1.cost { b } else { a })
                    .expect("a pattern is left");
                used[j] = true;
                state = apply(state, j, step);
                work.check()?;
            }
            if best.as_ref().is_none_or(|b| state.better_than(b)) {
                best = Some(state);
            }
        }
        return Ok(finish(best.expect("inputs are not empty")));
    }
    let mut best: Vec<Option<State>> = vec![None; 1 << n];
    for j in 0..n {
        best[1 << j] = Some(start(j));
    }
    work.check()?;
    // A state's candidates and the steps that beat their slot's state: only those are built.
    let mut winners: Vec<(usize, Step)> = Vec::with_capacity(n);
    for used in 1u64..(1 << n) {
        if stop() {
            return Err(Stopped::Cancelled);
        }
        let Some(state) = best[used as usize].take() else {
            continue;
        };
        // Each candidate goes to a slot of its own, so each is decided against the state
        // as it is, in the candidates' order; one that only ties is not stored.
        winners.clear();
        for j in candidates(&state, &|j| used & (1 << j) != 0) {
            let step = estimate(&state, j);
            let slot = &best[(used | (1 << j)) as usize];
            if slot
                .as_ref()
                .is_none_or(|s| State::beats(step.cost, state.estimates[0], s))
            {
                winners.push((j, step));
            }
        }
        work.check()?;
        // Later masks are supersets, so this state is read no more: its last winner takes
        // it, the others a copy. With none (the full mask among them) it stays.
        let Some((&(last, step), rest)) = winners.split_last() else {
            best[used as usize] = Some(state);
            continue;
        };
        for &(j, step) in rest {
            // A copy copies the state's stars' sets too.
            work.charge(state.copied())?;
            best[(used | (1 << j)) as usize] = Some(apply(state.clone(), j, step));
            work.check()?;
        }
        best[(used | (1 << last)) as usize] = Some(apply(state, last, step));
        work.check()?;
    }
    Ok(finish(
        best[(1 << n) - 1]
            .take()
            .expect("every pattern is reachable"),
    ))
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

    /// Spec: the master's order of 12 November 13:53, "enter the set search only when that
    /// plan's own estimate says the query is expensive enough", with "effort follows expected
    /// work", and its doubt review: a plan's expected work is its steps' estimates summed, NaN
    /// left out and infinity kept; a BGP is planned with the sets where its starts' reads are
    /// within [`SET_START_ALLOWANCE`] or it expects at least [`SET_GATE_ROWS`] rows, not else;
    /// its budget is [`SET_BUDGET`], or [`SET_ENTRIES_PER_ROW`] per expected row where that is
    /// more, saturating (NaN to the floor); a test's budget and decision replace the planner's.
    #[test]
    fn the_sets_are_read_only_where_their_lists_are_short_or_the_query_expects_much_work() {
        let plan = |rows: Vec<f64>| Plan {
            order: (0..rows.len()).collect(),
            rows,
            work: 0,
        };
        assert_eq!(expected_work(&plan(vec![f64::NAN, 10.0, 5.5])), 15.5);
        assert_eq!(
            expected_work(&plan(vec![1.0, f64::INFINITY])),
            f64::INFINITY
        );
        let many = SET_START_ALLOWANCE + 1;
        assert_eq!(
            [
                admits(0.0, SET_START_ALLOWANCE),
                admits(SET_GATE_ROWS - 1.0, many),
                admits(SET_GATE_ROWS, many),
                admits(f64::NAN, many),
                admits(f64::INFINITY, usize::MAX),
            ],
            [true, false, true, false, true]
        );
        let large = SET_BUDGET as f64 / SET_ENTRIES_PER_ROW;
        assert_eq!(budget_for(0.0), SET_BUDGET);
        assert_eq!(budget_for(f64::NAN), SET_BUDGET);
        assert_eq!(budget_for(large / 2.0), SET_BUDGET);
        assert_eq!(budget_for(4.0 * large), 4 * SET_BUDGET);
        assert_eq!(budget_for(f64::INFINITY), usize::MAX);
        BUDGET.set(Some(2));
        ADMIT.set(Some(false));
        let (budget, admit) = (budget_for(4.0 * large), admits(0.0, 0));
        BUDGET.set(None);
        ADMIT.set(None);
        assert_eq!((budget, admit), (2, false));
    }

    #[test]
    fn a_selective_filter_joins_before_a_fan_out() {
        // ?x memberOf <d> (50) . ?x takesCourse ?c (300k, 3 per x) . ?x a Grad (5k)
        let inputs = [
            input(50, &[(0, 50)]),
            input(300_000, &[(0, 100_000), (1, 2_000)]),
            input(5_000, &[(0, 5_000)]),
        ];
        let plan = order(&inputs, 2, 32, None, SET_BUDGET, &|| false).unwrap();
        assert_eq!(plan.order, vec![0, 2, 1]);
        assert_eq!(plan.rows.len(), 3);
    }

    #[test]
    fn checks_keep_at_most_their_input_and_steps_at_least_a_row() {
        // ?x p <c> (40) . ?x a C (100k, every x distinct): a check of 40 rows.
        let inputs = [input(40, &[(0, 40)]), input(100_000, &[(0, 100_000)])];
        let plan = along(&inputs, 1, 32, None, SET_BUDGET, &[0, 1]).unwrap();
        assert!(plan.rows[1] <= 40.0, "{:?}", plan.rows);
        // ?x p ?y (10) . ?y q ?z (5, 5 distinct ?y among 1M): 0.00005 rows, shown as one.
        let inputs = [
            input(10, &[(0, 10), (1, 10)]),
            input(5, &[(1, 1_000_000), (2, 5)]),
        ];
        let plan = along(&inputs, 3, 32, None, SET_BUDGET, &[0, 1]).unwrap();
        assert_eq!(plan.rows[1], 1.0);
        // Two single-variable patterns sharing 15 of the first's 189 values.
        let mut inputs = [input(189, &[(0, 189)]), input(239, &[(0, 239)])];
        inputs[0].overlaps = vec![(1, 15)];
        inputs[1].overlaps = vec![(0, 15)];
        let plan = along(&inputs, 1, 32, None, SET_BUDGET, &[0, 1]).unwrap();
        assert!((plan.rows[1] - 15.0).abs() < 1e-9, "{:?}", plan.rows);
    }

    #[test]
    fn disconnected_patterns_still_get_an_order() {
        let inputs = [input(10, &[(0, 10)]), input(3, &[(1, 3)])];
        let plan = order(&inputs, 2, 32, None, SET_BUDGET, &|| false).unwrap();
        assert_eq!(plan.order.len(), 2);
        assert!((plan.rows[1] - 30.0).abs() < 1e-9);
    }

    /// A star of 250 patterns over characteristic sets: every step reads each set the star
    /// still matches at most once, however many patterns the star has (it read every
    /// pattern's predicate in every candidate set, twice per candidate: 250 triple-term
    /// patterns planned in 114 s, 12 October 2026). And it stops at the first step after
    /// its query is cancelled.
    #[test]
    fn a_large_star_reads_each_matching_set_once_a_step_and_stops_when_asked() {
        use nrese_rdf::{GraphName, NamedNode, Quad};
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let node = |name: String| NamedNode::new_unchecked(format!("http://e/{name}"));
        let mut tx = engine.transaction();
        for s in 0..300u32 {
            for p in 0..12u32 {
                if (s * 7 + p * 3) % 5 < 3 {
                    let quad = Quad::new(
                        node(format!("s{s}")),
                        node(format!("p{p}")),
                        node(format!("o{}", s % 17)),
                        GraphName::DefaultGraph,
                    );
                    tx.insert(quad.as_ref());
                }
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let sets = snapshot.characteristic_sets_in(Default::default()).unwrap();
        let predicate = |p: usize| {
            let p = node(format!("p{}", p % 12));
            snapshot.lookup(p.as_ref().into()).unwrap().raw()
        };
        let inputs: Vec<Input> = (0..250)
            .map(|i| Input {
                count: 180,
                vars: vec![(0, 180), (i + 1, 17)],
                star: Some(Star {
                    subject: 0,
                    predicate: predicate(i),
                    selectivity: 1.0,
                }),
                edge_object: None,
                overlaps: Vec::new(),
            })
            .collect();
        // The reads of the whole search, not as many as the budget allows.
        STATISTICS.with(|c| c.set((0, 0)));
        let plan = order(&inputs, 251, 32, Some(&sets), usize::MAX, &|| false).unwrap();
        assert_eq!(plan.order.len(), 250);
        let (reads, steps) = STATISTICS.with(|c| c.get());
        assert!(
            steps > 0 && reads <= steps * sets.len(),
            "{reads} reads in {steps} steps"
        );
        let asked = std::cell::Cell::new(0);
        let stop = || {
            asked.set(asked.get() + 1);
            asked.get() > 3
        };
        assert!(matches!(
            order(&inputs, 251, 32, Some(&sets), usize::MAX, &stop),
            Err(Stopped::Cancelled)
        ));
        assert_eq!(asked.get(), 4, "stopped at the first step after");
    }

    /// The greedy search's estimates over all its starts stay within its effort, or one
    /// start's n²/2 where a single start needs more: a star of 1,000 patterns estimated
    /// 8 million candidates, twice per query.
    #[test]
    fn the_greedy_search_keeps_to_its_effort() {
        for n in [100usize, 250, 1_000] {
            let inputs: Vec<Input> = (0..n)
                .map(|i| input(1_000 + i as u64, &[(0, 900), (i + 1, 500)]))
                .collect();
            STATISTICS.with(|c| c.set((0, 0)));
            let plan = order(&inputs, n + 1, 32, None, SET_BUDGET, &|| false).unwrap();
            assert_eq!(plan.order.len(), n);
            let (_, steps) = STATISTICS.with(|c| c.get());
            assert!(
                steps <= GREEDY_EFFORT.max(n * n / 2) + n,
                "{n} patterns: {steps}"
            );
        }
    }

    /// BGPs too large for the exact order get a greedy one: a star of 14 patterns, and
    /// chains beyond 64 (a 64-bit membership mask overflowed at 65 patterns: the review
    /// of 3 October 2026, P2), up to 200 patterns over 201 variables.
    #[test]
    fn large_bgps_are_ordered_greedily() {
        let inputs: Vec<Input> = (0..14)
            .map(|i| input(100 + i, &[(0, 100), (i as usize + 1, 50)]))
            .collect();
        let plan = order(&inputs, 15, 32, None, SET_BUDGET, &|| false).unwrap();
        let mut sorted = plan.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..14).collect::<Vec<_>>());
        for n in [64usize, 65, 200] {
            let inputs: Vec<Input> = (0..n)
                .map(|i| input(1_000 + i as u64, &[(i, 500), (i + 1, 500)]))
                .collect();
            let plan = order(&inputs, n + 1, 32, None, SET_BUDGET, &|| false).unwrap();
            let mut sorted = plan.order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..n).collect::<Vec<_>>());
            assert_eq!(plan.rows.len(), n);
        }
    }
    /// A query's cancellation reaches the orderer while it plans the large patterns of
    /// `tests/it/depth_tests.rs` (a path of 1,000 steps spelled out as triple patterns, stars
    /// of 1,000 patterns, of 1,000 single-variable checks and of 250 triple terms; a
    /// 1,000-step path planned for 28 s, a 250-pattern star for 114 s, unable to stop:
    /// 12 October 2026). Cancelled halfway through the steps its planning takes, a query
    /// stops at that step, refused with `Cancelled`.
    #[test]
    fn large_patterns_stop_planning_at_the_step_they_are_cancelled() {
        use crate::{CancellationToken, QueryEvaluationError, QueryOptions, Resources};
        use nrese_rdf::{GraphName, NamedNode, Quad};
        use std::cell::Cell;
        use std::rc::Rc;
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let node = |name: String| NamedNode::new_unchecked(format!("http://e/{name}"));
        let mut tx = engine.transaction();
        for s in 0..5 {
            for i in 0..7 {
                let quad = Quad::new(
                    node(format!("s{s}")),
                    node(format!("p{i}")),
                    node(format!("s{}", (s + i) % 5)),
                    GraphName::DefaultGraph,
                );
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let p = |i: usize| format!("<http://e/p{}>", i % 7);
        let patterns = |n: usize, f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<String>();
        for (shape, text) in [
            (
                "chain",
                format!(
                    "SELECT * WHERE {{ ?s {} ?o }}",
                    (0..1_000).map(p).collect::<Vec<_>>().join("/")
                ),
            ),
            (
                "star",
                format!(
                    "SELECT * WHERE {{ {} }}",
                    patterns(1_000, &|i| format!("?s {} ?o{i} . ", p(i)))
                ),
            ),
            (
                "checks",
                format!(
                    "SELECT * WHERE {{ {} }}",
                    patterns(1_000, &|i| format!("?s {} <http://e/s{}> . ", p(i), i % 5))
                ),
            ),
            (
                "triple-terms",
                format!(
                    "SELECT * WHERE {{ ?s ?q ?o . {} }}",
                    patterns(250, &|i| format!(
                        "?s {} <<( ?a{i} {} ?o )>> . ",
                        p(i),
                        p(i + 1)
                    ))
                ),
            ),
        ] {
            let query = nrese_sparql_syntax::SparqlParser::new()
                .parse_query(&text)
                .unwrap();
            let steps = Rc::new(Cell::new(0usize));
            let count = Rc::clone(&steps);
            STEP.set(Some(Box::new(move || count.set(count.get() + 1))));
            let planned = crate::plan_query(&snapshot, &query, &QueryOptions::default());
            let all = steps.replace(0);
            assert!(planned.is_ok(), "{shape}: {:?}", planned.err());
            assert!(all >= 100, "{shape}: planned in {all} steps");
            let token = CancellationToken::new();
            let (count, cancel) = (Rc::clone(&steps), token.clone());
            STEP.set(Some(Box::new(move || {
                count.set(count.get() + 1);
                if count.get() == all / 2 {
                    cancel.cancel();
                }
            })));
            let options = QueryOptions {
                resources: Resources {
                    cancellation: Some(token),
                    ..Resources::default()
                },
                ..QueryOptions::default()
            };
            let planned = crate::plan_query(&snapshot, &query, &options);
            STEP.set(None);
            assert!(
                matches!(planned, Err(QueryEvaluationError::Cancelled)),
                "{shape}: {:?}",
                planned.err()
            );
            assert_eq!(steps.get(), all / 2, "{shape}: steps of {all}");
        }
    }
}
