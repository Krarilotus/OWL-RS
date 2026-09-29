//! The native query executor (execution-core design, XC3): SPARQL algebra evaluated over id
//! tables with the shared execution core (`nrese-exec`, decision D11).
//!
//! **Whole-query switch.** [`evaluate`] returns `None` unless every operator of the query is
//! supported ([`supported`]); the caller then runs spareval exactly as before. A few rare
//! semantic corners are only detectable at runtime (UNDEF in MINUS or NOT EXISTS keys); they
//! also hand the query back to spareval ([`NativeError::Fallback`]). So native coverage can
//! grow without ever changing results.
//!
//! **Execution.** Intermediate results are [`IdTable`]s of term ids; terms are decoded only
//! for expressions and for the output. BGPs are ordered greedily by *exact* pattern counts
//! (`Snapshot::count`), joined by index nested loops when the running result is much
//! smaller than the next pattern, and otherwise by merge joins on sorted scans or hash joins.
//! `COUNT(*)` over a single pattern reads the count from the index.

mod expr;
mod fast;
mod paths;
mod plan;
mod ranges;
mod value;
mod wcoj;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Instant;

use nrese_engine::quad::Permutation;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::join::{
    anti_join, join, join_keeping_left_order, join_with_undef, left_join, semi_join,
};
use nrese_exec::{
    Budget, BudgetExceeded, IdTable, UNDEF, computed_id, computed_index, group::group_rows,
};
use oxrdf::vocab::xsd;
use oxrdf::{Literal, Term, Variable};
use oxsdatatypes::{Decimal, Double, Float, Integer};
use rayon::prelude::*;
use spareval::{
    CancellationToken, QueryEvaluationError, QueryResults, QuerySolutionIter, QueryTripleIter,
};
use spargebra::Query;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern, OrderExpression,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

use crate::query::{PlanStep, QueryOptions};
use expr::Evaluator;
use value::Value;

/// A right side at least this many times larger than the running result is joined by
/// probing the index once per result row instead of scanning it.
const PROBE_FACTOR: u64 = 32;

/// Result rows per parallel task of an index nested-loop join; smaller results probe on
/// one thread.
const PROBE_CHUNK: usize = 4096;

/// Decoded terms kept per query for repeated ids (ORDER BY, aggregates, expressions).
const DECODE_CACHE_ENTRIES: usize = 1 << 18;

pub(crate) enum NativeError {
    /// Not handled natively after all: run the query on spareval.
    Fallback,
    Evaluation(QueryEvaluationError),
}

impl From<QueryEvaluationError> for NativeError {
    fn from(error: QueryEvaluationError) -> Self {
        Self::Evaluation(error)
    }
}

type NativeResult<T> = Result<T, NativeError>;

/// Runs `query` natively if it is fully supported; `None` means "use spareval".
pub(crate) fn evaluate<'a>(
    snapshot: &'a Snapshot,
    query: &Query,
    options: &QueryOptions,
) -> Option<Result<QueryResults<'a>, QueryEvaluationError>> {
    let (pattern, form) = native_pattern(query, options)?;
    let ctx = Context::new(snapshot, options);
    let solutions = match ctx.eval(pattern) {
        Ok(solutions) => solutions,
        Err(NativeError::Fallback) => return None,
        Err(NativeError::Evaluation(error)) => return Some(Err(error)),
    };
    match form {
        Form::Select => {}
        Form::Ask => return Some(Ok(QueryResults::Boolean(!solutions.table.is_empty()))),
        Form::Construct(template) => {
            let Context {
                snapshot, computed, ..
            } = ctx;
            let triples = construct(snapshot, computed.into_inner(), solutions, template);
            return Some(Ok(QueryResults::Graph(QueryTripleIter::new(
                triples.map(Ok),
            ))));
        }
    }
    let variables: Arc<[Variable]> = solutions.vars.clone().into();
    let Context {
        snapshot, computed, ..
    } = ctx;
    let computed = computed.into_inner();
    let table = solutions.table;
    let rows = (0..table.len()).map(move |row| {
        Ok((0..table.width())
            .map(|column| decode(snapshot, &computed, table.get(row, column)))
            .collect::<Vec<_>>())
    });
    Some(Ok(QueryResults::Solutions(QuerySolutionIter::from_tuples(
        variables, rows,
    ))))
}

/// Runs `query` natively, recording each operator ([`PlanStep`]); returns the steps and
/// the number of solutions. `None` means "use spareval", as for [`evaluate`].
pub(crate) fn explain(
    snapshot: &Snapshot,
    query: &Query,
    options: &QueryOptions,
) -> Option<Result<(Vec<PlanStep>, u64), QueryEvaluationError>> {
    let (pattern, _) = native_pattern(query, options)?;
    let mut ctx = Context::new(snapshot, options);
    ctx.trace = Some(RefCell::default());
    match ctx.eval(pattern) {
        Ok(solutions) => Some(Ok((
            ctx.trace.take().unwrap_or_default().into_inner(),
            solutions.table.len() as u64,
        ))),
        Err(NativeError::Fallback) => None,
        Err(NativeError::Evaluation(error)) => Some(Err(error)),
    }
}

/// What a query returns.
enum Form<'q> {
    Select,
    Ask,
    /// The triples of the template, per solution.
    Construct(&'q [TriplePattern]),
}

/// The pattern of a query the native executor runs, and the query form.
fn native_pattern<'q>(
    query: &'q Query,
    options: &QueryOptions,
) -> Option<(&'q GraphPattern, Form<'q>)> {
    if options.dataset.is_some() || !query_supported(query) {
        return None;
    }
    match query {
        Query::Select { pattern, .. } => Some((pattern, Form::Select)),
        Query::Ask { pattern, .. } => Some((pattern, Form::Ask)),
        Query::Construct {
            template, pattern, ..
        } => Some((pattern, Form::Construct(template))),
        Query::Describe { .. } => None,
    }
}

/// A position of a CONSTRUCT template, resolved against the solution columns.
enum TemplateTerm {
    Constant(Term),
    Column(usize),
    /// A variable the solutions don't bind: the template triple never applies.
    Unbound,
    /// The template's n-th blank node label: a fresh blank node per solution.
    Fresh(usize),
}

/// The triples of `template` instantiated with every solution, as spareval produces them:
/// template blank nodes are fresh per solution, triples with an unbound or ill-placed term
/// (a literal subject, a non-IRI predicate) are skipped, and repeated triples without blank
/// nodes are emitted once (the memory of emitted triples is bounded, as in spareval).
fn construct<'a>(
    snapshot: &'a Snapshot,
    computed: Vec<Term>,
    solutions: Solutions,
    template: &[TriplePattern],
) -> impl Iterator<Item = oxrdf::Triple> + 'a {
    let mut labels: Vec<String> = Vec::new();
    let mut resolve = |term: &TermPattern| match term {
        TermPattern::NamedNode(n) => TemplateTerm::Constant(n.clone().into()),
        TermPattern::Literal(l) => TemplateTerm::Constant(l.clone().into()),
        TermPattern::BlankNode(b) => {
            let label = b.as_str().to_owned();
            let index = labels.iter().position(|l| *l == label).unwrap_or_else(|| {
                labels.push(label);
                labels.len() - 1
            });
            TemplateTerm::Fresh(index)
        }
        TermPattern::Variable(v) => solutions
            .column(v)
            .map_or(TemplateTerm::Unbound, TemplateTerm::Column),
        #[allow(unreachable_patterns)]
        _ => TemplateTerm::Unbound,
    };
    let resolved: Vec<[TemplateTerm; 3]> = template
        .iter()
        .map(|t| {
            let predicate = match &t.predicate {
                NamedNodePattern::NamedNode(n) => TemplateTerm::Constant(n.clone().into()),
                NamedNodePattern::Variable(v) => solutions
                    .column(v)
                    .map_or(TemplateTerm::Unbound, TemplateTerm::Column),
            };
            [resolve(&t.subject), predicate, resolve(&t.object)]
        })
        .collect();
    let fresh_count = labels.len();
    let table = solutions.table;
    let mut emitted: HashSet<oxrdf::Triple> = HashSet::new();
    let mut buffer: Vec<oxrdf::Triple> = Vec::new();
    let mut row = 0;
    std::iter::from_fn(move || {
        loop {
            if let Some(triple) = buffer.pop() {
                return Some(triple);
            }
            if row >= table.len() {
                return None;
            }
            let fresh: Vec<oxrdf::BlankNode> = (0..fresh_count)
                .map(|_| oxrdf::BlankNode::default())
                .collect();
            let value = |term: &TemplateTerm| -> Option<Term> {
                match term {
                    TemplateTerm::Constant(term) => Some(term.clone()),
                    TemplateTerm::Column(c) => decode(snapshot, &computed, table.get(row, *c)),
                    TemplateTerm::Unbound => None,
                    TemplateTerm::Fresh(i) => Some(fresh[*i].clone().into()),
                }
            };
            for [s, p, o] in &resolved {
                let subject = match value(s) {
                    Some(Term::NamedNode(n)) => oxrdf::NamedOrBlankNode::from(n),
                    Some(Term::BlankNode(b)) => oxrdf::NamedOrBlankNode::from(b),
                    _ => continue,
                };
                let Some(Term::NamedNode(predicate)) = value(p) else {
                    continue;
                };
                let Some(object) = value(o) else {
                    continue;
                };
                let triple = oxrdf::Triple::new(subject, predicate, object);
                let new = triple.subject.is_blank_node()
                    || triple.object.is_blank_node()
                    || emitted.insert(triple.clone());
                if new {
                    buffer.push(triple);
                    if emitted.len() > 1024 * 1024 {
                        emitted.clear();
                    }
                }
            }
            buffer.reverse();
            row += 1;
        }
    })
}

fn decode(snapshot: &Snapshot, computed: &[Term], id: u64) -> Option<Term> {
    if id == UNDEF {
        return None;
    }
    match computed_index(id) {
        Some(index) => computed.get(index as usize).cloned(),
        None => snapshot.decode(TermId::from_raw(id)),
    }
}

/// True if [`evaluate`] handles `query` (barring runtime fallbacks), with no protocol dataset.
pub(crate) fn query_supported(query: &Query) -> bool {
    match query {
        Query::Select {
            dataset: None,
            pattern,
            ..
        }
        | Query::Ask {
            dataset: None,
            pattern,
            ..
        } => supported(pattern),
        Query::Construct {
            dataset: None,
            template,
            pattern,
            ..
        } => supported(pattern) && template.iter().all(supported_triple),
        _ => false,
    }
}

