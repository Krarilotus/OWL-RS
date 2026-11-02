//! SPARQL aggregate values and per-group helpers. Group planning, scheduling and
//! result interning remain in the executor; these helpers preserve row order and errors.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nrese_engine::TermId;
use nrese_exec::group::{Groups, group_rows};
use nrese_exec::{Budget, BudgetExceeded, Grant, IdTable, Reservation, UNDEF};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{Literal, Term, Variable};
use nrese_sparql_syntax::algebra::{AggregateExpression, AggregateFunction, Expression};
use nrese_xsd::{Decimal, Double, Float, Integer};

use super::expr::Evaluator;
use super::numeric::Numeric;
use super::value::Value;
use super::{
    Context, NativeResult, ScanPattern, Solutions, calendar, expression_variables, pushdown, value,
};
use crate::QueryEvaluationError;

/// An aggregate's value: a stored or computed id, or a new term the query interns.
pub(super) enum Agg {
    Id(u64),
    Term(Term),
}

/// Computes aggregates over groups of rows. Thread-safe given a thread-safe `term`, so
/// groups can be aggregated in parallel; the caller interns the results.
pub(super) struct Aggregator<'a> {
    pub(super) evaluator: &'a Evaluator,
    pub(super) term: &'a dyn Fn(u64) -> Option<Term>,
    /// The value of an expression over one variable, per (expression, id): groups share
    /// most of their values, and `STR(?x)` of an id is the same in each of them.
    pub(super) memo: RefCell<HashMap<(usize, u64), Option<Term>>>,
    /// The same for SUM and AVG: the value as a number ([`Aggregator::numeric_total`]).
    pub(super) numbers: RefCell<HashMap<(usize, u64), Number>>,
    /// Where the terms it keeps are charged; `None` where it only evaluates.
    pub(super) kept: Option<&'a Kept>,
}

/// The terms an aggregation keeps, charged to the query as they are made, whatever size
/// the query's expressions give them: what [`Aggregator::memo`] remembers (released with
/// it), a group's values while its aggregate is computed, and the results until the caller
/// interns them ([`Kept::results`]). A refusal is kept: the caller ends the aggregation at
/// the group it happened in. All three through one [`Grant`] (a term costs one atomic, no
/// call up the levels), each kind's share counted here: one grant per aggregation part, so
/// the room it holds ahead is one grant's.
pub(super) struct Kept {
    grant: RefCell<Grant>,
    /// What each [`Held`] kind holds of the grant's charge.
    held: [Cell<usize>; 3],
    /// What [`Aggregator::value_of`] charged to `results` before it made the result.
    prepaid: Cell<usize>,
    refused: Cell<Option<BudgetExceeded>>,
}

/// Which of [`Kept`]'s charges a term goes to.
#[derive(Clone, Copy)]
enum Held {
    Memo,
    Values,
    Results,
}

/// What a kept term holds: its slot and its strings.
fn kept_bytes(term: &Term) -> usize {
    std::mem::size_of::<Term>().saturating_add(crate::cache::term_heap(term))
}

impl Kept {
    pub(super) fn new(budget: &Arc<Budget>) -> Self {
        Self {
            grant: RefCell::new(Grant::new(Arc::clone(budget))),
            held: [Cell::new(0), Cell::new(0), Cell::new(0)],
            prepaid: Cell::new(0),
            refused: Cell::new(None),
        }
    }

    /// Charges `bytes` as `kind`; false once anything was refused.
    fn hold(&self, kind: Held, bytes: usize) -> bool {
        if self.refused.get().is_some() {
            return false;
        }
        match self.grant.borrow_mut().take(bytes) {
            Ok(()) => {
                let held = &self.held[kind as usize];
                held.set(held.get() + bytes);
                true
            }
            Err(refused) => {
                self.refused.set(Some(refused));
                false
            }
        }
    }

    /// Releases up to `bytes` of what `kind` holds.
    fn release(&self, kind: Held, bytes: usize) {
        let held = &self.held[kind as usize];
        let bytes = bytes.min(held.get());
        held.set(held.get() - bytes);
        self.grant.borrow_mut().give_back(bytes);
    }

    /// A group's values are gone: so is their charge.
    fn values_done(&self) {
        self.release(Held::Values, self.held[Held::Values as usize].get());
    }

