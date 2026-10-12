//! SPARQL aggregate values and per-group helpers. Group planning, scheduling and
//! result interning remain in the executor; these helpers preserve row order and errors.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use nrese_engine::TermId;
use nrese_exec::UNDEF;
use nrese_rdf::vocab::xsd;
use nrese_rdf::{Literal, Term, Variable};
use nrese_sparql_syntax::algebra::{AggregateExpression, AggregateFunction, Expression};
use nrese_xsd::{Decimal, Double, Float, Integer};

use super::expr::Evaluator;
use super::numeric::Numeric;
use super::value::Value;
use super::{ScanPattern, Solutions, calendar, expression_variables, pushdown, value};

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

    /// The expression's value for each of `rows`. With `distinct`, only for the first row
    /// of each combination of the expression's variables: the repeats can't add a value.
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
        // A value drawn per row (RAND, BNODE, ...) is drawn for every row.
        if pushdown::per_solution(expr) {
            return rows.iter().map(|&row| eval(row)).collect();
        }
        if let [column] = columns[..] {
            // One variable: its ids stand for the rows, and the values are remembered
            // across groups.
            let key = expr as *const Expression as usize;
            let mut seen = HashSet::new();
            let mut memo = self.memo.borrow_mut();
            return rows
                .iter()
                .filter(|&&row| !distinct || seen.insert(table.get(row, column)))
                .map(|&row| {
                    memo.entry((key, table.get(row, column)))
                        .or_insert_with(|| eval(row))
                        .clone()
                })
                .collect();
        }
        if !distinct || columns.is_empty() {
            return rows.iter().map(|&row| eval(row)).collect();
        }
        let mut seen: HashSet<Vec<u64>> = HashSet::new();
        rows.iter()
            .filter(|&&row| seen.insert(columns.iter().map(|&c| table.get(row, c)).collect()))
            .map(|&row| eval(row))
            .collect()
    }

    pub(super) fn aggregate(
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
                        group_concat(&values, separator.as_deref().unwrap_or(" "))
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
pub(super) fn aggregate_in_one_pass(
    solutions: &Solutions,
    group_of: &[u32],
    groups: usize,
    aggregates: &[(Variable, AggregateExpression)],
) -> Option<Vec<Vec<Agg>>> {
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
    Some(
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
    )
}

/// GROUP_CONCAT (SPARQL 1.1 §18.5.1.7): string literals only (anything else makes the
/// result unbound), joined in row order with the separator, as CONCAT of the values: a
/// simple literal whatever language the values share.
pub fn group_concat(values: &[Term], separator: &str) -> Option<Term> {
    let mut concat = String::new();
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