/// True if the native executor supports every operator in `pattern`.
pub(crate) fn supported(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => patterns.iter().all(supported_triple),
        GraphPattern::Graph { inner, .. } => supported(inner) && scans_everywhere(inner),
        GraphPattern::Path {
            subject, object, ..
        } => supported_term(subject) && supported_term(object),
        GraphPattern::Join { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => supported(left) && supported(right),
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => supported(left) && supported(right) && expression.as_ref().is_none_or(expr::supported),
        GraphPattern::Filter { expr, inner } => supported(inner) && supported_filter(expr),
        GraphPattern::Extend {
            inner, expression, ..
        } => supported(inner) && expr::supported(expression),
        GraphPattern::Values { bindings, .. } => bindings
            .iter()
            .flatten()
            .flatten()
            .all(|term| matches!(term, GroundTerm::NamedNode(_) | GroundTerm::Literal(_))),
        GraphPattern::OrderBy { inner, expression } => {
            supported(inner)
                && expression.iter().all(|e| match e {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => expr::supported(e),
                })
        }
        GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => supported(inner),
        GraphPattern::Group {
            inner, aggregates, ..
        } => {
            supported(inner)
                && aggregates.iter().all(|(_, aggregate)| match aggregate {
                    AggregateExpression::CountSolutions { .. } => true,
                    AggregateExpression::FunctionCall { name, expr, .. } => {
                        expr::supported(expr)
                            && matches!(
                                name,
                                AggregateFunction::Count
                                    | AggregateFunction::Sum
                                    | AggregateFunction::Avg
                                    | AggregateFunction::Min
                                    | AggregateFunction::Max
                                    | AggregateFunction::Sample
                            )
                    }
                })
        }
        _ => false,
    }
}

/// True if every solution of `pattern` comes from triple patterns in every branch, so inside
/// `GRAPH ?g` each row binds `?g` from a scan (and inside `GRAPH <g>` each row needs a
/// match in `<g>`). Patterns that can produce rows without a scan (VALUES, empty groups,
/// aggregates, BIND alone, nested GRAPH, property paths) and subqueries stay on spareval
/// there.
fn scans_everywhere(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => !patterns.is_empty(),
        GraphPattern::Join { left, right } => scans_everywhere(left) || scans_everywhere(right),
        GraphPattern::Union { left, right } => scans_everywhere(left) && scans_everywhere(right),
        GraphPattern::LeftJoin { left, .. } | GraphPattern::Minus { left, .. } => {
            scans_everywhere(left)
        }
        GraphPattern::Filter { inner, .. } | GraphPattern::Extend { inner, .. } => {
            scans_everywhere(inner)
        }
        // A subquery (projection and its modifiers) scopes its variables: its own `?g` is
        // not the graph variable.
        _ => false,
    }
}

fn supported_term(term: &TermPattern) -> bool {
    matches!(
        term,
        TermPattern::NamedNode(_)
            | TermPattern::BlankNode(_)
            | TermPattern::Literal(_)
            | TermPattern::Variable(_)
    )
}

fn supported_triple(triple: &TriplePattern) -> bool {
    let term = |t: &TermPattern| {
        matches!(
            t,
            TermPattern::NamedNode(_)
                | TermPattern::BlankNode(_)
                | TermPattern::Literal(_)
                | TermPattern::Variable(_)
        )
    };
    term(&triple.subject) && term(&triple.object)
}

/// FILTER expressions: the supported expression language, plus (NOT) EXISTS over a supported
/// pattern that reads no variable from outside except through the shared ones (an
/// uncorrelated sub-pattern, which is a semi- or anti-join).
fn supported_filter(expression: &Expression) -> bool {
    match expression {
        // FILTER(A && B) is FILTER(A) then FILTER(B): an error in either drops the row.
        Expression::And(a, b) => supported_filter(a) && supported_filter(b),
        Expression::Exists(pattern) => uncorrelated(pattern),
        Expression::Not(inner) if matches!(**inner, Expression::Exists(_)) => {
            supported_filter(inner)
        }
        other => expr::supported(other),
    }
}

/// True if a conjunct of `expression` is an (NOT) EXISTS, which needs a join.
fn contains_exists(expression: &Expression) -> bool {
    match expression {
        Expression::And(a, b) => contains_exists(a) || contains_exists(b),
        Expression::Exists(_) => true,
        Expression::Not(inner) => matches!(**inner, Expression::Exists(_)),
        _ => false,
    }
}

fn uncorrelated(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => patterns.iter().all(supported_triple),
        GraphPattern::Join { left, right } => uncorrelated(left) && uncorrelated(right),
        GraphPattern::Filter { expr, inner } => {
            let mut bound = Vec::new();
            bound_variables(inner, &mut bound);
            expr::supported(expr)
                && uncorrelated(inner)
                && expression_variables(expr).iter().all(|v| bound.contains(v))
        }
        _ => false,
    }
}

fn bound_variables(pattern: &GraphPattern, out: &mut Vec<Variable>) {
    pattern.on_in_scope_variable(|v| {
        if !out.contains(v) {
            out.push(v.clone());
        }
    });
}

fn expression_variables(expression: &Expression) -> Vec<Variable> {
    let mut out = Vec::new();
    fn walk(e: &Expression, out: &mut Vec<Variable>) {
        match e {
            Expression::Variable(v) | Expression::Bound(v) => out.push(v.clone()),
            Expression::Or(a, b)
            | Expression::And(a, b)
            | Expression::Equal(a, b)
            | Expression::SameTerm(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b)
            | Expression::Add(a, b)
            | Expression::Subtract(a, b)
            | Expression::Multiply(a, b)
            | Expression::Divide(a, b) => {
                walk(a, out);
                walk(b, out);
            }
            Expression::In(a, list) => {
                walk(a, out);
                list.iter().for_each(|e| walk(e, out));
            }
            Expression::Not(a) | Expression::UnaryPlus(a) | Expression::UnaryMinus(a) => {
                walk(a, out)
            }
            Expression::If(a, b, c) => {
                walk(a, out);
                walk(b, out);
                walk(c, out);
            }
            Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
                list.iter().for_each(|e| walk(e, out));
            }
            _ => {}
        }
    }
    walk(expression, &mut out);
    out
}

/// Solutions: a table whose column `i` binds variable `vars[i]`. `ordered` marks a row
/// order that matters (from ORDER BY): operators then keep it, as spareval does, so a
/// sorted subquery stays sorted through the joins above it.
struct Solutions {
    vars: Vec<Variable>,
    table: IdTable,
    ordered: bool,
}

impl Solutions {
    fn column(&self, variable: &Variable) -> Option<usize> {
        self.vars.iter().position(|v| v == variable)
    }

    fn unit() -> Self {
        Self {
            vars: Vec::new(),
            table: IdTable::from_rows(0, [&[][..]]),
            ordered: false,
        }
    }
}

struct Context<'a> {
    snapshot: &'a Snapshot,
    /// Which statements the query reads (asserted, inferred or both).
    model: ReadModel,
    evaluator: Evaluator,
    computed: RefCell<Vec<Term>>,
    computed_ids: RefCell<HashMap<Term, u64>>,
    decoded: RefCell<HashMap<u64, Option<Term>>>,
    cancellation: Option<CancellationToken>,
    budget: Budget,
    /// The operators run so far, for EXPLAIN; `None` when not explaining.
    trace: Option<RefCell<Vec<PlanStep>>>,
    /// Nesting depth of the operator being evaluated (for the trace).
    depth: Cell<usize>,
    /// The graph that triple patterns match in (`GRAPH`).
    graph: RefCell<GraphScope>,
}

/// The active graph of triple patterns.
#[derive(Clone, Debug)]
enum GraphScope {
    Default,
    Named(TermId),
    /// `GRAPH ?g`: any named graph, bound to the variable.
    Variable(Variable),
    /// `GRAPH <g>` for a graph the store doesn't know: nothing matches.
    Missing,
}

impl<'a> Context<'a> {
    fn new(snapshot: &'a Snapshot, options: &QueryOptions) -> Self {
        Self {
            snapshot,
            model: options.read_model,
            evaluator: Evaluator::default(),
            computed: RefCell::default(),
            computed_ids: RefCell::default(),
            decoded: RefCell::default(),
            cancellation: options.cancellation.clone(),
            budget: options
                .memory_limit
                .map_or_else(Budget::unlimited, Budget::new),
            trace: None,
            depth: Cell::new(0),
            graph: RefCell::new(GraphScope::Default),
        }
    }

    /// Records an operator that ran below the current one (EXPLAIN only).
    fn note(
        &self,
        operator: &str,
        detail: String,
        estimated_rows: Option<u64>,
        rows: usize,
        start: Instant,
    ) {
        if let Some(trace) = &self.trace {
            trace.borrow_mut().push(PlanStep {
                depth: self.depth.get(),
                operator: operator.to_owned(),
                detail,
                estimated_rows,
                rows: rows as u64,
                micros: start.elapsed().as_micros() as u64,
            });
        }
    }