    /// What the budget refused, if it did.
    pub(super) fn refused(&self) -> Option<BudgetExceeded> {
        self.refused.get()
    }

    /// The results' charge, handed to the caller (the bytes, which it releases once the
    /// results are interned and dropped); what was remembered is released with `self`.
    pub(super) fn results(self) -> usize {
        let others = self.held[Held::Memo as usize].get() + self.held[Held::Values as usize].get();
        let mut grant = self.grant.into_inner();
        grant.give_back(others);
        grant.transfer()
    }
}

/// An expression's value for SUM and AVG.
#[derive(Clone, Copy)]
pub(super) enum Number {
    Value(Numeric),
    /// An error: SUM and AVG are unbound.
    Error,
    /// Not a number (a duration, or no sum): the general path decides.
    Other,
}

impl Aggregator<'_> {
    /// SUM (or AVG with `average`) of `expr`, an expression of one variable, over `rows`:
    /// each id's number found once, then added up without a term per row. `None` where the
    /// general path must decide (a value that isn't a number, a value drawn per row). BSBM
    /// BI q4 averaged `xsd:float(xsd:string(?price))` over 154 M rows.
    fn numeric_total(
        &self,
        solutions: &Solutions,
        rows: &[usize],
        expr: &Expression,
        average: bool,
    ) -> Option<Agg> {
        if pushdown::per_solution(expr) {
            return None;
        }
        let mut columns: Vec<usize> = expression_variables(expr)
            .iter()
            .filter_map(|v| solutions.column(v))
            .collect();
        columns.sort_unstable();
        columns.dedup();
        let [column] = columns[..] else {
            return None;
        };
        let key = expr as *const Expression as usize;
        let table = &solutions.table;
        let mut numbers = self.numbers.borrow_mut();
        let mut total = Some(Numeric::Integer(Integer::from(0)));
        let mut error = false;
        for &row in rows {
            let number =
                *numbers
                    .entry((key, table.get(row, column)))
                    .or_insert_with(|| {
                        match self.evaluator.eval(expr, &self.binding(solutions, row)) {
                            None => Number::Error,
                            Some(term) => match Numeric::of(&Value::of(&term)) {
                                Some(number) => Number::Value(number),
                                None => Number::Other,
                            },
                        }
                    });
            match number {
                Number::Value(number) => total = total.and_then(|total| total.add(number)),
                Number::Error => error = true,
                Number::Other => return None,
            }
        }
        Some(finish_total(total, error, rows.len() as u64, average))
    }

    pub(super) fn binding<'s>(
        &'s self,
        solutions: &'s Solutions,
        row: usize,
    ) -> impl Fn(&Variable) -> Option<Term> + 's {
        move |variable| (self.term)(solutions.table.get(row, solutions.column(variable)?))
    }

    /// An aggregate over one variable's ids without decoding terms, where that is exact:
    /// COUNT always, and SUM/AVG/MIN/MAX when every value is an inline integer (whose id
    /// order is value order). `None` means "evaluate on terms". Errors as in §18.5.1: an
    /// unbound value makes SUM/AVG/MIN/MAX unbound, and an i64 overflow makes SUM/AVG
    /// unbound.
    fn aggregate_ids(
        &self,
        name: &AggregateFunction,
        mut ids: Vec<u64>,
        distinct: bool,
    ) -> Option<Agg> {
        let dedup = |ids: &mut Vec<u64>| {
            let mut seen = std::collections::HashSet::with_capacity(ids.len());
            ids.retain(|id| seen.insert(*id));
        };
        match name {
            AggregateFunction::Count => {
                ids.retain(|&id| id != UNDEF);
                if distinct {
                    dedup(&mut ids);
                }
                Some(Agg::Term(integer(ids.len() as u64)))
            }
            AggregateFunction::Sum
            | AggregateFunction::Avg
            | AggregateFunction::Min
            | AggregateFunction::Max => {
                if ids.contains(&UNDEF) {
                    return Some(Agg::Id(UNDEF));
                }
                let values: Option<Vec<i64>> = ids
                    .iter()
                    .map(|&id| TermId::from_raw(id).as_inline_integer())
                    .collect();
                let values = values?;
                if distinct {
                    dedup(&mut ids);
                }
                let values: Vec<i64> = if distinct {
                    ids.iter()
                        .filter_map(|&id| TermId::from_raw(id).as_inline_integer())
                        .collect()
                } else {
                    values
                };
                // By value: xsd:integer and derived ids are of different kinds.
                let value = |id: &u64| TermId::from_raw(*id).as_inline_integer();
                Some(match name {
                    AggregateFunction::Min => {
                        Agg::Id(ids.iter().copied().min_by_key(value).unwrap_or(UNDEF))
                    }
                    AggregateFunction::Max => {
                        Agg::Id(ids.iter().copied().max_by_key(value).unwrap_or(UNDEF))
                    }
                    _ => {
                        let Some(sum) = values.iter().try_fold(0i64, |acc, &v| acc.checked_add(v))
                        else {
                            return Some(Agg::Id(UNDEF));
                        };
                        if *name == AggregateFunction::Sum {
                            Agg::Term(
                                Literal::new_typed_literal(sum.to_string(), xsd::INTEGER).into(),
                            )
                        } else if values.is_empty() {
                            Agg::Term(integer(0))
                        } else {
                            match Decimal::from(sum).checked_div(Decimal::from(values.len() as i64))
                            {
                                Some(avg) => Agg::Term(
                                    Literal::new_typed_literal(avg.to_string(), xsd::DECIMAL)
                                        .into(),
                                ),
                                None => Agg::Id(UNDEF),
                            }
                        }
                    }
                })
            }
            _ => None,
        }
    }

    /// Charges `term` as `kind` of [`Self::kept`]; false once the budget refused anything.
    fn hold(&self, kind: Held, term: &Option<Term>) -> bool {
        match (self.kept, term) {
            (Some(kept), Some(term)) => kept.hold(kind, kept_bytes(term)),
            (Some(kept), None) => kept.refused().is_none(),
            (None, _) => true,
        }
    }

    /// The expression's value for each of `rows`. With `distinct`, only for the first row
    /// of each combination of the expression's variables: the repeats can't add a value.
    /// Each value is charged as it is made ([`Kept`]); a refusal ends the list.
    fn evaluated(
        &self,
        solutions: &Solutions,
        rows: &[usize],
        expr: &Expression,
        distinct: bool,
    ) -> Vec<Option<Term>> {
        let mut columns: Vec<usize> = expression_variables(expr)
            .iter()
            .filter_map(|v| solutions.column(v))
            .collect();
        columns.sort_unstable();
        columns.dedup();
        let table = &solutions.table;
        let eval = |row: usize| self.evaluator.eval(expr, &self.binding(solutions, row));
        let mut values = Vec::with_capacity(rows.len());
        let slots = rows
            .len()
            .saturating_mul(std::mem::size_of::<Option<Term>>());
        if let Some(kept) = self.kept
            && !kept.hold(Held::Values, slots)
        {
            return values;
        }
        let mut push = |value: Option<Term>| {
            let fits = self.hold(Held::Values, &value);
            if fits {
                values.push(value);
            }
            fits
        };
        // A value drawn per row (RAND, BNODE, ...) is drawn for every row.
        if pushdown::per_solution(expr) {
            for &row in rows {
                if !push(eval(row)) {
                    break;
                }
            }
        } else if let [column] = columns[..] {
            // One variable: its ids stand for the rows, and the values are remembered
            // across groups.
            let key = expr as *const Expression as usize;
            let mut seen = HashSet::new();
            let mut memo = self.memo.borrow_mut();
            for &row in rows {
                let id = table.get(row, column);
                if distinct && !seen.insert(id) {
                    continue;
                }
                // One lookup, one copy (the remembered value's), as before it was charged.
                let value = match memo.entry((key, id)) {
                    std::collections::hash_map::Entry::Occupied(known) => known.get().clone(),
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        let value = eval(row);
                        if self.hold(Held::Memo, &value) {
                            slot.insert(value).clone()
                        } else {
                            value
                        }
                    }
                };
                if !push(value) {
                    break;
                }
            }
        } else if !distinct || columns.is_empty() {
            for &row in rows {
                if !push(eval(row)) {
                    break;
                }
            }
        } else {
            let mut seen: HashSet<Vec<u64>> = HashSet::new();
            for &row in rows {
                if seen.insert(columns.iter().map(|&c| table.get(row, c)).collect())
                    && !push(eval(row))
                {
                    break;
                }
            }
        }
        values
    }

    /// `aggregate` over `rows`. Its values are released once it is computed, and the
    /// result, kept until the caller interns it, is charged ([`Kept`]).
    pub(super) fn aggregate(
        &self,
        solutions: &Solutions,
        rows: &[usize],
        aggregate: &AggregateExpression,
    ) -> Agg {
        let result = self.value_of(solutions, rows, aggregate);
        if let Some(kept) = self.kept {
            kept.values_done();
            // What `value_of` charged before it made the result is part of it.
            let prepaid = kept.prepaid.take();
            match &result {
                Agg::Term(term) => {
                    kept.hold(Held::Results, kept_bytes(term).saturating_sub(prepaid));
                }
                Agg::Id(_) => kept.release(Held::Results, prepaid),
            }
        }
        result
    }

    fn value_of(
        &self,
        solutions: &Solutions,
        rows: &[usize],
        aggregate: &AggregateExpression,
    ) -> Agg {
        match aggregate {
            AggregateExpression::CountSolutions { distinct } => {
                let count = if *distinct {
                    let mut seen: Vec<Vec<u64>> =
                        rows.iter().map(|&r| solutions.table.row(r)).collect();
                    seen.sort_unstable();
                    seen.dedup();
                    seen.len()
                } else {
                    rows.len()
                };
                Agg::Term(integer(count as u64))
            }
            AggregateExpression::FunctionCall {
                name,
                expr,
                distinct,
            } => {
                if let Expression::Variable(variable) = expr {
                    let ids: Vec<u64> = match solutions.column(variable) {
                        Some(column) => rows
                            .iter()
                            .map(|&r| solutions.table.get(r, column))
                            .collect(),
                        None => vec![UNDEF; rows.len()],
                    };
                    if let Some(result) = self.aggregate_ids(name, ids, *distinct) {
                        return result;
                    }
                }
                if !*distinct
                    && matches!(name, AggregateFunction::Sum | AggregateFunction::Avg)
                    && let Some(result) =
                        self.numeric_total(solutions, rows, expr, *name == AggregateFunction::Avg)
                {
                    return result;
                }
                let evaluated = self.evaluated(solutions, rows, expr, *distinct);
                // COUNT skips errors and SAMPLE takes the first value, but one error makes
                // SUM, AVG, MIN and MAX unbound.
                let fails_on_error =
                    !matches!(name, AggregateFunction::Count | AggregateFunction::Sample);
                if fails_on_error && evaluated.iter().any(Option::is_none) {
                    return Agg::Id(UNDEF);
                }
                let mut values: Vec<Term> = evaluated.into_iter().flatten().collect();
                if *distinct {
                    // The first of each, in order.
                    let mut seen: HashSet<&Term> = HashSet::with_capacity(values.len());
                    let first: Vec<bool> = values.iter().map(|value| seen.insert(value)).collect();
                    let mut first = first.into_iter();
                    values.retain(|_| first.next().unwrap_or(false));
                }
                let result = match name {
                    AggregateFunction::Count => Some(integer(values.len() as u64)),
                    AggregateFunction::Sample => values.into_iter().next(),
                    // The first of equal extremes.
                    AggregateFunction::Min => values.into_iter().reduce(|best, v| {
                        if value::order(Some(&v), Some(&best)).is_lt() {
                            v
                        } else {
                            best
                        }
                    }),
                    AggregateFunction::Max => values.into_iter().reduce(|best, v| {
                        if value::order(Some(&v), Some(&best)).is_gt() {
                            v
                        } else {
                            best
                        }
                    }),
                    AggregateFunction::Sum => sum(&values),
                    AggregateFunction::Avg => average(&values),
                    AggregateFunction::GroupConcat { separator } => {
                        // The string, its separator once between each two values however
                        // long the query's separator is, charged before it is made: as the
                        // result's, which `aggregate` charges the rest of.
                        // A value that isn't a string makes it unbound: nothing to charge.
                        let separator = separator.as_deref().unwrap_or(" ");
                        match (self.kept, group_concat_bytes(&values, separator)) {
                            (_, None) => None,
                            (Some(kept), Some(bytes)) => {
                                if !kept.hold(Held::Results, bytes) {
                                    return Agg::Id(UNDEF);
                                }
                                kept.prepaid.set(bytes);
                                group_concat(&values, separator)
                            }
                            (None, Some(_)) => group_concat(&values, separator),
                        }
                    }
                    _ => None,
                };
                result.map_or(Agg::Id(UNDEF), Agg::Term)
            }
        }
    }
}