    fn check(&self) -> NativeResult<()> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(QueryEvaluationError::Cancelled.into());
        }
        Ok(())
    }

    /// Charges a produced table against the query's memory budget.
    fn produced(&self, solutions: Solutions) -> NativeResult<Solutions> {
        self.check()?;
        self.budget
            .charge(solutions.table.memory_bytes())
            .map_err(|e| QueryEvaluationError::Dataset(Box::new(e)))?;
        Ok(solutions)
    }

    /// The most rows a table `width` columns wide may have within the remaining budget.
    fn max_rows(&self, width: usize) -> usize {
        self.budget.remaining() / (width.max(1) * 8)
    }

    /// The error for an operator that would outgrow the budget.
    fn too_large(&self, rows: usize, width: usize) -> NativeError {
        let requested = rows.saturating_mul(width.max(1) * 8);
        QueryEvaluationError::Dataset(Box::new(BudgetExceeded {
            limit: self.budget.used().saturating_add(self.budget.remaining()),
            requested,
            used: self.budget.used(),
        }))
        .into()
    }

    fn consumed(&self, solutions: &Solutions) {
        self.budget.release(solutions.table.memory_bytes());
    }

    fn term(&self, id: u64) -> Option<Term> {
        if id == UNDEF {
            return None;
        }
        if let Some(index) = computed_index(id) {
            return self.computed.borrow().get(index as usize).cloned();
        }
        let mut decoded = self.decoded.borrow_mut();
        if let Some(term) = decoded.get(&id) {
            return term.clone();
        }
        let term = self.snapshot.decode(TermId::from_raw(id));
        // Bounded: a scan over millions of distinct labels must not keep them all.
        if decoded.len() < DECODE_CACHE_ENTRIES {
            decoded.insert(id, term.clone());
        }
        term
    }

    /// The id of `term`: its stored id if the snapshot knows it, else a computed id, the same
    /// one for equal terms.
    fn id(&self, term: &Term) -> u64 {
        if let Some(id) = self.snapshot.lookup(term.as_ref()) {
            return id.raw();
        }
        let mut ids = self.computed_ids.borrow_mut();
        if let Some(&id) = ids.get(term) {
            return id;
        }
        let mut computed = self.computed.borrow_mut();
        let id = computed_id(computed.len() as u64);
        computed.push(term.clone());
        ids.insert(term.clone(), id);
        id
    }

    fn binding<'s>(
        &'s self,
        solutions: &'s Solutions,
        row: usize,
    ) -> impl Fn(&Variable) -> Option<Term> + 's {
        move |variable| {
            let column = solutions.column(variable)?;
            self.term(solutions.table.get(row, column))
        }
    }

    fn eval(&self, pattern: &GraphPattern) -> NativeResult<Solutions> {
        let Some(trace) = &self.trace else {
            return self.eval_operator(pattern);
        };
        let depth = self.depth.get();
        let index = {
            let mut trace = trace.borrow_mut();
            let (operator, detail) = describe(pattern);
            trace.push(PlanStep {
                depth,
                operator: operator.to_owned(),
                detail,
                estimated_rows: None,
                rows: 0,
                micros: 0,
            });
            trace.len() - 1
        };
        self.depth.set(depth + 1);
        let start = Instant::now();
        let result = self.eval_operator(pattern);
        self.depth.set(depth);
        if let Ok(solutions) = &result {
            let step = &mut trace.borrow_mut()[index];
            step.rows = solutions.table.len() as u64;
            step.micros = start.elapsed().as_micros() as u64;
        }
        result
    }

    fn eval_operator(&self, pattern: &GraphPattern) -> NativeResult<Solutions> {
        self.check()?;
        match pattern {
            GraphPattern::Bgp { patterns } => self.bgp(patterns, &[]),
            GraphPattern::Graph { name, inner } => {
                let scope = match name {
                    NamedNodePattern::NamedNode(n) => self
                        .snapshot
                        .lookup(n.as_ref().into())
                        .map_or(GraphScope::Missing, GraphScope::Named),
                    NamedNodePattern::Variable(v) => GraphScope::Variable(v.clone()),
                };
                let outer = self.graph.replace(scope);
                let result = self.eval(inner);
                *self.graph.borrow_mut() = outer;
                result
            }
            GraphPattern::Path { .. } if !matches!(*self.graph.borrow(), GraphScope::Default) => {
                Err(NativeError::Fallback)
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => self.path(subject, path, object),
            GraphPattern::Join { left, right } => {
                let (left, right) = (self.eval(left)?, self.eval(right)?);
                self.join(left, right)
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                let (left, right) = (self.eval(left)?, self.eval(right)?);
                self.left_join(left, right, expression.as_ref())
            }
            GraphPattern::Filter { expr, inner } => {
                // Range conjuncts on a variable narrow the scan that binds it; the full
                // FILTER still runs on every row, so the ranges only prune.
                let solutions = match &**inner {
                    GraphPattern::Bgp { patterns } => {
                        self.bgp(patterns, &ranges::hints(expr, self.snapshot))?
                    }
                    other => self.eval(other)?,
                };
                self.filter(solutions, expr)
            }
            GraphPattern::Union { left, right } => {
                let (left, right) = (self.eval(left)?, self.eval(right)?);
                self.union(left, right)
            }
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                let mut solutions = self.eval(inner)?;
                let values: Vec<u64> = (0..solutions.table.len())
                    .map(|row| {
                        self.evaluator
                            .eval(expression, &self.binding(&solutions, row))
                            .map_or(UNDEF, |term| self.id(&term))
                    })
                    .collect();
                let mut columns = std::mem::take(&mut solutions.table).into_columns();
                columns.push(values);
                solutions.vars.push(variable.clone());
                solutions.table = IdTable::from_columns(columns);
                self.produced(solutions)
            }
            GraphPattern::Minus { left, right } => {
                let (left, right) = (self.eval(left)?, self.eval(right)?);
                let (lk, rk) = shared_columns(&left, &right);
                if lk.is_empty() {
                    return Ok(left);
                }
                if has_undef(&left.table, &lk) || has_undef(&right.table, &rk) {
                    return Err(NativeError::Fallback);
                }
                let table = anti_join(&left.table, &right.table, &lk, &rk);
                self.consumed(&right);
                self.produced(Solutions {
                    vars: left.vars,
                    table,
                    ordered: left.ordered,
                })
            }
            GraphPattern::Values {
                variables,
                bindings,
            } => {
                let mut table = IdTable::new(variables.len());
                for binding in bindings {
                    let row: Vec<u64> = binding
                        .iter()
                        .map(|term| match term {
                            Some(GroundTerm::NamedNode(n)) => self.id(&n.clone().into()),
                            Some(GroundTerm::Literal(l)) => self.id(&l.clone().into()),
                            _ => UNDEF,
                        })
                        .collect();
                    table.push_row(&row);
                }
                self.produced(Solutions {
                    vars: variables.clone(),
                    table,
                    ordered: false,
                })
            }
            GraphPattern::OrderBy { inner, expression } => {
                let solutions = self.eval(inner)?;
                self.order_by(solutions, expression, None)
            }
            GraphPattern::Project { inner, variables } => {
                let solutions = self.eval(inner)?;
                Ok(self.project(solutions, variables))
            }
            GraphPattern::Distinct { inner } => {
                // SELECT DISTINCT … ORDER BY projected variables: deduplicate first, then sort
                // the (usually far fewer) distinct rows: the same rows, in an order that
                // ORDER BY permits (ties may come out differently, which SPARQL allows).
                if let GraphPattern::Project {
                    inner: projected,
                    variables,
                } = &**inner
                    && let GraphPattern::OrderBy {
                        inner: sorted,
                        expression,
                    } = &**projected
                    && expression.iter().all(|key| {
                        let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = key;
                        matches!(e, Expression::Variable(v) if variables.contains(v))
                    })
                {
                    let solutions = self.eval(sorted)?;
                    let mut solutions = self.project(solutions, variables);
                    solutions.table.dedup_preserving_order();
                    return self.order_by(solutions, expression, None);
                }
                let mut solutions = self.eval(inner)?;
                solutions.table.dedup_preserving_order();
                Ok(solutions)
            }
            GraphPattern::Reduced { inner } => self.eval(inner),
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => {
                // LIMIT over one (filtered, projected) pattern: stream the scan, stop early.
                if let Some(length) = length
                    && let Some(solutions) = self.limited_scan(inner, start + length)?
                {
                    let mut solutions = solutions;
                    solutions.table.slice(*start, Some(*length));
                    return Ok(solutions);
                }
                // ORDER BY + LIMIT: only the first start + length rows need a full order.
                if let GraphPattern::OrderBy {
                    inner: sorted,
                    expression,
                } = &**inner
                {
                    let solutions = self.eval(sorted)?;
                    let mut solutions =
                        self.order_by(solutions, expression, length.map(|l| start + l))?;
                    solutions.table.slice(*start, *length);
                    return Ok(solutions);
                }
                if let GraphPattern::Project {
                    inner: projected,
                    variables,
                } = &**inner
                    && let GraphPattern::OrderBy {
                        inner: sorted,
                        expression,
                    } = &**projected
                {
                    let solutions = self.eval(sorted)?;
                    let solutions =
                        self.order_by(solutions, expression, length.map(|l| start + l))?;
                    let mut solutions = self.project(solutions, variables);
                    solutions.table.slice(*start, *length);
                    return Ok(solutions);
                }
                let mut solutions = self.eval(inner)?;
                solutions.table.slice(*start, *length);
                Ok(solutions)
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => self.group(inner, variables, aggregates),
            _ => Err(NativeError::Fallback),
        }
    }

    // --- property paths ------------------------------------------------------------------

    fn path(
        &self,
        subject: &TermPattern,
        path: &spargebra::algebra::PropertyPathExpression,
        object: &TermPattern,
    ) -> NativeResult<Solutions> {
        let resolved = paths::Path::resolve(path, self.snapshot);
        let evaluator = paths::PathEvaluator {
            snapshot: self.snapshot,
            model: self.model,
        };
        // A variable (blank nodes are variables here), or a constant's id (computed if the
        // store doesn't know it; such an id has no edges).
        let end = |term: &TermPattern| -> Result<Variable, u64> {
            match term {
                TermPattern::Variable(v) => Ok(v.clone()),
                TermPattern::BlankNode(b) => {
                    Ok(Variable::new_unchecked(format!("_bnode_{}", b.as_str())))
                }
                TermPattern::NamedNode(n) => Err(self.id(&n.clone().into())),
                TermPattern::Literal(l) => Err(self.id(&l.clone().into())),
                #[allow(unreachable_patterns)]
                _ => Err(UNDEF),
            }
        };
        let column = |values: Vec<u64>| IdTable::from_columns(vec![values]);
        let (vars, table) = match (end(subject), end(object)) {
            (Err(start), Err(finish)) => {
                let rows = usize::from(evaluator.connects(&resolved, start, finish));
                (
                    Vec::new(),
                    IdTable::from_rows(0, std::iter::repeat_n(&[][..], rows)),
                )
            }
            (Err(start), Ok(o)) => (vec![o], column(evaluator.from(&resolved, start))),
            (Ok(s), Err(finish)) => (vec![s], column(evaluator.to(&resolved, finish))),
            (Ok(s), Ok(o)) if s == o => {
                let same: Vec<u64> = evaluator
                    .open(&resolved)
                    .into_iter()
                    .filter(|(a, b)| a == b)
                    .map(|(a, _)| a)
                    .collect();
                (vec![s], column(same))
            }
            (Ok(s), Ok(o)) => {
                let (starts, ends): (Vec<u64>, Vec<u64>) =
                    evaluator.open(&resolved).into_iter().unzip();
                (vec![s, o], IdTable::from_columns(vec![starts, ends]))
            }
        };
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }

    // --- basic graph patterns ------------------------------------------------------------

    fn bgp(&self, triples: &[TriplePattern], hints: &[ranges::Hint]) -> NativeResult<Solutions> {
        let mut scans = Vec::with_capacity(triples.len());
        for triple in triples {
            match self.scan_pattern(triple) {
                Some(scan) => scans.push(scan),
                // A constant the store doesn't know: nothing can match.
                None => {
                    let mut vars = Vec::new();
                    for triple in triples {
                        for v in triple_variables(triple) {
                            if !vars.contains(&v) {
                                vars.push(v);
                            }
                        }
                    }
                    let width = vars.len();
                    return Ok(Solutions {
                        vars,
                        table: IdTable::new(width),
                        ordered: false,
                    });
                }
            }
        }
        if scans.is_empty() {
            return Ok(Solutions::unit());
        }
        // Each pattern's id ranges, if a hint narrows its object and the object comes first
        // after the bound prefix in the permutation that sorts on it.
        let ranged: Vec<Option<RangedScan<'_>>> = scans
            .iter()
            .map(|s| {
                let Slot::Var(object) = &s.slots[2] else {
                    return None;
                };
                if !s.in_default_graph() {
                    return None;
                }
                let hint = hints.iter().find(|h| &h.variable == object)?;
                let permutation = s.permutation_for(Some(object));
                (s.first_free(permutation) == Some(2) && !s.repeats_variable())
                    .then_some((permutation, hint.ranges.as_slice()))
            })
            .collect();
        let counts: Vec<u64> = scans
            .iter()
            .zip(&ranged)
            .map(|(s, ranged)| match ranged {
                Some((permutation, ranges)) => ranges
                    .iter()
                    .filter_map(|&(low, high)| {
                        self.snapshot.count_range_in(
                            self.model,
                            &s.quad_pattern(),
                            *permutation,
                            low,
                            high,
                        )
                    })
                    .sum(),
                None => self.snapshot.count_in(self.model, &s.quad_pattern()),
            })
            .collect();
        let start = Instant::now();
        if scans.len() >= 3
            && ranged.iter().all(Option::is_none)
            && scans.iter().all(ScanPattern::in_default_graph)
            && let Some(solutions) = self.cyclic_bgp(&scans, &counts)?
        {
            if self.trace.is_some() {
                let detail = triples.iter().map(ToString::to_string).collect::<Vec<_>>();
                let rows = solutions.table.len();
                self.note("wcoj", detail.join(" . "), None, rows, start);
            }
            return Ok(solutions);
        }
        let plan = self.join_order(&scans, &counts);
        let estimate = |step: usize| {
            let rows = plan.rows[step];
            rows.is_finite().then(|| rows.round() as u64)
        };
        let order = &plan.order;
        let first = order[0];
        let join_var = order.get(1).and_then(|&i| {
            scans[first]
                .vars()
                .into_iter()
                .find(|v| scans[i].vars().contains(v))
        });
        let mut result = match ranged[first] {
            Some((permutation, ranges)) => self.scan_ranges(&scans[first], permutation, ranges)?,
            None => self.scan(&scans[first], join_var.as_ref())?,
        };
        if self.trace.is_some() {
            let operator = if ranged[first].is_some() {
                "range scan"
            } else {
                "scan"
            };
            let detail = triples[first].to_string();
            self.note(operator, detail, estimate(0), result.table.len(), start);
        }
        for (step, &next) in order.iter().enumerate().skip(1) {
            let start = Instant::now();
            let shared: Vec<Variable> = scans[next]
                .vars()
                .into_iter()
                .filter(|v| result.column(v).is_some())
                .collect();
            let probe = !shared.is_empty()
                && (result.table.len() as u64).saturating_mul(PROBE_FACTOR) < counts[next];
            result = if probe {
                self.probe_join(result, &scans[next], &shared)?
            } else {
                let scanned = match ranged[next] {
                    Some((permutation, ranges)) => {
                        self.scan_ranges(&scans[next], permutation, ranges)?
                    }
                    None => self.scan(&scans[next], shared.first())?,
                };
                self.join(result, scanned)?
            };
            if self.trace.is_some() {
                let operator = match (probe, shared.is_empty()) {
                    (true, _) => "index join",
                    (false, true) => "cross product",
                    (false, false) => "join",
                };
                let detail = triples[next].to_string();
                self.note(operator, detail, estimate(step), result.table.len(), start);
            }
        }
        Ok(result)
    }

    /// The order in which to join a BGP's patterns ([`plan`]; `counts` are exact). Two
    /// patterns start with the smaller one; larger BGPs are planned with distinct counts.
    fn join_order(&self, scans: &[ScanPattern], counts: &[u64]) -> plan::Plan {
        if scans.len() <= 2 {
            let mut order: Vec<usize> = (0..scans.len()).collect();
            order.sort_by_key(|&i| counts[i]);
            let mut rows = vec![f64::NAN; order.len()];
            rows[0] = counts[order[0]] as f64;
            return plan::Plan { order, rows };
        }
        let mut vars: Vec<Variable> = Vec::new();
        let inputs: Vec<plan::Input> = scans
            .iter()
            .zip(counts)
            .map(|(scan, &count)| plan::Input {
                count,
                vars: scan
                    .vars()
                    .into_iter()
                    .map(|v| {
                        let d = self.distinct(scan, &v, count);
                        let index = vars.iter().position(|x| *x == v).unwrap_or_else(|| {
                            vars.push(v);
                            vars.len() - 1
                        });
                        (index, d)
                    })
                    .collect(),
            })
            .collect();
        plan::order(&inputs, vars.len(), PROBE_FACTOR)
    }

    /// Distinct values of `var` among `scan`'s `count` matches: the engine's statistics
    /// where an index order puts `var` right after the constants, else `count`.
    fn distinct(&self, scan: &ScanPattern, var: &Variable, count: u64) -> u64 {
        const CANDIDATES: [Permutation; 4] = [
            Permutation::Gspo,
            Permutation::Gpos,
            Permutation::Gosp,
            Permutation::Gpso,
        ];
        if !scan.in_default_graph() {
            return count;
        }
        let positions: Vec<usize> = (0..4).filter(|&c| scan.slots[c].is_var(var)).collect();
        let [position] = positions[..] else {
            return count;
        };
        if scan.vars().len() == 1 {
            return count;
        }
        let bound = |c: usize| c == 3 || matches!(scan.slots[c], Slot::Const(_));
        CANDIDATES
            .iter()
            .find(|p| {
                let order = p.order();
                let prefix = order.iter().take_while(|&&c| bound(c)).count();
                order[prefix] == position && order[prefix + 1..].iter().all(|&c| !bound(c))
            })
            .and_then(|&p| {
                self.snapshot
                    .distinct_estimate_in(self.model, &scan.quad_pattern(), p)
            })
            .map_or(count, |d| d.min(count))
    }

    /// A cyclic BGP by a worst-case-optimal join ([`wcoj`]); `None` if it isn't cyclic.
    fn cyclic_bgp(&self, scans: &[ScanPattern], counts: &[u64]) -> NativeResult<Option<Solutions>> {
        let mut vars: Vec<Variable> = Vec::new();
        for scan in scans {
            for v in scan.vars() {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
        }
        let patterns: Vec<[wcoj::Pos; 3]> = scans
            .iter()
            .map(|scan| {
                // Default-graph patterns only (checked by the caller): the graph is constant.
                [0, 1, 2].map(|c| match &scan.slots[c] {
                    Slot::Const(id) => wcoj::Pos::Const(id.raw()),
                    Slot::Var(v) => {
                        wcoj::Pos::Var(vars.iter().position(|x| x == v).expect("collected above"))
                    }
                })
            })
            .collect();
        if !wcoj::cyclic(&patterns, vars.len()) {
            return Ok(None);
        }
        let width = vars.len();
        // A pattern without matches (constants-only ones included) empties the BGP.
        if counts.contains(&0) {
            return Ok(Some(Solutions {
                vars,
                table: IdTable::new(width),
                ordered: false,
            }));
        }
        let patterns: Vec<[wcoj::Pos; 3]> = patterns
            .into_iter()
            .filter(|p| p.iter().any(|x| matches!(x, wcoj::Pos::Var(_))))
            .collect();
        let query = wcoj::Query {
            snapshot: self.snapshot,
            model: self.model,
            patterns,
            variables: width,
            max_rows: self.max_rows(width),
        };
        let token = self.cancellation.clone();
        let cancelled = move || token.as_ref().is_some_and(CancellationToken::is_cancelled);
        let mut stats = wcoj::Stats::default();
        let rows = match query.run(&cancelled, &mut stats) {
            Ok(rows) => rows,
            Err(wcoj::Stop::Cancelled) => return Err(QueryEvaluationError::Cancelled.into()),
            Err(wcoj::Stop::TooManyRows) => {
                return Err(self.too_large(query.max_rows.saturating_add(1), width));
            }
        };
        let mut table = IdTable::new(width);
        for row in rows.chunks_exact(width) {
            table.push_row(row);
        }
        if let Some(trace) = &self.trace {
            let order: Vec<String> = stats
                .order
                .iter()
                .zip(&stats.candidates)
                .map(|(&v, n)| {
                    format!(
                        "{} ({} candidates)",
                        vars[v],
                        n.load(std::sync::atomic::Ordering::Relaxed)
                    )
                })
                .collect();
            trace.borrow_mut().push(PlanStep {
                depth: self.depth.get() + 1,
                operator: "wcoj order".to_owned(),
                detail: format!(
                    "{} | {} lookups",
                    order.join(", "),
                    stats.lookups.load(std::sync::atomic::Ordering::Relaxed)
                ),
                estimated_rows: None,
                rows: table.len() as u64,
                micros: 0,
            });
        }
        Ok(Some(self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })?))
    }

    fn scan_pattern(&self, triple: &TriplePattern) -> Option<ScanPattern> {
        let slot = |term: &TermPattern| -> Option<Slot> {
            Some(match term {
                TermPattern::Variable(v) => Slot::Var(v.clone()),
                TermPattern::BlankNode(b) => {
                    Slot::Var(Variable::new_unchecked(format!("_bnode_{}", b.as_str())))
                }
                TermPattern::NamedNode(n) => Slot::Const(self.snapshot.lookup(n.as_ref().into())?),
                TermPattern::Literal(l) => Slot::Const(self.snapshot.lookup(l.as_ref().into())?),
                #[allow(unreachable_patterns)]
                _ => return None,
            })
        };
        let predicate = match &triple.predicate {
            NamedNodePattern::Variable(v) => Slot::Var(v.clone()),
            NamedNodePattern::NamedNode(n) => Slot::Const(self.snapshot.lookup(n.as_ref().into())?),
        };
        let graph = match &*self.graph.borrow() {
            GraphScope::Default => Slot::Const(TermId::DEFAULT_GRAPH),
            GraphScope::Named(id) => Slot::Const(*id),
            GraphScope::Variable(v) => Slot::Var(v.clone()),
            GraphScope::Missing => return None,
        };
        Some(ScanPattern {
            slots: [
                slot(&triple.subject)?,
                predicate,
                slot(&triple.object)?,
                graph,
            ],
        })
    }

    /// Scans one pattern into a table, sorted on `sort_var` first when some permutation can.
    fn scan(&self, scan: &ScanPattern, sort_var: Option<&Variable>) -> NativeResult<Solutions> {
        let pattern = scan.quad_pattern();
        let permutation = scan.permutation_for(sort_var);
        let vars = scan.vars();
        let columns_of: Vec<Vec<usize>> = vars
            .iter()
            .map(|v| (0..4).filter(|&i| scan.slots[i].is_var(v)).collect())
            .collect();
        let mut table = IdTable::new(vars.len());
        let mut row = vec![0u64; vars.len()];
        let quads = self
            .snapshot
            .scan_sorted_in(self.model, &pattern, permutation)
            .expect("permutation_for returns a usable permutation");
        'quads: for (n, quad) in quads.enumerate() {
            if n % (1 << 16) == 0 {
                self.check()?;
            }
            let components = quad.components();
            for (slot, positions) in row.iter_mut().zip(&columns_of) {
                let value = components[positions[0]];
                // A variable used twice in one pattern must bind the same term.
                if positions[1..].iter().any(|&p| components[p] != value) {
                    continue 'quads;
                }
                *slot = value;
            }
            table.push_row(&row);
        }
        // The scan is sorted on the free components in permutation order.
        let order: Vec<usize> = permutation
            .order()
            .iter()
            .filter_map(|&component| match &scan.slots[component] {
                Slot::Var(v) => vars.iter().position(|x| x == v),
                Slot::Const(_) => None,
            })
            .collect();
        let mut sorted = Vec::new();
        for column in order {
            if !sorted.contains(&column) {
                sorted.push(column);
            }
        }
        let table = table.assume_sorted_by(sorted);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }

    /// Scans `scan` over `ranges` of its object (the first free component of
    /// `permutation`), in increasing order, so the output is sorted like a full scan.
    fn scan_ranges(
        &self,
        scan: &ScanPattern,
        permutation: Permutation,
        ranges: &[(TermId, TermId)],
    ) -> NativeResult<Solutions> {
        let pattern = scan.quad_pattern();
        let vars = scan.vars();
        let columns: Vec<usize> = vars
            .iter()
            .map(|v| {
                (0..4)
                    .find(|&i| scan.slots[i].is_var(v))
                    .expect("variable of the pattern")
            })
            .collect();
        let mut table = IdTable::new(vars.len());
        let mut row = vec![0u64; vars.len()];
        for &(low, high) in ranges {
            let Some(quads) =
                self.snapshot
                    .scan_range_in(self.model, &pattern, permutation, low, high)
            else {
                return self.scan(scan, None);
            };
            for (n, quad) in quads.enumerate() {
                if n % (1 << 16) == 0 {
                    self.check()?;
                }
                let components = quad.components();
                for (slot, &c) in row.iter_mut().zip(&columns) {
                    *slot = components[c];
                }
                table.push_row(&row);
            }
        }
        let object = vars
            .iter()
            .position(|v| scan.slots[2].is_var(v))
            .expect("ranged object");
        let table = table.assume_sorted_by(vec![object]);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }

    /// Joins `result` with `scan` by probing the index once per distinct key of `result`.
    fn probe_join(
        &self,
        mut result: Solutions,
        scan: &ScanPattern,
        shared: &[Variable],
    ) -> NativeResult<Solutions> {
        let key_columns: Vec<usize> = shared
            .iter()
            .map(|v| result.column(v).expect("shared"))
            .collect();
        if has_undef(&result.table, &key_columns) {
            let scanned = self.scan(scan, shared.first())?;
            return self.join(result, scanned);
        }
        if !result.ordered {
            result.table.sort_by(&key_columns);
        }
        let new_vars: Vec<Variable> = scan
            .vars()
            .into_iter()
            .filter(|v| result.column(v).is_none())
            .collect();
        let mut vars = result.vars.clone();
        vars.extend(new_vars.iter().cloned());
        // Where each new variable sits in a matching quad (the shared ones become constants).
        let positions: Vec<Vec<usize>> = new_vars
            .iter()
            .map(|v| (0..4).filter(|&i| scan.slots[i].is_var(v)).collect())
            .collect();
        let shared_positions: Vec<Vec<usize>> = shared
            .iter()
            .map(|v| (0..4).filter(|&i| scan.slots[i].is_var(v)).collect())
            .collect();
        let probe = Probe {
            snapshot: self.snapshot,
            model: self.model,
            table: &result.table,
            scan,
            key_columns: &key_columns,
            shared_positions: &shared_positions,
            positions: &positions,
            width: vars.len(),
            max_rows: self.max_rows(vars.len()),
            produced: AtomicUsize::new(0),
        };
        let token = self.cancellation.clone();
        let cancelled = move || token.as_ref().is_some_and(CancellationToken::is_cancelled);
        let rows = result.table.len();
        let out = if rows < 2 * PROBE_CHUNK {
            probe.rows(0..rows, &cancelled)
        } else {
            let parts: Vec<Result<IdTable, ProbeStop>> = (0..rows.div_ceil(PROBE_CHUNK))
                .into_par_iter()
                .map(|i| {
                    probe.rows(
                        i * PROBE_CHUNK..((i + 1) * PROBE_CHUNK).min(rows),
                        &cancelled,
                    )
                })
                .collect();
            parts
                .into_iter()
                .collect::<Result<Vec<IdTable>, ProbeStop>>()
                .map(|parts| IdTable::concat(vars.len(), parts))
        };
        let out = match out {
            Ok(out) => out,
            Err(ProbeStop::Cancelled) => return Err(QueryEvaluationError::Cancelled.into()),
            Err(ProbeStop::TooManyRows) => {
                return Err(self.too_large(probe.max_rows.saturating_add(1), vars.len()));
            }
        };
        let out = if result.ordered {
            out
        } else {
            out.assume_sorted_by(key_columns)
        };
        self.consumed(&result);
        self.produced(Solutions {
            vars,
            table: out,
            ordered: result.ordered,
        })
    }

    // --- joins ---------------------------------------------------------------------------

    fn join(&self, left: Solutions, right: Solutions) -> NativeResult<Solutions> {
        let (lk, rk) = shared_columns(&left, &right);
        let vars = joined_vars(&left, &right, &rk);
        let max_rows = self.max_rows(vars.len());
        let table = if has_undef(&left.table, &lk) || has_undef(&right.table, &rk) {
            if left.ordered {
                return Err(NativeError::Fallback);
            }
            join_with_undef(&left.table, &right.table, &lk, &rk, max_rows)
        } else if left.ordered {
            join_keeping_left_order(&left.table, &right.table, &lk, &rk, max_rows)
        } else {
            join(&left.table, &right.table, &lk, &rk, max_rows)
        }
        .map_err(|e| self.too_large(e.max_rows.saturating_add(1), vars.len()))?;
        self.consumed(&left);
        self.consumed(&right);
        self.produced(Solutions {
            vars,
            table,
            ordered: left.ordered,
        })
    }

    fn left_join(
        &self,
        left: Solutions,
        right: Solutions,
        expression: Option<&Expression>,
    ) -> NativeResult<Solutions> {
        let (lk, rk) = shared_columns(&left, &right);
        if has_undef(&left.table, &lk) || has_undef(&right.table, &rk) {
            return Err(NativeError::Fallback);
        }
        let vars = joined_vars(&left, &right, &rk);
        let max_rows = self.max_rows(vars.len());
        let table = match expression {
            None => left_join(&left.table, &right.table, &lk, &rk, None, max_rows),
            Some(expression) => {
                let accept = |row: &[u64]| {
                    let binding = |v: &Variable| {
                        let column = vars.iter().position(|x| x == v)?;
                        self.term(row[column])
                    };
                    self.evaluator.filter(expression, &binding)
                };
                left_join(&left.table, &right.table, &lk, &rk, Some(&accept), max_rows)
            }
        }
        .map_err(|e| self.too_large(e.max_rows.saturating_add(1), vars.len()))?;
        self.consumed(&left);
        self.consumed(&right);
        self.produced(Solutions {
            vars,
            table,
            ordered: left.ordered,
        })
    }

    fn union(&self, left: Solutions, right: Solutions) -> NativeResult<Solutions> {
        let mut vars = left.vars.clone();
        for v in &right.vars {
            if !vars.contains(v) {
                vars.push(v.clone());
            }
        }
        let widen = |s: &Solutions| {
            let columns: Vec<Vec<u64>> = vars
                .iter()
                .map(|v| match s.column(v) {
                    Some(c) => s.table.column(c).to_vec(),
                    None => vec![UNDEF; s.table.len()],
                })
                .collect();
            IdTable::from_columns(columns)
        };
        let mut table = widen(&left);
        table.append(&widen(&right));
        self.consumed(&left);
        self.consumed(&right);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }

    // --- filters -------------------------------------------------------------------------

    /// `pattern` if it is one triple pattern, optionally filtered and projected, evaluated
    /// until `limit` rows pass: the scan streams in chunks and stops early. `None` for other
    /// shapes.
    fn limited_scan(
        &self,
        pattern: &GraphPattern,
        limit: usize,
    ) -> NativeResult<Option<Solutions>> {
        let (projection, rest) = match pattern {
            GraphPattern::Project { inner, variables } => (Some(variables), &**inner),
            other => (None, other),
        };
        let (filter, bgp) = match rest {
            GraphPattern::Filter { expr, inner } if !contains_exists(expr) => {
                (Some(expr), &**inner)
            }
            other => (None, other),
        };
        let GraphPattern::Bgp { patterns } = bgp else {
            return Ok(None);
        };
        let [triple] = patterns.as_slice() else {
            return Ok(None);
        };
        let vars = triple_variables(triple);
        let mut vars_unique: Vec<Variable> = Vec::new();
        for v in vars {
            if !vars_unique.contains(&v) {
                vars_unique.push(v);
            }
        }
        let width = vars_unique.len();
        let Some(scan) = self.scan_pattern(triple) else {
            let solutions = Solutions {
                vars: vars_unique,
                table: IdTable::new(width),
                ordered: false,
            };
            return Ok(Some(match projection {
                Some(variables) => self.project(solutions, variables),
                None => solutions,
            }));
        };
        let places: Vec<Vec<usize>> = vars_unique
            .iter()
            .map(|v| (0..4).filter(|&i| scan.slots[i].is_var(v)).collect())
            .collect();
        let mut out = IdTable::new(width);
        let mut chunk = IdTable::new(width);
        let mut row = vec![0u64; width];
        let mut quads = self
            .snapshot
            .quads_for_pattern_in(self.model, &scan.quad_pattern());
        loop {
            let more = quads.next();
            if let Some(quad) = more {
                let components = quad.components();
                let consistent = places.iter().zip(row.iter_mut()).all(|(p, slot)| {
                    *slot = components[p[0]];
                    p[1..].iter().all(|&i| components[i] == *slot)
                });
                if consistent {
                    chunk.push_row(&row);
                }
            }
            if chunk.len() >= 4096 || (more.is_none() && !chunk.is_empty()) {
                self.check()?;
                let mut part = Solutions {
                    vars: vars_unique.clone(),
                    table: std::mem::replace(&mut chunk, IdTable::new(width)),
                    ordered: false,
                };
                if let Some(expression) = filter {
                    let mask = self.filter_mask(&part, expression)?;
                    part.table.retain_mask(&mask);
                }
                out.append(&part.table);
                if out.len() >= limit {
                    out.slice(0, Some(limit));
                    break;
                }
            }
            if more.is_none() {
                break;
            }
        }
        let solutions = Solutions {
            vars: vars_unique,
            table: out,
            ordered: false,
        };
        Ok(Some(match projection {
            Some(variables) => self.project(solutions, variables),
            None => solutions,
        }))
    }

    /// Which rows pass `expression`: the compiled id-level predicate where it decides
    /// ([`fast`]), the generic term evaluator for every other row.
    fn filter_mask(
        &self,
        solutions: &Solutions,
        expression: &Expression,
    ) -> NativeResult<Vec<bool>> {
        let compiled = fast::compile(expression, self.snapshot);
        let rows = solutions.table.len();
        let mask = FilterMask {
            snapshot: self.snapshot,
            evaluator: &self.evaluator,
            compiled: compiled.as_ref(),
            expression,
            solutions,
        };
        if rows < 2 * PARALLEL_EXPRESSION_ROWS {
            let term = |id: u64| self.term(id);
            let mut out = Vec::with_capacity(rows);
            for start in (0..rows).step_by(1 << 16) {
                self.check()?;
                out.extend(mask.rows(start..(start + (1 << 16)).min(rows), &term));
            }
            return Ok(out);
        }
        // Parallel: each task decodes through the snapshot with its own cache; the query's
        // computed terms are only read while the filter runs.
        let computed = self.computed.borrow();
        let computed: &[Term] = &computed;
        let token = self.cancellation.as_ref();
        let parts: Vec<Option<Vec<bool>>> = (0..rows.div_ceil(PARALLEL_EXPRESSION_ROWS))
            .into_par_iter()
            .map(|i| {
                if token.is_some_and(CancellationToken::is_cancelled) {
                    return None;
                }
                let decoder = Decoder::new(self.snapshot, computed);
                let range =
                    i * PARALLEL_EXPRESSION_ROWS..((i + 1) * PARALLEL_EXPRESSION_ROWS).min(rows);
                Some(mask.rows(range, &|id| decoder.term(id)))
            })
            .collect();
        let mut out = Vec::with_capacity(rows);
        for part in parts {
            let Some(part) = part else {
                return Err(QueryEvaluationError::Cancelled.into());
            };
            out.extend(part);
        }
        Ok(out)
    }

    fn filter(&self, mut solutions: Solutions, expression: &Expression) -> NativeResult<Solutions> {
        match expression {
            Expression::And(a, b) if contains_exists(expression) => {
                let solutions = self.filter(solutions, a)?;
                self.filter(solutions, b)
            }
            Expression::Exists(pattern) => self.exists(solutions, pattern, true),
            Expression::Not(inner) if matches!(**inner, Expression::Exists(_)) => {
                let Expression::Exists(pattern) = &**inner else {
                    unreachable!()
                };
                self.exists(solutions, pattern, false)
            }
            _ => {
                let mask = self.filter_mask(&solutions, expression)?;
                solutions.table.retain_mask(&mask);
                Ok(solutions)
            }
        }
    }

    fn exists(
        &self,
        solutions: Solutions,
        pattern: &GraphPattern,
        keep_matching: bool,
    ) -> NativeResult<Solutions> {
        let inner = self.eval(pattern)?;
        let (lk, rk) = shared_columns(&solutions, &inner);
        if has_undef(&solutions.table, &lk) {
            return Err(NativeError::Fallback);
        }
        let table = if keep_matching {
            semi_join(&solutions.table, &inner.table, &lk, &rk)
        } else {
            anti_join(&solutions.table, &inner.table, &lk, &rk)
        };
        self.consumed(&inner);
        Ok(Solutions {
            vars: solutions.vars,
            table,
            ordered: solutions.ordered,
        })
    }

    // --- modifiers -----------------------------------------------------------------------

    fn project(&self, solutions: Solutions, variables: &[Variable]) -> Solutions {
        let columns: Vec<Vec<u64>> = variables
            .iter()
            .map(|v| match solutions.column(v) {
                Some(c) => solutions.table.column(c).to_vec(),
                None => vec![UNDEF; solutions.table.len()],
            })
            .collect();
        let mut table = IdTable::from_columns(columns);
        if variables.is_empty() {
            table = IdTable::from_rows(0, std::iter::repeat_n(&[][..], solutions.table.len()));
        }
        Solutions {
            vars: variables.to_vec(),
            table,
            ordered: solutions.ordered,
        }
    }

    /// Sorts by the ORDER BY keys. With `limit`, only the first `limit` rows are guaranteed to
    /// be in order (a partial sort), which is all `LIMIT` needs.
    /// A sort key per row of `column` whose integer order is the SPARQL `ORDER BY` order of the
    /// bound terms. Inline integers (and UNDEF) sort by id already (offset binary). Otherwise
    /// each distinct id is decoded once, the distinct terms are sorted, and every row gets its
    /// term's rank: equal-ordering terms (1 and 1.0) share a rank.
    /// Every aggregate of every group (`members` lists each group's rows), in parallel
    /// chunks of groups for large inputs.
    fn aggregate_groups(
        &self,
        solutions: &Solutions,
        members: &[Vec<usize>],
        aggregates: &[(Variable, AggregateExpression)],
    ) -> NativeResult<Vec<Vec<Agg>>> {
        let per_group = |aggregator: &Aggregator<'_>, rows: &[usize]| -> Vec<Agg> {
            aggregates
                .iter()
                .map(|(_, aggregate)| aggregator.aggregate(solutions, rows, aggregate))
                .collect()
        };
        if solutions.table.len() < 2 * PARALLEL_EXPRESSION_ROWS || members.len() < 2 {
            let term = |id: u64| self.term(id);
            let aggregator = Aggregator {
                evaluator: &self.evaluator,
                term: &term,
            };
            return Ok(members
                .iter()
                .map(|rows| per_group(&aggregator, rows))
                .collect());
        }
        // Chunks of groups holding about PARALLEL_EXPRESSION_ROWS rows each.
        let mut bounds = vec![0];
        let mut rows = 0;
        for (i, group) in members.iter().enumerate() {
            rows += group.len();
            if rows >= PARALLEL_EXPRESSION_ROWS {
                bounds.push(i + 1);
                rows = 0;
            }
        }
        if *bounds.last().expect("starts with 0") != members.len() {
            bounds.push(members.len());
        }
        let computed = self.computed.borrow();
        let computed: &[Term] = &computed;
        let token = self.cancellation.as_ref();
        let (snapshot, evaluator) = (self.snapshot, &self.evaluator);
        let parts: Vec<Option<Vec<Vec<Agg>>>> = bounds
            .par_windows(2)
            .map(|window| {
                if token.is_some_and(CancellationToken::is_cancelled) {
                    return None;
                }
                let decoder = Decoder::new(snapshot, computed);
                let term = |id: u64| decoder.term(id);
                let aggregator = Aggregator {
                    evaluator,
                    term: &term,
                };
                Some(
                    members[window[0]..window[1]]
                        .iter()
                        .map(|rows| per_group(&aggregator, rows))
                        .collect(),
                )
            })
            .collect();
        let mut out = Vec::with_capacity(members.len());
        for part in parts {
            let Some(part) = part else {
                return Err(QueryEvaluationError::Cancelled.into());
            };
            out.extend(part);
        }
        Ok(out)
    }

    fn order_ranks(&self, column: &[u64]) -> Vec<u64> {
        let integer = |id: u64| TermId::from_raw(id).kind() == nrese_engine::TermKind::Integer;
        if column.iter().all(|&id| id == UNDEF || integer(id)) {
            // UNDEF sorts first in SPARQL; every inline integer id is above 0.
            return column
                .iter()
                .map(|&id| if id == UNDEF { 0 } else { id })
                .collect();
        }
        let mut distinct = column.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        let terms: Vec<Option<Term>> = distinct.iter().map(|&id| self.term(id)).collect();
        let mut by_term: Vec<usize> = (0..distinct.len()).collect();
        by_term.sort_by(|&a, &b| value::order(terms[a].as_ref(), terms[b].as_ref()));
        let mut rank_of = vec![0u64; distinct.len()];
        let mut rank = 0;
        for (i, &d) in by_term.iter().enumerate() {
            if i > 0 && value::order(terms[by_term[i - 1]].as_ref(), terms[d].as_ref()).is_ne() {
                rank += 1;
            }
            rank_of[d] = rank;
        }
        column
            .iter()
            .map(|id| rank_of[distinct.binary_search(id).expect("id is in the column")])
            .collect()
    }

    fn order_by(
        &self,
        mut solutions: Solutions,
        keys: &[OrderExpression],
        limit: Option<usize>,
    ) -> NativeResult<Solutions> {
        let n = solutions.table.len();
        // A key that is a variable holding only inline integers (or UNDEF) sorts by id: inline
        // integer ids are ordered by value (offset binary), so no term is decoded.
        let key_values: Vec<SortKey> = keys
            .iter()
            .map(|key| {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = key;
                if let Expression::Variable(v) = e
                    && let Some(column) = solutions.column(v)
                {
                    return SortKey::Ids(self.order_ranks(solutions.table.column(column)));
                }
                SortKey::Terms(
                    (0..n)
                        .map(|row| self.evaluator.eval(e, &self.binding(&solutions, row)))
                        .collect(),
                )
            })
            .collect();
        let compare = |a: &usize, b: &usize| {
            for (key, values) in keys.iter().zip(&key_values) {
                let ordering = match values {
                    SortKey::Ids(ids) => ids[*a].cmp(&ids[*b]),
                    SortKey::Terms(terms) => value::order(terms[*a].as_ref(), terms[*b].as_ref()),
                };
                let ordering = match key {
                    OrderExpression::Asc(_) => ordering,
                    OrderExpression::Desc(_) => ordering.reverse(),
                };
                if ordering.is_ne() {
                    return ordering;
                }
            }
            a.cmp(b)
        };
        let mut order: Vec<usize> = (0..n).collect();
        match limit {
            Some(k) if k < n => {
                order.select_nth_unstable_by(k, compare);
                order.truncate(k);
                order.sort_unstable_by(compare);
            }
            _ => order.sort_unstable_by(compare),
        }
        let columns: Vec<Vec<u64>> = solutions
            .table
            .columns()
            .iter()
            .map(|column| order.iter().map(|&row| column[row]).collect())
            .collect();
        let width = solutions.table.width();
        solutions.table = if width == 0 {
            IdTable::from_rows(0, std::iter::repeat_n(&[][..], order.len()))
        } else {
            IdTable::from_columns(columns)
        };
        solutions.ordered = true;
        Ok(solutions)
    }

    // --- aggregation ---------------------------------------------------------------------

    fn group(
        &self,
        inner: &GraphPattern,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
    ) -> NativeResult<Solutions> {
        // COUNT(*) of one triple pattern without GROUP BY: the index knows the answer.
        if variables.is_empty()
            && let [(target, AggregateExpression::CountSolutions { distinct: false })] = aggregates
            && let GraphPattern::Bgp { patterns } = inner
            && let [triple] = patterns.as_slice()
        {
            let count = match self.scan_pattern(triple) {
                Some(scan) if !scan.repeats_variable() => {
                    self.snapshot.count_in(self.model, &scan.quad_pattern())
                }
                Some(scan) => self.scan(&scan, None)?.table.len() as u64,
                None => 0,
            };
            let mut table = IdTable::new(1);
            table.push_row(&[self.id(&integer(count))]);
            return Ok(Solutions {
                vars: vec![target.clone()],
                table,
                ordered: false,
            });
        }
        // GROUP BY one variable with only row counts, over one triple pattern: the index
        // counts each group by binary search (O(groups · log n)), without reading matches.
        if let [key] = variables
            && let GraphPattern::Bgp { patterns } = inner
            && let [triple] = patterns.as_slice()
            && let Some(scan) = self.scan_pattern(triple)
            && !scan.repeats_variable()
            && aggregates
                .iter()
                .all(|(_, aggregate)| counts_rows(aggregate, &scan))
            && let Some(component) = (0..4).find(|&c| scan.slots[c].is_var(key))
        {
            let permutation = scan.permutation_for(Some(key));
            if scan.first_free(permutation) == Some(component)
                && let Some(groups) =
                    self.snapshot
                        .group_counts_in(self.model, &scan.quad_pattern(), permutation)
            {
                let mut columns = vec![
                    groups
                        .iter()
                        .map(|(value, _)| value.raw())
                        .collect::<Vec<_>>(),
                ];
                let counts: Vec<u64> = groups.iter().map(|&(_, n)| self.id(&integer(n))).collect();
                let mut vars = vec![key.clone()];
                for (target, _) in aggregates {
                    columns.push(counts.clone());
                    vars.push(target.clone());
                }
                let table = IdTable::from_columns(columns).assume_sorted_by(vec![0]);
                return self.produced(Solutions {
                    vars,
                    table,
                    ordered: false,
                });
            }
        }
        let solutions = self.eval(inner)?;
        let key_table = if variables.is_empty() {
            IdTable::from_rows(0, std::iter::repeat_n(&[][..], solutions.table.len()))
        } else {
            let key_columns: Vec<Vec<u64>> = variables
                .iter()
                .map(|v| match solutions.column(v) {
                    Some(c) => solutions.table.column(c).to_vec(),
                    None => vec![UNDEF; solutions.table.len()],
                })
                .collect();
            let key_table = IdTable::from_columns(key_columns);
            let sorted_on: Vec<usize> = variables
                .iter()
                .map_while(|v| solutions.column(v))
                .collect();
            if sorted_on.len() == variables.len() && solutions.table.is_sorted_on(&sorted_on) {
                key_table.assume_sorted_by((0..variables.len()).collect())
            } else {
                key_table
            }
        };
        let keys: Vec<usize> = (0..variables.len()).collect();
        let groups = group_rows(&key_table, &keys);
        // Without GROUP BY, an empty input still yields one (empty) group.
        let group_count = groups.len();
        let mut members: Vec<Vec<usize>> = vec![Vec::new(); group_count];
        for (row, &group) in groups.group_of.iter().enumerate() {
            members[group as usize].push(row);
        }
        let mut columns: Vec<Vec<u64>> = groups.keys.clone().into_columns();
        let mut vars: Vec<Variable> = variables.to_vec();
        let values = self.aggregate_groups(&solutions, &members, aggregates)?;
        for (index, (target, _)) in aggregates.iter().enumerate() {
            let column: Vec<u64> = values
                .iter()
                .map(|group| match &group[index] {
                    Agg::Id(id) => *id,
                    Agg::Term(term) => self.id(term),
                })
                .collect();
            columns.push(column);
            vars.push(target.clone());
        }
        let table = if columns.is_empty() {
            IdTable::from_rows(0, std::iter::repeat_n(&[][..], group_count))
        } else {
            IdTable::from_columns(columns)
        };
        self.consumed(&solutions);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }
}