/// SUM (or AVG with `average`) of `count` values adding up to `total`, as `sum` and
/// `average` give it: unbound after an error or an overflow, AVG of none 0, of integers
/// and decimals a decimal.
pub(super) fn finish_total(total: Option<Numeric>, error: bool, count: u64, average: bool) -> Agg {
    let Some(total) = total.filter(|_| !error) else {
        return Agg::Id(UNDEF);
    };
    if !average {
        return Agg::Term(total.term());
    }
    if count == 0 {
        return Agg::Term(integer(0));
    }
    let mean = match total {
        Numeric::Integer(_) | Numeric::Decimal(_) => total
            .decimal()
            .and_then(|sum| sum.checked_div(Decimal::from(count as i64)))
            .map(|mean| Numeric::Decimal(mean).term()),
        Numeric::Float(f) => Some(Numeric::Float(f / Float::from(count as f32)).term()),
        Numeric::Double(d) => Some(Numeric::Double(d / Double::from(count as f64)).term()),
    };
    mean.map_or(Agg::Id(UNDEF), Agg::Term)
}

pub(super) fn counts_rows(aggregate: &AggregateExpression, scan: &ScanPattern) -> bool {
    match aggregate {
        AggregateExpression::CountSolutions { distinct: false } => true,
        AggregateExpression::FunctionCall {
            name: AggregateFunction::Count,
            expr: Expression::Variable(v),
            distinct: false,
        } => scan.vars().contains(v),
        _ => false,
    }
}

pub(super) fn integer(value: u64) -> Term {
    Literal::new_typed_literal(value.to_string(), xsd::INTEGER).into()
}

/// An `xsd:integer` result as an inline id where it fits, else as a term.
pub(super) fn integer_agg(value: i64) -> Agg {
    match TermId::inline_integer(value) {
        Some(id) => Agg::Id(id.raw()),
        None => Agg::Term(Literal::new_typed_literal(value.to_string(), xsd::INTEGER).into()),
    }
}

/// What one pass keeps of a group's values for one aggregate.
#[derive(Clone, Copy, Default)]
struct Running {
    /// Bound values.
    count: u64,
    sum: i64,
    overflow: bool,
    unbound: bool,
    /// The smallest and largest value, with its id.
    min: Option<(i64, u64)>,
    max: Option<(i64, u64)>,
}

impl Running {
    fn add(&mut self, id: u64) {
        if id == UNDEF {
            self.unbound = true;
            return;
        }
        self.count += 1;
        let Some(value) = TermId::from_raw(id).as_inline_integer() else {
            return;
        };
        match self.sum.checked_add(value) {
            Some(sum) => self.sum = sum,
            None => self.overflow = true,
        }
        if self.min.is_none_or(|(m, _)| value < m) {
            self.min = Some((value, id));
        }
        if self.max.is_none_or(|(m, _)| value > m) {
            self.max = Some((value, id));
        }
    }
}