/// A pattern's ranged scan: the permutation that sorts on its object, and the object's
/// id ranges from a range hint.
type RangedScan<'a> = (Permutation, &'a [(TermId, TermId)]);

/// One ORDER BY key's values per row: ids where id order is value order, else terms.
enum SortKey {
    Ids(Vec<u64>),
    Terms(Vec<Option<Term>>),
}

/// True if `aggregate` counts the rows of a pattern: `COUNT(*)`, or `COUNT(?v)` for a
/// variable the pattern always binds.
/// An operator's name and details for EXPLAIN.
fn describe(pattern: &GraphPattern) -> (&'static str, String) {
    let list = |vars: &[Variable]| {
        vars.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    };
    match pattern {
        GraphPattern::Bgp { patterns } => ("bgp", format!("{} patterns", patterns.len())),
        GraphPattern::Path {
            subject,
            path,
            object,
        } => ("path", format!("{subject} {path} {object}")),
        GraphPattern::Join { .. } => ("join", String::new()),
        GraphPattern::LeftJoin { expression, .. } => (
            "optional",
            expression
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        ),
        GraphPattern::Filter { expr, .. } => ("filter", expr.to_string()),
        GraphPattern::Union { .. } => ("union", String::new()),
        GraphPattern::Extend {
            variable,
            expression,
            ..
        } => ("bind", format!("{expression} AS {variable}")),
        GraphPattern::Minus { .. } => ("minus", String::new()),
        GraphPattern::Values { variables, .. } => ("values", list(variables)),
        GraphPattern::OrderBy { expression, .. } => (
            "order by",
            expression
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        GraphPattern::Project { variables, .. } => ("project", list(variables)),
        GraphPattern::Distinct { .. } => ("distinct", String::new()),
        GraphPattern::Reduced { .. } => ("reduced", String::new()),
        GraphPattern::Slice { start, length, .. } => (
            "slice",
            match length {
                Some(length) => format!("offset {start} limit {length}"),
                None => format!("offset {start}"),
            },
        ),
        GraphPattern::Group { variables, .. } => ("group", list(variables)),
        _ => ("operator", String::new()),
    }
}

/// Rows per parallel task of a filter or aggregation; smaller inputs run on one thread.
const PARALLEL_EXPRESSION_ROWS: usize = 1 << 13;

/// Decoded terms a [`Decoder`] keeps.
const DECODER_CACHE_ENTRIES: usize = 1 << 12;

/// Term decoding for one task of a parallel operator: the snapshot, the query's computed
/// terms (read-only meanwhile), and a small cache of its own.
struct Decoder<'a> {
    snapshot: &'a Snapshot,
    computed: &'a [Term],
    cache: RefCell<HashMap<u64, Option<Term>>>,
}

impl<'a> Decoder<'a> {
    fn new(snapshot: &'a Snapshot, computed: &'a [Term]) -> Self {
        Self {
            snapshot,
            computed,
            cache: RefCell::default(),
        }
    }

    fn term(&self, id: u64) -> Option<Term> {
        if let Some(term) = self.cache.borrow().get(&id) {
            return term.clone();
        }
        let term = decode(self.snapshot, self.computed, id);
        let mut cache = self.cache.borrow_mut();
        if cache.len() < DECODER_CACHE_ENTRIES {
            cache.insert(id, term.clone());
        }
        term
    }
}

/// A FILTER over a table: the id-level fast path where it decides, the evaluator otherwise.
/// Thread-safe; the caller supplies term decoding.
struct FilterMask<'a> {
    snapshot: &'a Snapshot,
    evaluator: &'a Evaluator,
    compiled: Option<&'a fast::Fast>,
    expression: &'a Expression,
    solutions: &'a Solutions,
}

impl FilterMask<'_> {
    /// Whether each row of `rows` passes.
    fn rows(&self, rows: Range<usize>, term: &dyn Fn(u64) -> Option<Term>) -> Vec<bool> {
        let table = &self.solutions.table;
        rows.map(|row| {
            let decided = self.compiled.map(|fast| {
                let value = |v: &Variable| {
                    self.solutions
                        .column(v)
                        .map_or(UNDEF, |c| table.get(row, c))
                };
                fast.eval(&value, self.snapshot)
            });
            match decided {
                Some(fast::Tri::True) => true,
                Some(fast::Tri::False | fast::Tri::Error) => false,
                Some(fast::Tri::Unknown) | None => {
                    let binding = |v: &Variable| term(table.get(row, self.solutions.column(v)?));
                    self.evaluator.filter(self.expression, &binding)
                }
            }
        })
        .collect()
    }
}