/// Every aggregate of every group in one pass over the rows, where each is `COUNT(*)`,
/// or `COUNT`, `SUM`, `AVG`, `MIN` or `MAX` (without DISTINCT) of a variable holding only
/// inline integers (`COUNT`: any terms): the results [`Aggregator::aggregate_ids`] gives,
/// without a list of rows per group. `None` for other aggregates. DBpedia q12 summed 1 M
/// goals into 35 k teams.
/// Once it applies, and before it allocates, it calls `reserve` with what it holds at most
/// ([`pass_bytes`]); a refusal ends it.
pub(super) fn aggregate_in_one_pass(
    solutions: &Solutions,
    group_of: &[u32],
    groups: usize,
    aggregates: &[(Variable, AggregateExpression)],
    reserve: &mut dyn FnMut(usize) -> Result<(), BudgetExceeded>,
) -> Result<Option<Vec<Vec<Agg>>>, BudgetExceeded> {
    let Some(plan) = one_pass_plan(solutions, aggregates) else {
        return Ok(None);
    };
    reserve(pass_bytes(groups, plan.len()))?;
    let mut running = vec![Running::default(); groups * plan.len()];
    let mut rows = vec![0u64; groups];
    for (row, &group) in group_of.iter().enumerate() {
        let group = group as usize;
        rows[group] += 1;
        for (a, (column, _)) in plan.iter().enumerate() {
            if let Some(column) = column {
                running[group * plan.len() + a].add(solutions.table.get(row, *column));
            }
        }
    }
    Ok(Some(
        (0..groups)
            .map(|group| {
                plan.iter()
                    .enumerate()
                    .map(|(a, (column, name))| {
                        let r = running[group * plan.len() + a];
                        if column.is_none() {
                            return integer_agg(rows[group] as i64);
                        }
                        match name {
                            AggregateFunction::Count => integer_agg(r.count as i64),
                            _ if r.unbound => Agg::Id(UNDEF),
                            AggregateFunction::Min => Agg::Id(r.min.map_or(UNDEF, |m| m.1)),
                            AggregateFunction::Max => Agg::Id(r.max.map_or(UNDEF, |m| m.1)),
                            _ if r.overflow => Agg::Id(UNDEF),
                            AggregateFunction::Sum => integer_agg(r.sum),
                            _ if r.count == 0 => Agg::Term(integer(0)),
                            _ => match Decimal::from(r.sum)
                                .checked_div(Decimal::from(r.count as i64))
                            {
                                Some(avg) => Agg::Term(
                                    Literal::new_typed_literal(avg.to_string(), xsd::DECIMAL)
                                        .into(),
                                ),
                                None => Agg::Id(UNDEF),
                            },
                        }
                    })
                    .collect()
            })
            .collect(),
    ))
}