/// An index nested-loop join of a key-sorted `table` with `scan`: the index is probed once
/// per distinct key. Holds only thread-safe state, so chunks of rows run in parallel.
struct Probe<'a> {
    snapshot: &'a Snapshot,
    model: ReadModel,
    table: &'a IdTable,
    scan: &'a ScanPattern,
    key_columns: &'a [usize],
    /// The scan positions of each key column's variable.
    shared_positions: &'a [Vec<usize>],
    /// The scan positions of each new variable.
    positions: &'a [Vec<usize>],
    width: usize,
    /// Output rows allowed in total (the query's memory budget), and produced so far.
    max_rows: usize,
    produced: AtomicUsize,
}

/// Why an index nested-loop join stopped early.
enum ProbeStop {
    Cancelled,
    TooManyRows,
}

impl Probe<'_> {
    /// The joined rows for `table` rows `rows`, in order.
    fn rows(
        &self,
        rows: Range<usize>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<IdTable, ProbeStop> {
        let table = self.table;
        let width = table.width();
        let mut out = IdTable::new(self.width);
        let mut matches: Vec<Vec<u64>> = Vec::new();
        let mut row = vec![0u64; self.width];
        let mut bound = self.scan.clone();
        let first = rows.start;
        let mut counted = 0;
        for r in rows {
            if (r - first).is_multiple_of(4096) {
                if cancelled() {
                    return Err(ProbeStop::Cancelled);
                }
                self.grow(out.len() - counted)?;
                counted = out.len();
            }
            let same_key = r > first
                && self
                    .key_columns
                    .iter()
                    .all(|&k| table.get(r, k) == table.get(r - 1, k));
            if !same_key {
                matches.clear();
                for (slots, &k) in self.shared_positions.iter().zip(self.key_columns) {
                    let id = TermId::from_raw(table.get(r, k));
                    for &i in slots {
                        bound.slots[i] = Slot::Const(id);
                    }
                }
                'quads: for quad in self
                    .snapshot
                    .quads_for_pattern_in(self.model, &bound.quad_pattern())
                {
                    let components = quad.components();
                    let mut values = Vec::with_capacity(self.positions.len());
                    for places in self.positions {
                        let value = components[places[0]];
                        if places[1..].iter().any(|&p| components[p] != value) {
                            continue 'quads;
                        }
                        values.push(value);
                    }
                    matches.push(values);
                }
            }
            for values in &matches {
                for (c, slot) in row.iter_mut().enumerate().take(width) {
                    *slot = table.get(r, c);
                }
                row[width..].copy_from_slice(values);
                out.push_row(&row);
            }
            // One key can match a whole index range: check within large fan-outs too.
            if out.len() - counted > 1 << 16 {
                self.grow(out.len() - counted)?;
                counted = out.len();
            }
        }
        self.grow(out.len() - counted)?;
        Ok(out)
    }

    fn grow(&self, rows: usize) -> Result<(), ProbeStop> {
        let total = self.produced.fetch_add(rows, AtomicOrdering::Relaxed) + rows;
        if total > self.max_rows {
            return Err(ProbeStop::TooManyRows);
        }
        Ok(())
    }
}

/// An aggregate's value: a stored or computed id, or a new term the query interns.
enum Agg {
    Id(u64),
    Term(Term),
}

/// Computes aggregates over groups of rows. Thread-safe given a thread-safe `term`, so
/// groups can be aggregated in parallel; the caller interns the results.
struct Aggregator<'a> {
    evaluator: &'a Evaluator,
    term: &'a dyn Fn(u64) -> Option<Term>,
}

impl Aggregator<'_> {
    fn binding<'s>(
        &'s self,
        solutions: &'s Solutions,
        row: usize,
    ) -> impl Fn(&Variable) -> Option<Term> + 's {
        move |variable| (self.term)(solutions.table.get(row, solutions.column(variable)?))
    }

    /// An aggregate over one variable's ids without decoding terms, where that is exact:
    /// COUNT always, and SUM/AVG/MIN/MAX when every value is an inline integer (whose id
    /// order is value order). `None` means "evaluate on terms". Error semantics are
    /// spareval's: an unbound value makes SUM/AVG/MIN/MAX unbound, and an i64 overflow
    /// makes SUM/AVG unbound.
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
                Some(match name {
                    AggregateFunction::Min => Agg::Id(ids.iter().copied().min().unwrap_or(UNDEF)),
                    AggregateFunction::Max => Agg::Id(ids.iter().copied().max().unwrap_or(UNDEF)),
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

    fn aggregate(
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
                let evaluated: Vec<Option<Term>> = rows
                    .iter()
                    .map(|&row| self.evaluator.eval(expr, &self.binding(solutions, row)))
                    .collect();
                // As spareval: COUNT skips errors and SAMPLE takes the first value, but one
                // error makes SUM, AVG, MIN and MAX unbound.
                let fails_on_error =
                    !matches!(name, AggregateFunction::Count | AggregateFunction::Sample);
                if fails_on_error && evaluated.iter().any(Option::is_none) {
                    return Agg::Id(UNDEF);
                }
                let mut values: Vec<Term> = evaluated.into_iter().flatten().collect();
                if *distinct {
                    let mut unique = Vec::with_capacity(values.len());
                    for value in values {
                        if !unique.contains(&value) {
                            unique.push(value);
                        }
                    }
                    values = unique;
                }
                let result = match name {
                    AggregateFunction::Count => Some(integer(values.len() as u64)),
                    AggregateFunction::Sample => values.into_iter().next().map(value::canonical),
                    // The first of equal extremes, as spareval keeps it.
                    AggregateFunction::Min => values
                        .into_iter()
                        .reduce(|best, v| {
                            if value::order(Some(&v), Some(&best)).is_lt() {
                                v
                            } else {
                                best
                            }
                        })
                        .map(value::canonical),
                    AggregateFunction::Max => values
                        .into_iter()
                        .reduce(|best, v| {
                            if value::order(Some(&v), Some(&best)).is_gt() {
                                v
                            } else {
                                best
                            }
                        })
                        .map(value::canonical),
                    AggregateFunction::Sum => sum(&values),
                    AggregateFunction::Avg => average(&values),
                    _ => None,
                };
                result.map_or(Agg::Id(UNDEF), Agg::Term)
            }
        }
    }
}