/// What a pass over every aggregate at once holds per group and aggregate, at most: its
/// running state twice (a parallel pass's parts, then them in one list), the value, and a
/// number's term (its lexical form, at most a decimal's 29 digits with sign and point, and
/// its datatype's IRI).
pub(super) const PASS_BYTES: usize = 2 * STATE_BYTES + std::mem::size_of::<Agg>() + 96;

/// The most a pass's running state per group and aggregate holds ([`Running`], and
/// [`Context::numeric_pass`]'s).
pub(super) const STATE_BYTES: usize = 96;

const _: () = assert!(std::mem::size_of::<Running>() <= STATE_BYTES);

/// [`PASS_BYTES`] for `groups` groups of `aggregates`, and each group's list of them.
pub(super) fn pass_bytes(groups: usize, aggregates: usize) -> usize {
    let per_group = aggregates
        .saturating_mul(PASS_BYTES)
        .saturating_add(std::mem::size_of::<Vec<Agg>>());
    groups.saturating_mul(per_group)
}

/// The groups of `solutions` by `variables` (GROUP BY), and per variable its column among
/// the groups' keys (`None`: unbound in every row). The rows are grouped on the columns the
/// variables name, read where they are, each once: a variable named twice groups as once,
/// and one no row binds is unbound in all of them, which splits no group (§11.2). Without
/// variables, all rows are one group, even none of them (§11.4: an aggregate of no rows
/// has a row); with variables none of them binds, all rows are one group, and no rows none.
pub(super) fn groups_of(
    solutions: &Solutions,
    variables: &[Variable],
) -> (Groups, Vec<Option<usize>>) {
    let (keys, positions) = bound_keys(solutions, variables);
    let rows = solutions.table.len();
    let groups = match keys.is_empty() && !variables.is_empty() {
        true => Groups {
            keys: IdTable::from_rows(0, std::iter::repeat_n(&[][..], usize::from(rows > 0))),
            group_of: vec![0; rows],
        },
        false => group_rows(&solutions.table, &keys),
    };
    (groups, positions)
}

/// The columns of `solutions` that `variables` (GROUP BY) name, each once, in the order
/// first named, and per variable its position among them (`None`: no column, unbound in
/// every row). Rows ordered by these columns are ordered by the variables' values: a
/// variable named again compares equal wherever its first naming does, and one without a
/// column ties everywhere. At most as many as the table's columns, whatever the text.
pub(super) fn bound_keys(
    solutions: &Solutions,
    variables: &[Variable],
) -> (Vec<usize>, Vec<Option<usize>>) {
    let mut keys: Vec<usize> = Vec::new();
    let positions = variables
        .iter()
        .map(|variable| {
            let column = solutions.column(variable)?;
            Some(match keys.iter().position(|&key| key == column) {
                Some(position) => position,
                None => {
                    keys.push(column);
                    keys.len() - 1
                }
            })
        })
        .collect();
    (keys, positions)
}