fn counts_rows(aggregate: &AggregateExpression, scan: &ScanPattern) -> bool {
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

fn integer(value: u64) -> Term {
    Literal::new_typed_literal(value.to_string(), xsd::INTEGER).into()
}

/// A running numeric sum with SPARQL type promotion; `None` once a non-number appears.
#[derive(Clone, Copy)]
enum Numeric {
    Integer(Integer),
    Decimal(Decimal),
    Float(Float),
    Double(Double),
}

impl Numeric {
    fn of(term: &Term) -> Option<Self> {
        Some(match Value::of(term) {
            Value::Integer(i) => Self::Integer(i),
            Value::Decimal(d) => Self::Decimal(d),
            Value::Float(f) => Self::Float(f),
            Value::Double(d) => Self::Double(d),
            _ => return None,
        })
    }

    fn add(self, other: Self) -> Option<Self> {
        use Numeric::{Decimal as D, Double as Db, Float as F, Integer as I};
        Some(match (self, other) {
            (I(a), I(b)) => I(a.checked_add(b)?),
            (I(_) | D(_), I(_) | D(_)) => D(self.decimal()?.checked_add(other.decimal()?)?),
            (I(_) | D(_) | F(_), I(_) | D(_) | F(_)) => F(self.float()? + other.float()?),
            _ => Db(self.double() + other.double()),
        })
    }

    fn decimal(self) -> Option<Decimal> {
        match self {
            Self::Integer(i) => Some(Decimal::from(i)),
            Self::Decimal(d) => Some(d),
            _ => None,
        }
    }

    fn float(self) -> Option<Float> {
        match self {
            Self::Integer(i) => Some(Float::from(i)),
            Self::Decimal(d) => Some(Float::from(d)),
            Self::Float(f) => Some(f),
            Self::Double(_) => None,
        }
    }

    fn double(self) -> Double {
        match self {
            Self::Integer(i) => Double::from(i),
            Self::Decimal(d) => Double::from(d),
            Self::Float(f) => Double::from(f),
            Self::Double(d) => d,
        }
    }

    fn term(self) -> Term {
        let (lexical, datatype) = match self {
            Self::Integer(i) => (i.to_string(), xsd::INTEGER),
            Self::Decimal(d) => (d.to_string(), xsd::DECIMAL),
            Self::Float(f) => (f.to_string(), xsd::FLOAT),
            Self::Double(d) => (d.to_string(), xsd::DOUBLE),
        };
        Literal::new_typed_literal(lexical, datatype).into()
    }
}

fn sum(values: &[Term]) -> Option<Term> {
    let mut total = Numeric::Integer(Integer::from(0));
    for value in values {
        total = total.add(Numeric::of(value)?)?;
    }
    Some(total.term())
}

fn average(values: &[Term]) -> Option<Term> {
    if values.is_empty() {
        return Some(integer(0));
    }
    let mut total = Numeric::Integer(Integer::from(0));
    for value in values {
        total = total.add(Numeric::of(value)?)?;
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

/// Columns of the variables `left` and `right` share, pairwise.
fn shared_columns(left: &Solutions, right: &Solutions) -> (Vec<usize>, Vec<usize>) {
    left.vars
        .iter()
        .enumerate()
        .filter_map(|(l, v)| Some((l, right.column(v)?)))
        .unzip()
}

fn joined_vars(left: &Solutions, right: &Solutions, right_keys: &[usize]) -> Vec<Variable> {
    let mut vars = left.vars.clone();
    vars.extend(
        right
            .vars
            .iter()
            .enumerate()
            .filter(|(c, _)| !right_keys.contains(c))
            .map(|(_, v)| v.clone()),
    );
    vars
}

fn has_undef(table: &IdTable, columns: &[usize]) -> bool {
    columns.iter().any(|&c| table.column(c).contains(&UNDEF))
}

fn triple_variables(triple: &TriplePattern) -> Vec<Variable> {
    let term = |t: &TermPattern| match t {
        TermPattern::Variable(v) => Some(v.clone()),
        TermPattern::BlankNode(b) => {
            Some(Variable::new_unchecked(format!("_bnode_{}", b.as_str())))
        }
        _ => None,
    };
    let predicate = match &triple.predicate {
        NamedNodePattern::Variable(v) => Some(v.clone()),
        NamedNodePattern::NamedNode(_) => None,
    };
    [term(&triple.subject), predicate, term(&triple.object)]
        .into_iter()
        .flatten()
        .collect()
}

#[derive(Clone, Debug)]
enum Slot {
    Var(Variable),
    Const(TermId),
}

impl Slot {
    fn is_var(&self, variable: &Variable) -> bool {
        matches!(self, Slot::Var(v) if v == variable)
    }
}

/// A triple pattern with constants resolved to ids, in a graph: `slots[3]` is the default
/// graph, a named graph, or a graph variable (any named graph, inside `GRAPH ?g`).
#[derive(Clone, Debug)]
struct ScanPattern {
    slots: [Slot; 4],
}

impl ScanPattern {
    fn quad_pattern(&self) -> QuadPattern {
        let constant = |slot: &Slot| match slot {
            Slot::Const(id) => Some(*id),
            Slot::Var(_) => None,
        };
        QuadPattern {
            subject: constant(&self.slots[0]),
            predicate: constant(&self.slots[1]),
            object: constant(&self.slots[2]),
            graph: match &self.slots[3] {
                Slot::Const(id) => GraphSelector::Exact(*id),
                Slot::Var(_) => GraphSelector::AnyNamed,
            },
        }
    }

    /// True if the pattern is in the default graph (statistics, ranges and the
    /// worst-case-optimal join assume it).
    fn in_default_graph(&self) -> bool {
        matches!(self.slots[3], Slot::Const(id) if id == TermId::DEFAULT_GRAPH)
    }

    /// Distinct variables in subject, predicate, object, graph order.
    fn vars(&self) -> Vec<Variable> {
        let mut vars: Vec<Variable> = Vec::new();
        for slot in &self.slots {
            if let Slot::Var(v) = slot
                && !vars.contains(v)
            {
                vars.push(v.clone());
            }
        }
        vars
    }

    /// The first component of `permutation`'s order that this pattern leaves unbound.
    fn first_free(&self, permutation: Permutation) -> Option<usize> {
        permutation
            .order()
            .into_iter()
            .find(|&c| matches!(self.slots[c], Slot::Var(_)))
    }

    fn repeats_variable(&self) -> bool {
        let mut names: Vec<&Variable> = self
            .slots
            .iter()
            .filter_map(|s| match s {
                Slot::Var(v) => Some(v),
                Slot::Const(_) => None,
            })
            .collect();
        let before = names.len();
        names.sort_by_key(|v| v.as_str());
        names.dedup();
        names.len() != before
    }

    /// A permutation whose free part starts with `sort_var`, if one exists; any usable one
    /// otherwise. A constant graph takes a graph-first order, a graph variable a graph-last
    /// one (its graph range is not a prefix).
    fn permutation_for(&self, sort_var: Option<&Variable>) -> Permutation {
        const GRAPH_FIRST: [Permutation; 4] = [
            Permutation::Gspo,
            Permutation::Gpos,
            Permutation::Gosp,
            Permutation::Gpso,
        ];
        const GRAPH_LAST: [Permutation; 3] =
            [Permutation::Spog, Permutation::Posg, Permutation::Ospg];
        let candidates: &[Permutation] = match self.slots[3] {
            Slot::Const(_) => &GRAPH_FIRST,
            Slot::Var(_) => &GRAPH_LAST,
        };
        let bound = |component: usize| matches!(self.slots[component], Slot::Const(_));
        let usable = |p: &Permutation| {
            let order = p.order();
            let prefix = order.iter().take_while(|&&c| bound(c)).count();
            order[prefix..].iter().all(|&c| !bound(c))
        };
        let first_free = |p: &Permutation| p.order().into_iter().find(|&c| !bound(c));
        let wanted = sort_var.and_then(|v| (0..4).find(|&c| self.slots[c].is_var(v)));
        candidates
            .iter()
            .copied()
            .filter(usable)
            .find(|p| wanted.is_none() || first_free(p) == wanted)
            .or_else(|| candidates.iter().copied().find(usable))
            .expect("GSPO or SPOG/POSG/OSPG answer every pattern with a bound prefix")
    }
}