/// The groups' key columns by variable, of the groups `kept` (all of them if `None`): a
/// copy of the key's column, or unbound for a variable without one ([`groups_of`]). As
/// many as the variables; the caller charges them.
pub(super) fn key_columns(
    groups: &Groups,
    positions: &[Option<usize>],
    kept: Option<&[usize]>,
) -> Vec<Vec<u64>> {
    let rows = kept.map_or(groups.len(), <[usize]>::len);
    positions
        .iter()
        .map(|position| match (position, kept) {
            (None, _) => vec![UNDEF; rows],
            (Some(p), None) => groups.keys.column(*p).to_vec(),
            (Some(p), Some(kept)) => {
                let column = groups.keys.column(*p);
                kept.iter().map(|&g| column[g]).collect()
            }
        })
        .collect()
}

impl Context<'_> {
    /// Each aggregate's column over `groups` groups (`group_of`: each row's), its values
    /// interned ([`Context::id`]); the caller charges the columns. From one pass over every
    /// aggregate where each allows it, what that holds charged first ([`pass_bytes`]);
    /// otherwise an aggregate at a time ([`Self::aggregate_column`]).
    pub(super) fn aggregate_columns(
        &self,
        solutions: &Solutions,
        group_of: &[u32],
        groups: usize,
        aggregates: &[(Variable, AggregateExpression)],
    ) -> NativeResult<Vec<Vec<u64>>> {
        if aggregates.is_empty() {
            return Ok(Vec::new());
        }
        let mut columns = Vec::with_capacity(aggregates.len());
        {
            let mut pass = Reservation::new(&*self.budget);
            let mut reserve = |bytes: usize| pass.charge(bytes);
            let memory = QueryEvaluationError::MemoryLimit;
            // Declared after `pass`: dropped before it.
            let values =
                match aggregate_in_one_pass(solutions, group_of, groups, aggregates, &mut reserve)
                    .map_err(memory)?
                {
                    Some(values) => Some(values),
                    None => self
                        .numeric_pass(solutions, group_of, groups, aggregates, &mut reserve)
                        .map_err(memory)?,
                };
            if let Some(values) = values {
                for a in 0..aggregates.len() {
                    columns.push(self.interned(values.iter().map(|group| &group[a]), groups)?);
                }
                return Ok(columns);
            }
        }
        let mut members: Vec<Vec<usize>> = vec![Vec::new(); groups];
        for (row, &group) in group_of.iter().enumerate() {
            members[group as usize].push(row);
        }
        for aggregate in aggregates {
            columns.push(self.aggregate_column(solutions, &members, aggregate)?);
        }
        Ok(columns)
    }

    /// `aggregate`'s column over the groups `members` lists, its values interned
    /// ([`Context::id`]). An aggregate at a time: its values and what it remembers of them
    /// ([`Aggregator`]) are held alone, freed before the next, however many aggregates the
    /// query names.
    pub(super) fn aggregate_column(
        &self,
        solutions: &Solutions,
        members: &[Vec<usize>],
        aggregate: &(Variable, AggregateExpression),
    ) -> NativeResult<Vec<u64>> {
        let (values, charged) =
            self.aggregate_groups(solutions, members, std::slice::from_ref(aggregate))?;
        let ids = self.interned(values.iter().map(|group| &group[0]), members.len());
        // Interned (or refused), the results are let go, then their charge.
        drop(values);
        self.budget.release(charged);
        ids
    }

    /// The ids of `len` values.
    fn interned<'v>(
        &self,
        values: impl Iterator<Item = &'v Agg>,
        len: usize,
    ) -> NativeResult<Vec<u64>> {
        let mut ids = Vec::with_capacity(len);
        for value in values {
            ids.push(match value {
                Agg::Id(id) => *id,
                Agg::Term(term) => self.id(term)?,
            });
        }
        Ok(ids)
    }
}

/// Per aggregate, the column [`aggregate_in_one_pass`] reads (`None`: the rows) and its
/// function; `None` if one isn't for that pass.
fn one_pass_plan(
    solutions: &Solutions,
    aggregates: &[(Variable, AggregateExpression)],
) -> Option<Vec<(Option<usize>, AggregateFunction)>> {
    // Per aggregate: the column it reads (`None`: the rows) and its function.
    let mut plan: Vec<(Option<usize>, AggregateFunction)> = Vec::new();
    for (_, aggregate) in aggregates {
        match aggregate {
            AggregateExpression::CountSolutions { distinct: false } => {
                plan.push((None, AggregateFunction::Count));
            }
            AggregateExpression::FunctionCall {
                name,
                expr: Expression::Variable(variable),
                distinct: false,
            } => {
                let numeric = matches!(
                    name,
                    AggregateFunction::Sum
                        | AggregateFunction::Avg
                        | AggregateFunction::Min
                        | AggregateFunction::Max
                );
                if !numeric && *name != AggregateFunction::Count {
                    return None;
                }
                let column = solutions.column(variable)?;
                if numeric
                    && !solutions.table.column(column).iter().all(|&id| {
                        id == UNDEF || TermId::from_raw(id).as_inline_integer().is_some()
                    })
                {
                    return None;
                }
                plan.push((Some(column), name.clone()));
            }
            _ => return None,
        }
    }
    Some(plan)
}

/// GROUP_CONCAT (SPARQL 1.1 §18.5.1.7): string literals only (anything else makes the
/// result unbound), joined in row order with the separator, as CONCAT of the values: a
/// simple literal whatever language the values share.
pub fn group_concat(values: &[Term], separator: &str) -> Option<Term> {
    let mut concat = String::with_capacity(group_concat_bytes(values, separator)?);
    for (i, value) in values.iter().enumerate() {
        let Term::Literal(literal) = value else {
            return None;
        };
        if literal.language().is_none() && literal.datatype() != xsd::STRING {
            return None;
        }
        if i > 0 {
            concat.push_str(separator);
        }
        concat.push_str(literal.value());
    }
    Some(Literal::new_simple_literal(concat).into())
}

/// The bytes of [`group_concat`]'s string: the values' and a separator between each two;
/// `None` where it has none (a value that isn't a string literal makes it unbound).
fn group_concat_bytes(values: &[Term], separator: &str) -> Option<usize> {
    let mut text = 0usize;
    for value in values {
        let Term::Literal(literal) = value else {
            return None;
        };
        if literal.language().is_none() && literal.datatype() != xsd::STRING {
            return None;
        }
        text = text.saturating_add(literal.value().len());
    }
    let separators = values
        .len()
        .saturating_sub(1)
        .saturating_mul(separator.len());
    Some(text.saturating_add(separators))
}

pub fn sum(values: &[Term]) -> Option<Term> {
    if let Some(durations) = durations(values) {
        return calendar::sum(&durations);
    }
    let mut total = Numeric::Integer(Integer::from(0));
    for value in values {
        total = total.add(Numeric::of(&Value::of(value))?)?;
    }
    Some(total.term())
}

pub fn average(values: &[Term]) -> Option<Term> {
    if values.is_empty() {
        return Some(integer(0));
    }
    if let Some(durations) = durations(values) {
        return calendar::average(&durations);
    }
    let mut total = Numeric::Integer(Integer::from(0));
    for value in values {
        total = total.add(Numeric::of(&Value::of(value))?)?;
    }
    let count = values.len() as i64;
    Some(match total {
        // SPARQL: the average of integers or decimals is a decimal.
        Numeric::Integer(_) | Numeric::Decimal(_) => {
            Numeric::Decimal(total.decimal()?.checked_div(Decimal::from(count))?).term()
        }
        Numeric::Float(f) => Numeric::Float(f / Float::from(count as f32)).term(),
        Numeric::Double(d) => Numeric::Double(d / Double::from(count as f64)).term(),
    })
}

/// The values, if the first is a year-month or day-time duration (`SUM` and `AVG` of
/// durations, SEP-0002); `None` for numbers.
fn durations(values: &[Term]) -> Option<Vec<Value>> {
    let first = value::Value::of(values.first()?);
    if !calendar::is_summable_duration(&first) {
        return None;
    }
    Some(values.iter().map(value::Value::of).collect())
}
