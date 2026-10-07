//! The native query executor (execution-core design, XC3): SPARQL algebra evaluated over id
//! tables with the shared execution core (`nrese-exec`, decision D11).
//!
//! **The only executor.** Every query and update runs here. What it doesn't implement is
//! an error ([`NativeError::Unsupported`], `QueryEvaluationError::Unsupported`), found
//! before evaluation where the algebra shows it ([`supported`]).
//!
//! **Execution.** Intermediate results are [`IdTable`]s of term ids; terms are decoded only
//! for expressions and for the output. BGPs are ordered greedily by *exact* pattern counts
//! (`Snapshot::count`), joined by index nested loops when the running result is much
//! smaller than the next pattern, and otherwise by merge joins on sorted scans or hash joins.
//! `COUNT(*)` over a single pattern reads the count from the index.

mod cache_key;
mod cached;
pub(crate) use cached::output_key;
mod calendar;
mod equality;
mod equijoin;
mod estimate;
mod exists;
pub(crate) mod expr;
mod fast;
mod federation;
mod geo;
mod geo_formats;
mod late;
mod lateral;

/// The reference system (an IRI) and geometry of a GeoSPARQL literal: WKT, GeoJSON or
/// GML; `None` for other terms. EPSG:4326 is read as CRS84 (longitude first).
pub fn geometry_literal(term: &nrese_rdf::Term) -> Option<(String, ::geo::Geometry<f64>)> {
    geo::parse(term).map(|shape| (shape.crs, shape.geometry))
}
mod output;
mod path_joins;
mod paths;
mod plan;
mod pushdown;
pub(crate) use pushdown::per_solution;
pub(crate) mod ql;
mod ranges;
mod search;
mod sets;
mod sideways;
mod spatial;
mod stream;
mod strings;
mod substitute;
mod triple_terms;
pub(crate) mod value;
mod vectors;
mod wcoj;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Instant;

use crate::results::{
    CancellationToken, QueryEvaluationError, QueryResults, QuerySolutionIter, QueryTripleIter,
};
use nrese_engine::quad::Permutation;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::join::{
    anti_join_in_place, compatible_mask, join, join_keeping_left_order, join_with_undef, left_join,
    outer_join_with_undef,
};
use nrese_exec::{
    Budget, BudgetExceeded, IdTable, UNDEF, computed_id, computed_index, group::group_rows,
};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{Literal, Term, Variable};
use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern, OrderExpression,
};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};
use nrese_xsd::{Decimal, Double, Float, Integer};
use rayon::prelude::*;

/// The most numbers kept per aggregate argument between groupings ([`Context::numeric_pass`]).
const NUMBER_MEMO_ENTRIES: usize = 1 << 22;

/// The rows of a cross product made at once below a GROUP BY ([`Context::group_crossed`]).
const CROSS_CHUNK_ROWS: usize = 1 << 22;

/// Distinct values of a sort key from which their terms are decoded in parallel.
const PARALLEL_RANKS: usize = 4096;
use std::borrow::Cow;

use crate::query::{PlanStep, QueryOptions};
use expr::Evaluator;
use value::Value;

/// A right side at least this many times larger than the running result is joined by
/// probing the index once per result row instead of scanning it.
const PROBE_FACTOR: u64 = 32;

/// Rows of the first pattern in the first (and smallest) morsel of a basic graph pattern
/// under LIMIT.
const MIN_MORSEL: usize = 1024;

/// A range scan over at least this many id ranges scans the whole pattern and keeps the
/// rows whose object is in one ([`IdSet`]).
const MANY_RANGES: usize = 1024;

/// Result rows per parallel task of an index nested-loop join; smaller results probe on
/// one thread.
const PROBE_CHUNK: usize = 4096;

/// Decoded terms kept per query for repeated ids (ORDER BY, aggregates, expressions).
const DECODE_CACHE_ENTRIES: usize = 1 << 18;

pub(crate) enum NativeError {
    /// Something the executor doesn't implement.
    Unsupported(String),
    Evaluation(QueryEvaluationError),
}

impl From<QueryEvaluationError> for NativeError {
    fn from(error: QueryEvaluationError) -> Self {
        Self::Evaluation(error)
    }
}

impl From<NativeError> for QueryEvaluationError {
    fn from(error: NativeError) -> Self {
        match error {
            NativeError::Unsupported(what) => QueryEvaluationError::Unsupported(what),
            NativeError::Evaluation(error) => error,
        }
    }
}

fn unsupported<T>(what: impl Into<String>) -> NativeResult<T> {
    Err(NativeError::Unsupported(what.into()))
}

type NativeResult<T> = Result<T, NativeError>;

/// Runs `query` on `snapshot` (borrowed from the view, or owned: a transaction's pending
/// state). The solutions are computed here and decoded as they are read.
pub(crate) fn evaluate<'a>(
    snapshot: Cow<'a, Snapshot>,
    query: &Query,
    options: &QueryOptions,
) -> Result<QueryResults<'a>, QueryEvaluationError> {
    let ctx = Context::new(&snapshot, options, query_dataset(query), query_base(query));
    let (pattern, form, _, _) = native_pattern(query, options, &ctx)?;
    let pattern = match &options.pre_bound {
        Some(values) => {
            // A blank node is put in as an alias IRI that stands for the node.
            for term in values.values() {
                if let nrese_rdf::Term::BlankNode(b) = term
                    && let Some(id) = snapshot.lookup(term.as_ref())
                {
                    ctx.register_alias(substitute::alias(b.as_str()).as_str(), id.raw());
                }
            }
            substitute::Values { terms: values }.top(&pattern)
        }
        None => pattern,
    };
    let solutions = ctx.eval_root(&pattern, options.pin.as_ref())?;
    match form {
        Form::Select => {}
        Form::Describe => {
            let triples = ctx.describe(&solutions)?;
            return Ok(QueryResults::Graph(QueryTripleIter::new(
                triples.into_iter().map(Ok),
            )));
        }
        Form::Ask => return Ok(QueryResults::Boolean(!solutions.table.is_empty())),
        Form::Construct(template) => {
            let computed = ctx.computed.into_inner();
            let triples: Vec<_> = construct(&snapshot, computed, solutions, template).collect();
            return Ok(QueryResults::Graph(QueryTripleIter::new(
                triples.into_iter().map(Ok),
            )));
        }
    }
    let variables: Arc<[Variable]> = solutions.vars.clone().into();
    let computed = ctx.computed.into_inner();
    let table = solutions.table;
    let rows = (0..table.len()).map(move |row| {
        Ok((0..table.width())
            .map(|column| decode(&snapshot, &computed, table.get(row, column)))
            .collect::<Vec<_>>())
    });
    Ok(QueryResults::Solutions(QuerySolutionIter::new(
        variables, rows,
    )))
}

/// Runs `query`, recording each operator ([`PlanStep`]); returns the rewrites that changed
/// the query ([`optimise`]), the steps and the number of solutions (triples for CONSTRUCT
/// and DESCRIBE).
pub(crate) fn explain(
    snapshot: &Snapshot,
    query: &Query,
    options: &QueryOptions,
) -> Result<ExplainParts, QueryEvaluationError> {
    let mut ctx = Context::new(snapshot, options, query_dataset(query), query_base(query));
    let (pattern, form, rewrites, ql) = native_pattern(query, options, &ctx)?;
    ctx.trace = Some(RefCell::default());
    let solutions = ctx.eval_root(&pattern, options.pin.as_ref())?;
    let steps = ctx.trace.take().unwrap_or_default().into_inner();
    // CONSTRUCT and DESCRIBE count triples.
    let rows = match form {
        Form::Select | Form::Ask => solutions.table.len(),
        Form::Describe => ctx.describe(&solutions)?.len(),
        Form::Construct(template) => {
            construct(snapshot, ctx.computed.into_inner(), solutions, template).count()
        }
    };
    Ok((rewrites, steps, rows as u64, ql))
}

/// The plan `query` would run as, after the rewrites, with estimated rows per node
/// ([`estimate`]), without running it; and the rewrites that changed it.
pub(crate) fn plan(
    snapshot: &Snapshot,
    query: &Query,
    options: &QueryOptions,
) -> Result<PlanParts, QueryEvaluationError> {
    let ctx = Context::new(snapshot, options, query_dataset(query), query_base(query));
    let (pattern, _, rewrites, ql) = native_pattern(query, options, &ctx)?;
    let mut steps = Vec::new();
    ctx.estimate_plan(&crate::plan::Plan::of(&pattern), 0, &mut steps);
    Ok((rewrites, steps, ql))
}

/// The rewrites, the steps with their rows, the rows, and what the QL rewriting did.
type ExplainParts = (
    Vec<&'static str>,
    Vec<PlanStep>,
    u64,
    Option<crate::ql::QlReport>,
);

/// The rewrites, the planned steps, and what the QL rewriting did.
type PlanParts = (
    Vec<&'static str>,
    Vec<crate::query::PlannedStep>,
    Option<crate::ql::QlReport>,
);

/// What the QL rewriting does to `query` (`None` where it doesn't apply), without
/// running it: the status that goes with its answers.
pub(crate) fn ql_report(
    snapshot: &Snapshot,
    query: &Query,
    options: &QueryOptions,
) -> Result<Option<crate::ql::QlReport>, QueryEvaluationError> {
    if ql_rewriting(query, options).is_none() {
        return Ok(None);
    }
    // Only the steps before the rewriting, not the optimiser: this runs once more per
    // query, ahead of its answers.
    let (pattern, form) = pattern_and_form(query);
    let rewritten;
    let pattern = if triple_terms::has_open(pattern) {
        rewritten = triple_terms::rewrite(pattern);
        &rewritten
    } else {
        pattern
    };
    Ok(ql_stage(query, options, snapshot, pattern, &form).1)
}

/// A query's pattern and form.
fn pattern_and_form(query: &Query) -> (&GraphPattern, Form<'_>) {
    match query {
        Query::Select { pattern, .. } => (pattern, Form::Select),
        Query::Ask { pattern, .. } => (pattern, Form::Ask),
        Query::Construct {
            template, pattern, ..
        } => (pattern, Form::Construct(template)),
        Query::Describe { pattern, .. } => (pattern, Form::Describe),
    }
}

/// The QL rewriting of a query's pattern: the pattern rewritten (`None` if unchanged), and
/// what it did (`None` where it doesn't apply). A schema with nothing to rewrite leaves the
/// pattern unread.
fn ql_stage(
    query: &Query,
    options: &QueryOptions,
    snapshot: &Snapshot,
    pattern: &GraphPattern,
    form: &Form<'_>,
) -> (Option<GraphPattern>, Option<crate::ql::QlReport>) {
    let Some(ql) = ql_rewriting(query, options) else {
        return (None, None);
    };
    let tbox = ql.tbox(snapshot, options.access.as_deref());
    // What its `complete` refers to: the closure it rewrites over.
    let regime = match ql.closure().lists {
        true => crate::Regime::Owl2Rl,
        false => crate::Regime::Owl2Ql,
    };
    if tbox.is_empty() {
        let mut report = crate::ql::QlReport::default();
        report.completeness.regime = Some(regime);
        return (None, Some(report));
    }
    let (needed, set) = match form {
        Form::Select => (None, false),
        Form::Ask => (Some(Vec::new()), true),
        Form::Construct(template) => {
            let mut vars = Vec::new();
            GraphPattern::Bgp {
                patterns: template.to_vec(),
            }
            .on_in_scope_variable(|v| vars.push(v.clone()));
            (Some(vars), true)
        }
        Form::Describe => (None, true),
    };
    // A witness the data already has wherever it folds adds nothing (design §3), asked of
    // the data the query reads: for a reader of every graph (the cache is the store's).
    let data = options.access.is_none().then(|| ql::DataCheck {
        ql,
        options: QueryOptions {
            ql: None,
            ..options.clone()
        },
    });
    let (out, mut report) = ql::rewrite_query(
        pattern,
        &tbox,
        snapshot,
        ql.limits(),
        needed,
        set,
        data.as_ref(),
    );
    report.completeness.regime = Some(regime);
    let changed = report.patterns > 0;
    (changed.then_some(out), Some(report))
}

pub use output::ResultsFormat;

/// Writes the results of a SELECT (or, in JSON, ASK) straight from the id table
/// ([`output`]); `None` for CONSTRUCT, DESCRIBE and ASK in TSV or CSV (use the general
/// path).
pub(crate) fn write_results(
    snapshot: &Snapshot,
    query: &Query,
    options: &QueryOptions,
    format: ResultsFormat,
    version: Option<&'static str>,
    out: &mut dyn std::io::Write,
) -> Option<Result<(), crate::query::WriteResultsError>> {
    let ctx = Context::new(snapshot, options, query_dataset(query), query_base(query));
    let (pattern, form, _, _) = match native_pattern(query, options, &ctx) {
        Ok(native) => native,
        Err(error) => return Some(Err(error.into())),
    };
    match form {
        Form::Construct(_) | Form::Describe => return None,
        Form::Ask if format != ResultsFormat::Json => return None,
        _ => {}
    }
    let ctx = Context::new(snapshot, options, query_dataset(query), query_base(query));
    let solutions = match ctx.eval_root(&pattern, options.pin.as_ref()) {
        Ok(solutions) => solutions,
        Err(error) => return Some(Err(QueryEvaluationError::from(error).into())),
    };
    if matches!(form, Form::Ask) {
        let bytes = output::boolean(!solutions.table.is_empty(), version);
        return Some(out.write_all(&bytes).map_err(Into::into));
    }
    let computed = ctx.computed.into_inner();
    let token = options.cancellation.clone();
    let cancelled = || token.as_ref().is_some_and(CancellationToken::is_cancelled);
    let alive = || {
        if cancelled() {
            Err(std::io::Error::other("query cancelled"))
        } else {
            Ok(())
        }
    };
    let writer = output::DirectResults {
        snapshot,
        computed: &computed,
        format,
        version,
    };
    Some(
        writer
            .write(&solutions.vars, &solutions.table, out, &alive)
            .map_err(|error| {
                if cancelled() {
                    QueryEvaluationError::Cancelled.into()
                } else {
                    error.into()
                }
            }),
    )
}

/// The quads an update removes and adds, in that order.
pub(crate) type QuadChanges = (Vec<nrese_rdf::Quad>, Vec<nrese_rdf::Quad>);

/// The quads a `DELETE`/`INSERT … WHERE` removes and adds, with its `WHERE` evaluated on
/// `snapshot`. `using`
/// is the operation's dataset (`USING`, `WITH`); the protocol's, in `options.dataset`,
/// replaces it. Templates
/// are filled as SPARQL 1.1 Update §3.1.3 says: a quad with an unbound or ill-placed term (a literal
/// subject or graph, a non-IRI predicate) is skipped, and inserted blank nodes are fresh per
/// solution.
pub(crate) fn delete_insert(
    snapshot: &Snapshot,
    pattern: &GraphPattern,
    delete: &[nrese_sparql_syntax::term::GroundQuadPattern],
    insert: &[nrese_sparql_syntax::term::QuadPattern],
    using: Option<&nrese_sparql_syntax::algebra::QueryDataset>,
    base: Option<&nrese_rdf::Iri<String>>,
    options: &QueryOptions,
) -> Result<QuadChanges, QueryEvaluationError> {
    use nrese_sparql_syntax::term::{GraphNamePattern, GroundTermPattern};
    let rewritten;
    let pattern = if triple_terms::has_open(pattern) {
        rewritten = triple_terms::rewrite(pattern);
        &rewritten
    } else {
        pattern
    };
    if let Some(what) = unsupported_part(pattern) {
        return Err(QueryEvaluationError::Unsupported(what));
    }
    let ctx = Context::new(snapshot, options, using, base);
    let pattern = optimise(pattern.clone(), &mut Vec::new(), &ctx);
    let solutions = ctx.eval(&pattern)?;
    let computed = ctx.computed.into_inner();
    let table = &solutions.table;
    let (mut deletes, mut inserts) = (Vec::new(), Vec::new());
    for row in 0..table.len() {
        let value = |v: &Variable| -> Option<Term> {
            decode(snapshot, &computed, table.get(row, solutions.column(v)?))
        };
        let subject = |term: Term| match term {
            Term::NamedNode(n) => Some(nrese_rdf::NamedOrBlankNode::from(n)),
            Term::BlankNode(b) => Some(nrese_rdf::NamedOrBlankNode::from(b)),
            Term::Literal(_) | Term::Triple(_) => None,
        };
        let predicate = |p: &NamedNodePattern| match p {
            NamedNodePattern::NamedNode(n) => Some(n.clone()),
            NamedNodePattern::Variable(v) => match value(v)? {
                Term::NamedNode(n) => Some(n),
                _ => None,
            },
        };
        let graph = |g: &GraphNamePattern| match g {
            GraphNamePattern::NamedNode(n) if crate::compat::names_default_graph(n.as_str()) => {
                Some(nrese_rdf::GraphName::DefaultGraph)
            }
            GraphNamePattern::NamedNode(n) => Some(nrese_rdf::GraphName::from(n.clone())),
            GraphNamePattern::DefaultGraph => Some(nrese_rdf::GraphName::DefaultGraph),
            GraphNamePattern::Variable(v) => match value(v)? {
                Term::NamedNode(n) => Some(n.into()),
                Term::BlankNode(b) => Some(b.into()),
                Term::Literal(_) | Term::Triple(_) => None,
            },
        };
        let ground = |t: &GroundTermPattern| fill_ground(t, &value);
        for quad in delete {
            let filled = (|| {
                Some(nrese_rdf::Quad::new(
                    subject(ground(&quad.subject)?)?,
                    predicate(&quad.predicate)?,
                    ground(&quad.object)?,
                    graph(&quad.graph_name)?,
                ))
            })();
            deletes.extend(filled);
        }
        let mut fresh: HashMap<String, nrese_rdf::BlankNode> = HashMap::new();
        let mut term = |t: &TermPattern| fill_template(t, &value, &mut fresh);
        for quad in insert {
            let Some(s) = term(&quad.subject).and_then(subject) else {
                continue;
            };
            let Some(o) = term(&quad.object) else {
                continue;
            };
            let (Some(p), Some(g)) = (predicate(&quad.predicate), graph(&quad.graph_name)) else {
                continue;
            };
            inserts.push(nrese_rdf::Quad::new(s, p, o, g));
        }
    }
    Ok((deletes, inserts))
}

/// What a query returns.
enum Form<'q> {
    Select,
    Ask,
    /// The triples of the template, per solution.
    Construct(&'q [TriplePattern]),
    /// The statements about each term the solutions bind ([`Context::describe`]).
    Describe,
}

/// A query as the executor runs it: its pattern, its form, the rewrites that changed it,
/// and what the QL rewriting did.
type NativePattern<'q> = (
    GraphPattern,
    Form<'q>,
    Vec<&'static str>,
    Option<crate::ql::QlReport>,
);

/// The pattern of a query the native executor runs, as it runs it ([`optimise`]), the
/// query form, and the rewrites that changed it.
fn native_pattern<'q>(
    query: &'q Query,
    options: &QueryOptions,
    ctx: &Context<'_>,
) -> Result<NativePattern<'q>, QueryEvaluationError> {
    let (pattern, form) = pattern_and_form(query);
    let mut rewrites = Vec::new();
    // SPARQL 1.2 triple-term patterns with variables, as plain algebra.
    let rewritten;
    let pattern = if triple_terms::has_open(pattern) {
        rewritten = triple_terms::rewrite(pattern);
        rewrites.push("triple-terms");
        &rewritten
    } else {
        pattern
    };
    // OWL 2 QL answers through existentials, before the optimiser, also as written: it
    // changes the answers, not just the plan.
    let (ql_pattern, ql_report) = ql_stage(query, options, &ctx.snapshot, pattern, &form);
    if let Some(report) = &ql_report {
        if report.patterns > 0 {
            rewrites.push("ql-tree-witness");
        }
        if !report.limits.is_empty() {
            rewrites.push("ql-limit");
        }
    }
    let rewritten_ql;
    let pattern = match ql_pattern {
        Some(out) => {
            rewritten_ql = out;
            &rewritten_ql
        }
        None => pattern,
    };
    let template_supported = match &form {
        Form::Construct(template) => template.iter().all(supported_template_triple),
        _ => true,
    };
    if !supported(pattern) || !template_supported {
        return Err(QueryEvaluationError::Unsupported(
            unsupported_part(pattern).unwrap_or_else(|| "a construct of the query".to_owned()),
        ));
    }
    let pattern = if options.as_written {
        pattern.clone()
    } else {
        optimise(pattern.clone(), &mut rewrites, ctx)
    };
    // ASK needs one solution: LIMIT 1 lets the evaluation stop at it.
    let pattern = match form {
        Form::Ask if !options.as_written => {
            rewrites.push("ask-limit");
            GraphPattern::Slice {
                inner: Box::new(pattern),
                start: 0,
                length: Some(1),
            }
        }
        _ => pattern,
    };
    Ok((pattern, form, rewrites, ql_report))
}

/// The QL rewriting, if it applies to `query`: on, and the query reads the inferred
/// statements of the default graph without a dataset or pre-bound variables, by a reader
/// who sees inferences (docs/design/ql-rewriting.md §1).
fn ql_rewriting<'o>(
    query: &Query,
    options: &'o QueryOptions,
) -> Option<&'o crate::ql::QlRewriting> {
    let ql = options.ql.as_deref()?;
    (options.read_model == ReadModel::Materialised
        && options.dataset.is_none()
        && query_dataset(query).is_none()
        // A reader who sees no inferences gets no answers through them.
        && options.access.as_deref().is_none_or(|a| a.inferred)
        && options.pre_bound.is_none())
    .then_some(ql)
}

/// The rewrites the executor applies to a query's (or an update's) pattern, in order;
/// `fired` gets the name of each that changed it, for EXPLAIN.
///
/// - `join-groups`: groups joined to each other become one basic graph pattern, so their
///   triple patterns are ordered together ([`crate::plan::Plan::flatten_joins`]).
/// - `eager-aggregation`: a group over a join aggregates the side its aggregates read
///   before the join ([`crate::plan::Plan::eager_aggregation`]).
/// - `filter-pushdown`: each filter moves to the smallest sub-pattern that binds its
///   variables ([`pushdown`]).
fn optimise(
    pattern: GraphPattern,
    fired: &mut Vec<&'static str>,
    ctx: &Context<'_>,
) -> GraphPattern {
    let pays = |side: &crate::plan::Plan, keys: &[Variable]| ctx.pre_aggregation_pays(side, keys);
    let eager = |pattern: &GraphPattern| crate::plan::eager_aggregation_where(pattern, &pays);
    type Pass<'p> = &'p dyn Fn(&GraphPattern) -> GraphPattern;
    let passes: [(&'static str, Pass<'_>); 3] = [
        ("join-groups", &crate::plan::rewrite),
        ("eager-aggregation", &eager),
        ("filter-pushdown", &|pattern| {
            pushdown::push_filters(pattern.clone())
        }),
    ];
    passes.into_iter().fold(pattern, |pattern, (name, pass)| {
        let rewritten = pass(&pattern);
        if rewritten != pattern {
            fired.push(name);
        }
        rewritten
    })
}

/// A description of the first part of `pattern` the executor doesn't implement.
fn unsupported_part(pattern: &GraphPattern) -> Option<String> {
    if supported(pattern) {
        return None;
    }
    let text = pattern.to_string();
    Some(format!(
        "the pattern {}",
        text.chars().take(200).collect::<String>()
    ))
}

/// A CONSTRUCT template's term against the solution columns; template blank nodes are
/// numbered by label (`labels`).
fn resolve_template(
    term: &TermPattern,
    solutions: &Solutions,
    labels: &mut Vec<String>,
) -> TemplateTerm {
    match term {
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
        TermPattern::Triple(t) => {
            let predicate = match &t.predicate {
                NamedNodePattern::NamedNode(n) => TemplateTerm::Constant(n.clone().into()),
                NamedNodePattern::Variable(v) => solutions
                    .column(v)
                    .map_or(TemplateTerm::Unbound, TemplateTerm::Column),
            };
            TemplateTerm::Triple(Box::new([
                resolve_template(&t.subject, solutions, labels),
                predicate,
                resolve_template(&t.object, solutions, labels),
            ]))
        }
    }
}

/// A position of a CONSTRUCT template, resolved against the solution columns.
enum TemplateTerm {
    /// A triple term of the template, its parts resolved.
    Triple(Box<[TemplateTerm; 3]>),
    Constant(Term),
    Column(usize),
    /// A variable the solutions don't bind: the template triple never applies.
    Unbound,
    /// The template's n-th blank node label: a fresh blank node per solution.
    Fresh(usize),
}

/// The triples of `template` instantiated with every solution (SPARQL 1.1 §16.2): template
/// blank nodes are fresh per solution, triples with an unbound or ill-placed term (a
/// literal subject, a non-IRI predicate) are skipped, and repeated triples without blank
/// nodes are emitted once (the memory of emitted triples is bounded).
fn construct<'a>(
    snapshot: &'a Snapshot,
    computed: Vec<Term>,
    solutions: Solutions,
    template: &[TriplePattern],
) -> impl Iterator<Item = nrese_rdf::Triple> + 'a {
    let mut labels: Vec<String> = Vec::new();
    let mut resolve = |term: &TermPattern| resolve_template(term, &solutions, &mut labels);
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
    let mut emitted: HashSet<nrese_rdf::Triple> = HashSet::new();
    let mut buffer: Vec<nrese_rdf::Triple> = Vec::new();
    let mut row = 0;
    std::iter::from_fn(move || {
        loop {
            if let Some(triple) = buffer.pop() {
                return Some(triple);
            }
            if row >= table.len() {
                return None;
            }
            let fresh: Vec<nrese_rdf::BlankNode> = (0..fresh_count)
                .map(|_| nrese_rdf::BlankNode::default())
                .collect();
            fn template_value(
                term: &TemplateTerm,
                column: &dyn Fn(usize) -> Option<Term>,
                fresh: &[nrese_rdf::BlankNode],
            ) -> Option<Term> {
                match term {
                    TemplateTerm::Constant(term) => Some(term.clone()),
                    TemplateTerm::Column(c) => column(*c),
                    TemplateTerm::Unbound => None,
                    TemplateTerm::Fresh(i) => Some(fresh[*i].clone().into()),
                    TemplateTerm::Triple(parts) => {
                        let subject = match template_value(&parts[0], column, fresh)? {
                            Term::NamedNode(n) => nrese_rdf::NamedOrBlankNode::from(n),
                            Term::BlankNode(b) => b.into(),
                            Term::Literal(_) | Term::Triple(_) => return None,
                        };
                        let Term::NamedNode(predicate) = template_value(&parts[1], column, fresh)?
                        else {
                            return None;
                        };
                        let object = template_value(&parts[2], column, fresh)?;
                        Some(nrese_rdf::Triple::new(subject, predicate, object).into())
                    }
                }
            }
            let column = |c: usize| decode(snapshot, &computed, table.get(row, c));
            let value = |term: &TemplateTerm| template_value(term, &column, &fresh);
            for [s, p, o] in &resolved {
                let subject = match value(s) {
                    Some(Term::NamedNode(n)) => nrese_rdf::NamedOrBlankNode::from(n),
                    Some(Term::BlankNode(b)) => nrese_rdf::NamedOrBlankNode::from(b),
                    _ => continue,
                };
                let Some(Term::NamedNode(predicate)) = value(p) else {
                    continue;
                };
                let Some(object) = value(o) else {
                    continue;
                };
                let triple = nrese_rdf::Triple::new(subject, predicate, object);
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

/// The query's own dataset (`FROM`, `FROM NAMED`), if it names one.
fn query_dataset(query: &Query) -> Option<&nrese_sparql_syntax::algebra::QueryDataset> {
    let (Query::Select { dataset, .. }
    | Query::Ask { dataset, .. }
    | Query::Construct { dataset, .. }
    | Query::Describe { dataset, .. }) = query;
    dataset.as_ref()
}

/// The query's `BASE`, against which `IRI()` resolves relative IRIs.
fn query_base(query: &Query) -> Option<&nrese_rdf::Iri<String>> {
    let (Query::Select { base_iri, .. }
    | Query::Ask { base_iri, .. }
    | Query::Construct { base_iri, .. }
    | Query::Describe { base_iri, .. }) = query;
    base_iri.as_ref()
}

/// True if [`evaluate`] handles `query` (barring runtime fallbacks).
pub(crate) fn query_supported(query: &Query) -> bool {
    match query {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Describe { pattern, .. } => supported_rewritten(pattern),
        Query::Construct {
            template, pattern, ..
        } => supported_rewritten(pattern) && template.iter().all(supported_template_triple),
    }
}

/// [`supported`] for the pattern as it runs, triple-term patterns rewritten.
fn supported_rewritten(pattern: &GraphPattern) -> bool {
    if triple_terms::has_open(pattern) {
        supported(&triple_terms::rewrite(pattern))
    } else {
        supported(pattern)
    }
}

/// A template's triple: any term, triple terms with variables included.
fn supported_template_triple(triple: &TriplePattern) -> bool {
    fn term(t: &TermPattern) -> bool {
        match t {
            TermPattern::Triple(t) => term(&t.subject) && term(&t.object),
            _ => true,
        }
    }
    term(&triple.subject) && term(&triple.object)
}

/// True if the native executor supports every operator in `pattern`.
pub(crate) fn supported(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => patterns.iter().all(supported_triple),
        GraphPattern::Graph { inner, .. } => supported(inner),
        // The block runs at the endpoint.
        GraphPattern::Service { .. } => true,
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
        } => {
            let mut outer = Vec::new();
            bound_variables(left, &mut outer);
            bound_variables(right, &mut outer);
            supported(left)
                && supported(right)
                && expression
                    .as_ref()
                    .is_none_or(|e| exists::supported_expression(e, &outer))
        }
        GraphPattern::Filter { expr, inner } => {
            let mut outer = Vec::new();
            bound_variables(inner, &mut outer);
            supported(inner) && exists::supported_expression(expr, &outer)
        }
        GraphPattern::Extend {
            inner, expression, ..
        } => {
            let mut outer = Vec::new();
            bound_variables(inner, &mut outer);
            supported(inner) && exists::supported_expression(expression, &outer)
        }
        GraphPattern::Values { .. } => true,
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
        GraphPattern::Lateral { left, right } => supported(left) && supported(right),
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
                                    | AggregateFunction::GroupConcat { .. }
                            )
                    }
                })
        }
    }
}

/// True if `GRAPH ?graph { pattern }` can be evaluated once for all graphs, every triple
/// pattern binding `?graph` from its scan: every solution then comes from triple patterns
/// in every branch, and the pattern holds nothing but triple patterns and the operators
/// that join them on `?graph` like on any variable.
///
/// Anything else is evaluated graph by graph: a row without a scan (VALUES, an empty
/// group, BIND alone), a property path (followed within one graph), a subquery or a nested
/// GRAPH (their `?graph` is another variable), MINUS (the shared `?graph` would make rows
/// with no other common variable remove each other) and a BIND to `?graph` itself.
fn scans_everywhere(pattern: &GraphPattern, graph: &Variable) -> bool {
    fn only_scans(pattern: &GraphPattern, graph: &Variable) -> bool {
        match pattern {
            GraphPattern::Bgp { .. } => true,
            GraphPattern::Join { left, right } | GraphPattern::Union { left, right } => {
                only_scans(left, graph) && only_scans(right, graph)
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                only_scans(left, graph)
                    && only_scans(right, graph)
                    && expression.as_ref().is_none_or(|e| filter(e, graph))
            }
            GraphPattern::Filter { inner, expr } => only_scans(inner, graph) && filter(expr, graph),
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => variable != graph && only_scans(inner, graph) && filter(expression, graph),
            _ => false,
        }
    }
    // `EXISTS` is a join on the shared variables, `?graph` among them.
    fn filter(expression: &Expression, graph: &Variable) -> bool {
        match expression {
            Expression::And(a, b) => filter(a, graph) && filter(b, graph),
            Expression::Exists(pattern) => only_scans(pattern, graph),
            Expression::Not(inner) if matches!(**inner, Expression::Exists(_)) => {
                filter(inner, graph)
            }
            // An `EXISTS` anywhere else is not a join.
            other => !pushdown::per_solution(other) || !contains_any_exists(other),
        }
    }
    fn rows_from_scans(pattern: &GraphPattern) -> bool {
        match pattern {
            GraphPattern::Bgp { patterns } => !patterns.is_empty(),
            GraphPattern::Join { left, right } => rows_from_scans(left) || rows_from_scans(right),
            GraphPattern::Union { left, right } => rows_from_scans(left) && rows_from_scans(right),
            GraphPattern::LeftJoin { left, .. } | GraphPattern::Minus { left, .. } => {
                rows_from_scans(left)
            }
            GraphPattern::Filter { inner, .. } | GraphPattern::Extend { inner, .. } => {
                rows_from_scans(inner)
            }
            _ => false,
        }
    }
    // Inside `GRAPH ?g { P }`, `?g` is bound only where P binds it (SPARQL 1.1 §18.6: P
    // is evaluated in each graph, then joined with `?g`). An expression reading `?g`
    // sees it unbound, which only the graph-by-graph evaluation gives.
    fn read_by_expressions(pattern: &GraphPattern, graph: &Variable) -> bool {
        match pattern {
            GraphPattern::Join { left, right } | GraphPattern::Union { left, right } => {
                read_by_expressions(left, graph) || read_by_expressions(right, graph)
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                read_by_expressions(left, graph)
                    || read_by_expressions(right, graph)
                    || expression
                        .as_ref()
                        .is_some_and(|e| exists::deep_variables(e).contains(graph))
            }
            GraphPattern::Filter { inner, expr: e }
            | GraphPattern::Extend {
                inner,
                expression: e,
                ..
            } => read_by_expressions(inner, graph) || exists::deep_variables(e).contains(graph),
            _ => false,
        }
    }
    fn bound_by_scans(pattern: &GraphPattern, graph: &Variable) -> bool {
        match pattern {
            GraphPattern::Bgp { patterns } => patterns.iter().any(|t| {
                [&t.subject, &t.object]
                    .into_iter()
                    .any(|term| matches!(term, TermPattern::Variable(v) if v == graph))
                    || matches!(&t.predicate, NamedNodePattern::Variable(v) if v == graph)
            }),
            GraphPattern::Join { left, right } | GraphPattern::Union { left, right } => {
                bound_by_scans(left, graph) || bound_by_scans(right, graph)
            }
            GraphPattern::LeftJoin { left, .. }
            | GraphPattern::Filter { inner: left, .. }
            | GraphPattern::Extend { inner: left, .. } => bound_by_scans(left, graph),
            _ => false,
        }
    }
    if read_by_expressions(pattern, graph) && !bound_by_scans(pattern, graph) {
        return false;
    }
    only_scans(pattern, graph) && rows_from_scans(pattern)
}

/// True if `expression` holds an `EXISTS` at any depth.
fn contains_any_exists(expression: &Expression) -> bool {
    expression.find(&mut |node| {
        matches!(
            node,
            nrese_sparql_syntax::visit::Node::Expression(Expression::Exists(_))
        )
    })
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
    let term = |t: &TermPattern| match t {
        // Ground triple terms are constants; others are rewritten first (`triple_terms`).
        TermPattern::Triple(t) => triple_terms::ground(t),
        _ => true,
    };
    term(&triple.subject) && term(&triple.object)
}

/// `v`'s place in `vars`, added at the end if it isn't there.
fn variable_index(vars: &mut Vec<Variable>, v: Variable) -> usize {
    vars.iter().position(|x| *x == v).unwrap_or_else(|| {
        vars.push(v);
        vars.len() - 1
    })
}

/// EXPLAIN's estimate of the rows `expression` keeps of `rows` ([`pushdown::selectivity`]).
fn filtered(rows: usize, expression: &Expression) -> Option<u64> {
    Some((rows as f64 * pushdown::selectivity(expression)).round() as u64)
}

/// A property path pattern, with the filter directly on it, if any.
struct PathPattern<'q> {
    subject: &'q TermPattern,
    path: &'q nrese_sparql_syntax::algebra::PropertyPathExpression,
    object: &'q TermPattern,
    filter: Option<&'q Expression>,
}

/// `pattern` as a path, alone or under a filter that needs no join (no `EXISTS`).
fn as_path(pattern: &GraphPattern) -> Option<PathPattern<'_>> {
    let (pattern, filter) = match pattern {
        GraphPattern::Filter { expr, inner } if !contains_any_exists(expr) => {
            (&**inner, Some(expr))
        }
        other => (other, None),
    };
    match pattern {
        GraphPattern::Path {
            subject,
            path,
            object,
        } => Some(PathPattern {
            subject,
            path,
            object,
            filter,
        }),
        _ => None,
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
/// order that matters (from ORDER BY): operators then keep it, so a
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
    /// The snapshot read; owned when it reads equality classes canonically
    /// ([`QueryOptions::equality_canonical`], [`Context::late_expansion`]).
    snapshot: Cow<'a, Snapshot>,
    /// Which statements the query reads (asserted, inferred or both).
    model: ReadModel,
    /// Alias IRIs that stand for blank nodes put into a pattern ([`substitute`]).
    aliases: RefCell<HashMap<String, u64>>,
    evaluator: Evaluator,
    computed: RefCell<Vec<Term>>,
    computed_ids: RefCell<HashMap<Term, u64>>,
    decoded: RefCell<HashMap<u64, Option<Term>>>,
    /// Per aggregate argument, the variable it reads and id: its number for SUM and AVG
    /// ([`Context::numeric_pass`]), kept for the whole evaluation. Keyed by the expression
    /// itself, not its address: patterns made during the evaluation (substituted
    /// subqueries) come and go, and a freed address may hold another expression next.
    numbers: RefCell<HashMap<(Expression, Variable), nrese_exec::IdMap<Number>>>,
    cancellation: Option<CancellationToken>,
    budget: Arc<Budget>,
    /// The operators run so far, for EXPLAIN; `None` when not explaining.
    trace: Option<RefCell<Vec<PlanStep>>>,
    /// Nesting depth of the operator being evaluated (for the trace).
    depth: Cell<usize>,
    /// The seeks of the last index nested-loop join, for EXPLAIN ([`Probe`]).
    probed: Cell<nrese_engine::SeekStats>,
    /// A LIMIT for the next basic graph pattern: any this many of its rows will do
    /// ([`Context::eval_limited`]). Taken by the pattern, so nothing nested sees it.
    limit: Cell<Option<usize>>,
    /// The graph that triple patterns match in (`GRAPH`).
    graph: RefCell<GraphScope>,
    /// [`QueryOptions::as_written`].
    as_written: bool,
    /// [`QueryOptions::cross_chunk_rows`].
    cross_chunk_rows: usize,
    /// [`QueryOptions::stream_rows`].
    stream_rows: Option<usize>,
    /// With a dataset of several default graphs (`FROM <a> FROM <b>`), the graphs whose
    /// merge the default graph is; the scope is then [`GraphScope::Union`].
    merge_set: Option<Vec<TermId>>,
    /// The graphs `GRAPH` may name, if the dataset lists them (`FROM NAMED`).
    named: Option<Vec<TermId>>,
    /// Numbers the columns the executor adds for itself ([`exists`]).
    synthetic: Cell<usize>,
    /// [`QueryOptions::services`].
    services: Option<crate::Services>,
    /// Whether `SERVICE` is refused to the requester ([`crate::GraphAccess::service`]).
    service_denied: bool,
    /// [`QueryOptions::equality_closed`].
    equality_closed: bool,
    /// The equality classes to expand after joins over canonical reads (stage C,
    /// [`Context::late_expansion`]); `None` where reads expand them, or don't need to.
    late: Option<Arc<nrese_engine::EqualityClasses>>,
    /// Whether GeoSPARQL relations in triple patterns are computed from geometries too
    /// (the query-rewrite extension; not [`QueryOptions::geosparql_stated_only`]).
    spatial_rewrite: bool,
    /// The result cache and this context's part of its keys ([`cached`]); `None` without
    /// one.
    cache: Option<Arc<cached::CacheScope>>,
    /// The name to pin the next part evaluated under: the query's whole pattern
    /// ([`Context::eval_root`]).
    pin: RefCell<Option<crate::cache::PinRequest>>,
}

/// The active graph of triple patterns.
#[derive(Clone, Debug)]
enum GraphScope {
    Default,
    /// The default graph as the merge of all graphs
    /// ([`QueryOptions::union_default_graph`]): every statement once.
    Union,
    Named(TermId),
    /// `GRAPH ?g`: any named graph, bound to the variable.
    Variable(Variable),
    /// `GRAPH <g>` for a graph the store doesn't know: nothing matches.
    Missing,
}

impl<'a> Context<'a> {
    /// Whether `triple` is a GeoSPARQL relation computed from geometries
    /// ([`spatial`]): not when only stated relations are read.
    fn is_spatial(&self, triple: &TriplePattern) -> bool {
        self.spatial_rewrite && spatial::is_spatial(triple)
    }

    /// [`spatial::split`], when relations are computed from geometries.
    #[allow(clippy::type_complexity)]
    fn spatial_split(
        &self,
        triples: &[TriplePattern],
    ) -> Option<(Vec<TriplePattern>, Vec<TriplePattern>, Vec<TriplePattern>)> {
        if self.spatial_rewrite {
            spatial::split(triples)
        } else {
            None
        }
    }

    /// `dataset` is the query's own (`FROM`); the protocol's, in `options`, replaces it.
    fn new(
        snapshot: &'a Snapshot,
        options: &QueryOptions,
        dataset: Option<&nrese_sparql_syntax::algebra::QueryDataset>,
        base: Option<&nrese_rdf::Iri<String>>,
    ) -> Self {
        use crate::dataset::{DefaultGraph, ResolvedDataset};
        let resolved = ResolvedDataset::resolve(
            snapshot,
            options.union_default_graph,
            options.dataset.as_ref(),
            dataset,
        );
        let resolved = match &options.access {
            Some(access) => resolved.restricted(snapshot, access),
            None => resolved,
        };
        let (scope, merge_set) = match resolved.default {
            DefaultGraph::Store => (GraphScope::Default, None),
            DefaultGraph::Graph(graph) => (GraphScope::Named(graph), None),
            DefaultGraph::Merge(graphs) => (GraphScope::Union, graphs),
            DefaultGraph::Empty => (GraphScope::Missing, None),
        };
        let mut ctx = Self {
            merge_set,
            named: resolved.named,
            as_written: options.as_written,
            spatial_rewrite: !options.geosparql_stated_only,
            cross_chunk_rows: options.cross_chunk_rows.unwrap_or(CROSS_CHUNK_ROWS).max(1),
            stream_rows: options.stream_rows,
            late: match options.equality_canonical || options.equality_early_expansion {
                true => None,
                false => snapshot.expand_late(options.read_model),
            },
            snapshot: match options.equality_canonical {
                true => Cow::Owned(snapshot.with_canonical_equality()),
                false => Cow::Borrowed(snapshot),
            },
            model: options.read_model,
            evaluator: Evaluator::with_base(base.cloned()),
            aliases: RefCell::default(),
            computed: RefCell::default(),
            computed_ids: RefCell::default(),
            decoded: RefCell::default(),
            numbers: RefCell::default(),
            cancellation: options.cancellation.clone(),
            budget: Arc::new(
                options
                    .memory_limit
                    .map_or_else(Budget::unlimited, Budget::new)
                    .within(options.shared_memory.clone()),
            ),
            trace: None,
            depth: Cell::new(0),
            probed: Cell::default(),
            limit: Cell::new(None),
            graph: RefCell::new(scope),
            synthetic: Cell::new(0),
            services: options.services.clone(),
            service_denied: options
                .access
                .as_ref()
                .is_some_and(|access| !access.service),
            equality_closed: options.equality_closed,
            cache: None,
            pin: RefCell::default(),
        };
        if let Some(cache) = &options.result_cache {
            // Besides the context's options, a result depends on the base IRI (`IRI()`)
            // and on the user's access, which may hide inferred statements.
            let fixed = format!("{base:?} {:?}", options.access);
            ctx.cache = Some(ctx.cache_scope(Arc::clone(cache), fixed.into_bytes().into()));
        }
        ctx
    }

    /// The id of a constant of a pattern: an alias IRI ([`substitute`]) stands for its
    /// blank node.
    fn lookup_const(&self, term: nrese_rdf::TermRef<'_>) -> Option<TermId> {
        if let nrese_rdf::TermRef::NamedNode(n) = term
            && n.as_str().starts_with(substitute::ALIAS)
        {
            return self
                .aliases
                .borrow()
                .get(n.as_str())
                .map(|&id| TermId::from_raw(id));
        }
        self.snapshot.lookup(term)
    }

    /// Makes `alias` stand for the blank node `id` in patterns and expressions.
    fn register_alias(&self, alias: &str, id: u64) {
        self.aliases.borrow_mut().insert(alias.to_owned(), id);
        if let Some(term) = self.term(id) {
            self.evaluator.register_alias(alias, term);
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
                cache: None,
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
            .map_err(QueryEvaluationError::MemoryLimit)?;
        Ok(solutions)
    }

    /// The most rows a table `width` columns wide may grow to within the remaining budget.
    /// Half of what the bytes would hold: a table that grows reallocates, and a join's
    /// parallel parts are copied into one table, so for a moment the rows exist twice.
    fn max_rows(&self, width: usize) -> usize {
        self.budget.remaining() / (width.max(1) * 8 * 2)
    }

    /// Charges a join's hash table over `rows` build rows (a key, a row number and the
    /// table's slack per row); the caller releases the returned bytes after the join.
    fn charge_hash_table(&self, rows: usize) -> NativeResult<usize> {
        let bytes = rows.saturating_mul(32);
        self.budget
            .charge(bytes)
            .map_err(QueryEvaluationError::MemoryLimit)?;
        Ok(bytes)
    }

    /// The error for an operator that would outgrow the budget.
    /// The row limit of a join: what the budget leaves for a table `width` columns wide,
    /// and the query's cancellation flag, which stops the join at its next row check.
    fn row_limit(&self, width: usize) -> nrese_exec::RowLimit {
        nrese_exec::RowLimit {
            max_rows: self.max_rows(width),
            stop: self.cancellation.as_ref().map(CancellationToken::flag),
        }
    }

    /// The error of a join stopped by its row limit: the query was cancelled, or the join
    /// would outgrow the budget.
    fn too_large(&self, rows: usize, width: usize) -> NativeError {
        if let Err(cancelled) = self.check() {
            return cancelled;
        }
        let requested = rows.saturating_mul(width.max(1) * 8);
        QueryEvaluationError::MemoryLimit(BudgetExceeded {
            limit: self.budget.used().saturating_add(self.budget.remaining()),
            // A table may grow to half of what is left (`max_rows`).
            requested: requested.saturating_mul(2),
            used: self.budget.used(),
            shared: self.budget.bounded_by_shared(),
        })
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
        if let Some(id) = self.lookup_const(term.as_ref()) {
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
            return self.eval_cached(pattern, None);
        };
        let depth = self.depth.get();
        // The plan's estimate of what this operator gives (`estimate`), as `plan_query`
        // shows it before running.
        let estimated_rows = self.estimate_rows(pattern);
        let index = {
            let mut trace = trace.borrow_mut();
            let (operator, detail) = describe(pattern);
            trace.push(PlanStep {
                depth,
                operator: operator.to_owned(),
                detail,
                estimated_rows,
                rows: 0,
                micros: 0,
                cache: None,
            });
            trace.len() - 1
        };
        self.depth.set(depth + 1);
        let start = Instant::now();
        let result = self.eval_cached(pattern, Some(index));
        self.depth.set(depth);
        if let Ok(solutions) = &result {
            let step = &mut trace.borrow_mut()[index];
            step.rows = solutions.table.len() as u64;
            step.micros = start.elapsed().as_micros() as u64;
        }
        result
    }

    /// `pattern`'s solutions where any `limit` of them will do (a LIMIT without ORDER BY):
    /// at least `limit` rows if there are as many, else all. The limit goes down to where
    /// rows are made: a basic graph pattern stops joining once it has enough
    /// ([`Context::bgp`]), an OPTIONAL needs only `limit` rows of its left side (a left join
    /// keeps every left row), a projection passes it through. Anything else is evaluated
    /// whole.
    fn eval_limited(&self, pattern: &GraphPattern, limit: usize) -> NativeResult<Solutions> {
        match pattern {
            GraphPattern::Project { inner, variables } => {
                let solutions = self.eval_limited(inner, limit)?;
                Ok(self.project(solutions, variables))
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } if !self.as_written => {
                let left = self.eval_limited(left, limit)?;
                self.optional(left, right, expression.as_ref())
            }
            GraphPattern::Bgp { .. } => {
                self.limit.set(Some(limit));
                let solutions = self.eval(pattern);
                self.limit.set(None);
                solutions
            }
            // Every conjunct must be applied inside the pattern, where the joins stop: one
            // applied after it could drop rows below the limit.
            GraphPattern::Filter { expr, inner }
                if !self.as_written
                    && let GraphPattern::Bgp { patterns } = &**inner
                    && placed_in(expr, patterns) =>
            {
                self.limit.set(Some(limit));
                let solutions = self.eval(pattern);
                self.limit.set(None);
                solutions
            }
            _ => self.eval(pattern),
        }
    }

    /// `BIND(expression AS variable)`: a column with the expression's value per row, unbound
    /// where it is an error.
    fn extend(
        &self,
        solutions: Solutions,
        variable: &Variable,
        expression: &Expression,
    ) -> NativeResult<Solutions> {
        // BIND(?x AS ?y) copies the column: the same terms, nothing to evaluate (the
        // aggregates of a SELECT come out this way).
        if let Expression::Variable(source) = expression {
            let values = match solutions.column(source) {
                Some(column) => solutions.table.column(column).to_vec(),
                None => vec![UNDEF; solutions.table.len()],
            };
            let mut solutions = solutions;
            let mut columns = std::mem::take(&mut solutions.table).into_columns();
            columns.push(values);
            solutions.vars.push(variable.clone());
            solutions.table = IdTable::from_columns(columns);
            return self.produced(solutions);
        }
        let (solutions, plain, added) = if contains_any_exists(expression) {
            self.with_exists(solutions, expression)?
        } else {
            (solutions, expression.clone(), 0)
        };
        let values: Vec<u64> = (0..solutions.table.len())
            .map(|row| {
                self.evaluator
                    .in_solution(row as u64, || {
                        self.evaluator.eval(&plain, &self.binding(&solutions, row))
                    })
                    .map_or(UNDEF, |term| self.id(&term))
            })
            .collect();
        let mut solutions = drop_last_columns(solutions, added);
        let mut columns = std::mem::take(&mut solutions.table).into_columns();
        columns.push(values);
        solutions.vars.push(variable.clone());
        solutions.table = IdTable::from_columns(columns);
        self.produced(solutions)
    }

    fn eval_operator(&self, pattern: &GraphPattern) -> NativeResult<Solutions> {
        self.check()?;
        if let Some(classes) = &self.late
            && late::joins(pattern)
            && late::safe(pattern)
            && self.late_scope()
        {
            return self.late_expansion(pattern, classes);
        }
        match pattern {
            GraphPattern::Bgp { patterns } => self.bgp(patterns, &[], &mut Vec::new()),
            GraphPattern::Graph { name, inner } => self.graph(name, inner),
            GraphPattern::Service {
                name,
                inner,
                silent,
            } => self.service(name, inner, *silent, None),
            // A SERVICE joined to a pattern gets the values the pattern binds.
            GraphPattern::Join { left, right }
                if matches!(**right, GraphPattern::Service { .. })
                    || matches!(**left, GraphPattern::Service { .. }) =>
            {
                let (local, remote) = match &**right {
                    GraphPattern::Service { .. } => (left, right),
                    _ => (right, left),
                };
                let GraphPattern::Service {
                    name,
                    inner,
                    silent,
                } = &**remote
                else {
                    unreachable!("matched above")
                };
                if vectors::is_search(name) {
                    return self.vector_join(local, inner);
                }
                let bound = self.eval(local)?;
                let found = self.service(name, inner, *silent, Some(&bound))?;
                self.join(bound, found)
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } if matches!(**right, GraphPattern::Service { .. }) => {
                let GraphPattern::Service {
                    name,
                    inner,
                    silent,
                } = &**right
                else {
                    unreachable!("matched above")
                };
                let left = self.eval(left)?;
                let found = self.service(name, inner, *silent, Some(&left))?;
                self.left_join(left, found, expression.as_ref())
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => self.path(subject, path, object),
            GraphPattern::Join { left, right } => {
                // Paths joined to triple patterns: ordered with them (`path_joins`).
                if !self.as_written
                    && let Some(join) = path_joins::PathJoin::of(pattern)
                {
                    return self.join_with_paths(&join);
                }
                // A path joined to another pattern is evaluated second, from the values
                // the pattern binds to one of its ends.
                match (as_path(left), as_path(right)) {
                    (_, Some(path)) => {
                        let bound = self.eval(left)?;
                        let reached = self.path_from(&bound, &path)?;
                        self.join(bound, reached)
                    }
                    (Some(path), None) => {
                        let bound = self.eval(right)?;
                        let reached = self.path_from(&bound, &path)?;
                        self.join(reached, bound)
                    }
                    (None, None) if self.as_written => {
                        let (left, right) = (self.eval(left)?, self.eval(right)?);
                        self.join(left, right)
                    }
                    // The smaller side first; the other from its rows (`sideways`).
                    (None, None) => self.join_sideways(left, right),
                }
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                let left = self.eval(left)?;
                if !self.as_written {
                    // Only rows that agree with a left row on the shared variables can
                    // match: the right side is evaluated from the left side's values.
                    return self.optional(left, right, expression.as_ref());
                }
                let right = match as_path(right) {
                    Some(path) => self.path_from(&left, &path)?,
                    None => self.eval(right)?,
                };
                self.left_join(left, right, expression.as_ref())
            }
            GraphPattern::Filter { expr, inner } => match &**inner {
                GraphPattern::Bgp { patterns } => {
                    // Range conjuncts on a variable narrow the scan that binds it (the
                    // conjunct still runs on every row, so the ranges only prune). Each
                    // conjunct runs as soon as the join sequence has bound its variables,
                    // so that a selective filter keeps the following joins small.
                    let hints = ranges::hints(expr, &self.snapshot);
                    let mut all = Vec::new();
                    pushdown::conjuncts_of(expr, &mut all);
                    let as_written = self.as_written;
                    let (early, late): (Vec<_>, Vec<_>) = all
                        .into_iter()
                        .partition(|c| !as_written && pushdown::movable(c));
                    let spatial_seed = self.spatial_seed(&early, patterns);
                    let mut early: Vec<(&Expression, Vec<Variable>)> = early
                        .into_iter()
                        .map(|c| (c, expression_variables(c)))
                        .collect();
                    // A spatial filter's candidates from the R-tree start the joins.
                    let mut solutions = match spatial_seed {
                        Some(seed) => {
                            let seed = self.produced(seed)?;
                            self.bgp_from(seed, patterns, &mut early)?
                        }
                        None => self.bgp(patterns, &hints, &mut early)?,
                    };
                    // What the pattern couldn't place: variables it doesn't bind (the
                    // conjunct is then an error or a BOUND test), EXISTS, draws per row.
                    for conjunct in early.into_iter().map(|(c, _)| c).chain(late) {
                        solutions = self.filter(solutions, conjunct)?;
                    }
                    Ok(solutions)
                }
                // HAVING: the group may apply parts of it before it reads everything
                // (`sets`); all of it is applied here.
                GraphPattern::Group {
                    inner,
                    variables,
                    aggregates,
                } => {
                    let solutions = self.group(inner, variables, aggregates, Some(expr))?;
                    self.filter(solutions, expr)
                }
                other => {
                    let solutions = self.eval(other)?;
                    self.filter(solutions, expr)
                }
            },
            GraphPattern::Union { left, right } => {
                let (left, right) = (self.eval(left)?, self.eval(right)?);
                self.union(left, right)
            }
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                let solutions = self.eval(inner)?;
                self.extend(solutions, variable, expression)
            }
            GraphPattern::Minus { left, right } => {
                let (left, right) = (self.eval(left)?, self.eval(right)?);
                let (lk, rk) = shared_columns(&left, &right);
                if lk.is_empty() {
                    return Ok(left);
                }
                let mut table = left.table;
                if has_undef(&table, &lk) || has_undef(&right.table, &rk) {
                    // A row goes if a compatible row shares a variable both bind.
                    let removed = compatible_mask(&table, &right.table, &lk, &rk, true);
                    table.retain_mask(&removed.iter().map(|r| !r).collect::<Vec<_>>());
                } else {
                    anti_join_in_place(&mut table, &right.table, &lk, &rk);
                }
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
                            Some(term) => self.id(&Term::from(term.clone())),
                            None => UNDEF,
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
                    // A BGP as a set over the projection (`sets`): what only feeds
                    // duplicates isn't read.
                    let solutions = match &**sorted {
                        GraphPattern::Bgp { patterns }
                            if patterns.len() > 1 && !self.as_written =>
                        {
                            self.eval_set(sorted, variables)?
                        }
                        _ => self.eval(sorted)?,
                    };
                    let mut solutions = self.project(solutions, variables);
                    solutions.table.dedup_preserving_order();
                    return self.order_by(solutions, expression, None);
                }
                // DISTINCT over a projection of joins: the joins work on sets (`sets`).
                if let GraphPattern::Project {
                    inner: projected,
                    variables,
                } = &**inner
                    && (sets::joins(projected)
                        || matches!(&**projected, GraphPattern::Bgp { patterns } if patterns.len() > 1))
                    && !self.as_written
                {
                    let solutions = self.eval_set(projected, variables)?;
                    let mut solutions = self.project(solutions, variables);
                    solutions.table.dedup_preserving_order();
                    return Ok(solutions);
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
                let mut solutions = match length {
                    Some(length) => self.eval_limited(inner, start + length)?,
                    None => self.eval(inner)?,
                };
                solutions.table.slice(*start, *length);
                Ok(solutions)
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => self.group(inner, variables, aggregates, None),
            GraphPattern::Lateral { left, right } => {
                let left = self.eval(left)?;
                self.lateral(left, right)
            }
        }
    }

    // --- GRAPH ---------------------------------------------------------------------------

    /// Evaluates `pattern` with `scope` as the active graph.
    fn in_graph(&self, scope: GraphScope, pattern: &GraphPattern) -> NativeResult<Solutions> {
        let outer = self.graph.replace(scope);
        let result = self.eval(pattern);
        *self.graph.borrow_mut() = outer;
        result
    }

    /// Whether the dataset has the named graph `id`.
    fn is_named_graph(&self, id: TermId) -> bool {
        match &self.named {
            Some(named) => named.contains(&id),
            None => self.snapshot.contains_named_graph(id),
        }
    }

    fn graph(&self, name: &NamedNodePattern, inner: &GraphPattern) -> NativeResult<Solutions> {
        let variable = match name {
            // Jena's name for the default graph (`compat`): the store's default graph.
            NamedNodePattern::NamedNode(n) if crate::compat::names_default_graph(n.as_str()) => {
                return self.in_graph(GraphScope::Default, inner);
            }
            NamedNodePattern::NamedNode(n) => {
                let id = self
                    .lookup_const(n.as_ref().into())
                    .filter(|&id| self.is_named_graph(id));
                return match id {
                    Some(id) => self.in_graph(GraphScope::Named(id), inner),
                    // No such graph in the dataset: no solutions, whatever the pattern
                    // would give without reading statements (VALUES, BIND).
                    None => {
                        let mut vars = Vec::new();
                        bound_variables(inner, &mut vars);
                        let table = IdTable::new(vars.len());
                        Ok(Solutions {
                            vars,
                            table,
                            ordered: false,
                        })
                    }
                };
            }
            NamedNodePattern::Variable(v) => v,
        };
        if scans_everywhere(inner, variable) {
            // Every row comes from scans, which bind the variable: one evaluation for
            // all graphs.
            let mut solutions = self.in_graph(GraphScope::Variable(variable.clone()), inner)?;
            if let (Some(named), Some(column)) = (&self.named, solutions.column(variable)) {
                let mask: Vec<bool> = solutions
                    .table
                    .column(column)
                    .iter()
                    .map(|&id| named.contains(&TermId::from_raw(id)))
                    .collect();
                solutions.table.retain_mask(&mask);
            }
            return Ok(solutions);
        }
        // Graph by graph: the pattern in each named graph, with the variable bound to it.
        let graphs: Vec<TermId> = match &self.named {
            Some(named) => named.clone(),
            None => self.snapshot.named_graphs().collect(),
        };
        let mut all: Option<Solutions> = None;
        for graph in graphs {
            self.check()?;
            let mut solutions = self.in_graph(GraphScope::Named(graph), inner)?;
            match solutions.column(variable) {
                // The pattern binds the variable itself: it must be this graph.
                Some(column) => {
                    let mask: Vec<bool> = solutions
                        .table
                        .column(column)
                        .iter()
                        .map(|&id| id == graph.raw() || id == UNDEF)
                        .collect();
                    solutions.table.retain_mask(&mask);
                    let mut columns = std::mem::take(&mut solutions.table).into_columns();
                    columns[column].fill(graph.raw());
                    solutions.table = IdTable::from_columns(columns);
                }
                None => {
                    let rows = solutions.table.len();
                    let mut columns = std::mem::take(&mut solutions.table).into_columns();
                    columns.push(vec![graph.raw(); rows]);
                    solutions.vars.push(variable.clone());
                    solutions.table = IdTable::from_columns(columns);
                }
            }
            all = Some(match all {
                Some(all) => self.union(all, solutions)?,
                None => solutions,
            });
        }
        match all {
            Some(all) => Ok(all),
            None => {
                let mut vars = Vec::new();
                bound_variables(inner, &mut vars);
                if !vars.contains(variable) {
                    vars.push(variable.clone());
                }
                let table = IdTable::new(vars.len());
                Ok(Solutions {
                    vars,
                    table,
                    ordered: false,
                })
            }
        }
    }

    // --- property paths ------------------------------------------------------------------

    /// An end of a path: a variable (blank nodes are variables here), or a constant's id
    /// (computed if the store doesn't know it; such an id has no edges).
    fn path_end(&self, term: &TermPattern) -> Result<Variable, u64> {
        match term {
            TermPattern::Variable(v) => Ok(v.clone()),
            TermPattern::BlankNode(b) => {
                Ok(Variable::new_unchecked(format!("_bnode_{}", b.as_str())))
            }
            TermPattern::NamedNode(n) => Err(self.id(&n.clone().into())),
            TermPattern::Literal(l) => Err(self.id(&l.clone().into())),
            // A triple term at a path's end matches nothing a path reaches.
            TermPattern::Triple(_) => Err(UNDEF),
        }
    }

    /// The path evaluator for the active graph. There is none while `GRAPH ?g` is evaluated
    /// for all graphs at once; [`scans_everywhere`] sends patterns with paths graph by graph.
    fn path_evaluator(&self) -> NativeResult<paths::PathEvaluator<'_>> {
        let graph = match &*self.graph.borrow() {
            GraphScope::Default => paths::PathGraph::Default,
            GraphScope::Union => paths::PathGraph::Merged(self.merge_set.as_deref()),
            GraphScope::Named(id) => paths::PathGraph::Named(*id),
            // A graph the store doesn't hold: the merge of no graphs.
            GraphScope::Missing => paths::PathGraph::Merged(Some(&[])),
            // `GRAPH ?g` sends patterns with paths graph by graph (`scans_everywhere`).
            GraphScope::Variable(_) => return unsupported("a path read in all graphs at once"),
        };
        Ok(paths::PathEvaluator {
            snapshot: &self.snapshot,
            model: self.model,
            graph,
            fixed: None,
            cancellation: self.cancellation.as_ref(),
            closures: Cell::new(0),
        })
    }

    /// The solutions of `path` that can join `bound`: if `bound` binds one of the path's
    /// variable ends in every row, the path is followed from those values (from the end
    /// with fewer distinct values, if both are bound). Otherwise all of the path's
    /// solutions.
    fn path_from(&self, bound: &Solutions, path: &PathPattern<'_>) -> NativeResult<Solutions> {
        let start = Instant::now();
        // The distinct values of a variable, if every row of `bound` has one.
        let values = |variable: &Variable| -> Option<Vec<u64>> {
            let mut values = bound.table.column(bound.column(variable)?).to_vec();
            values.sort_unstable();
            values.dedup();
            (values.last() != Some(&UNDEF)).then_some(values)
        };
        if self.as_written {
            return self.filtered_path(path, None, start);
        }
        let (Ok(s), Ok(o)) = (self.path_end(path.subject), self.path_end(path.object)) else {
            // A constant end already bounds the path.
            return self.filtered_path(path, None, start);
        };
        let resolved = paths::Path::resolve(path.path, &self.snapshot);
        let evaluator = self.path_evaluator()?;
        // Bound at both ends: followed from the end with fewer values (the join with
        // `bound` keeps the rows whose other end matches).
        let (starts, ends) = match (values(&s), values(&o)) {
            (Some(starts), Some(ends)) if ends.len() < starts.len() => (None, Some(ends)),
            (starts, ends) => (starts, ends),
        };
        let (pairs, from) = if let Some(starts) = starts {
            (evaluator.reached_from(&resolved, &starts), starts.len())
        } else if let Some(ends) = ends {
            (evaluator.reaching(&resolved, &ends), ends.len())
        } else {
            return self.filtered_path(path, None, start);
        };
        let estimate = || self.path_estimate(path, Some(from));
        self.note_closures(&evaluator, path.path, estimate, pairs.len(), start);
        let (vars, table) = if s == o {
            let same = pairs.into_iter().filter(|(a, b)| a == b).map(|(a, _)| a);
            (vec![s], IdTable::from_columns(vec![same.collect()]))
        } else {
            let (starts, ends): (Vec<u64>, Vec<u64>) = pairs.into_iter().unzip();
            (vec![s, o], IdTable::from_columns(vec![starts, ends]))
        };
        let reached = self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })?;
        self.filtered_path(path, Some((reached, from)), start)
    }

    /// The path's solutions (`reached`, from that many bound values, or all of them),
    /// after the path's own filter.
    fn filtered_path(
        &self,
        path: &PathPattern<'_>,
        reached: Option<(Solutions, usize)>,
        start: Instant,
    ) -> NativeResult<Solutions> {
        let (subject, object) = (path.subject, path.object);
        let mut detail = format!("{subject} {} {object}", path.path);
        let bound_values = reached.as_ref().map(|(_, values)| *values);
        let mut solutions = match reached {
            Some((solutions, values)) => {
                detail.push_str(&format!(" from {values} bound values"));
                solutions
            }
            None => self.path(subject, path.path, object)?,
        };
        if self.trace.is_some() {
            let estimate = self.path_estimate(path, bound_values);
            self.note("path", detail, estimate, solutions.table.len(), start);
        }
        if let Some(filter) = path.filter {
            let start = Instant::now();
            let estimate = filtered(solutions.table.len(), filter);
            solutions = self.filter(solutions, filter)?;
            if self.trace.is_some() {
                let rows = solutions.table.len();
                self.note("filter", filter.to_string(), estimate, rows, start);
            }
        }
        Ok(solutions)
    }

    /// Notes a closure computed per strongly connected component by `evaluator` (EXPLAIN).
    fn note_closures(
        &self,
        evaluator: &paths::PathEvaluator<'_>,
        path: &nrese_sparql_syntax::algebra::PropertyPathExpression,
        estimate: impl FnOnce() -> Option<u64>,
        rows: usize,
        start: Instant,
    ) {
        if self.trace.is_some() && evaluator.closures.get() > 0 {
            let detail = format!("{path} by strongly connected components");
            self.note("closure", detail, estimate(), rows, start);
        }
    }

    fn path(
        &self,
        subject: &TermPattern,
        path: &nrese_sparql_syntax::algebra::PropertyPathExpression,
        object: &TermPattern,
    ) -> NativeResult<Solutions> {
        let start = Instant::now();
        let resolved = paths::Path::resolve(path, &self.snapshot);
        let end = |term: &TermPattern| self.path_end(term);
        let mut evaluator = self.path_evaluator()?;
        evaluator.fixed = end(subject).err().or_else(|| end(object).err());
        let column = |values: Vec<u64>| IdTable::from_columns(vec![values]);
        let (vars, table) = match (end(subject), end(object)) {
            (Err(start), Err(finish)) => {
                let rows = evaluator.multiplicity(&resolved, start, finish);
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
        let estimate = || {
            let pattern = PathPattern {
                subject,
                path,
                object,
                filter: None,
            };
            Some(self.path_size(&pattern).round() as u64)
        };
        self.note_closures(&evaluator, path, estimate, table.len(), start);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }

    // --- basic graph patterns ------------------------------------------------------------

    /// Joins the patterns in the planned order. `filters` are conjuncts with their
    /// variables: each is applied, and removed from the list, after the first step that
    /// binds all its variables.
    fn bgp(
        &self,
        triples: &[TriplePattern],
        hints: &[ranges::Hint],
        filters: &mut Vec<(&Expression, Vec<Variable>)>,
    ) -> NativeResult<Solutions> {
        let limit = self.limit.take();
        // GeoSPARQL relations between features (`spatial`): those with a constant side
        // start the joins, the others follow them.
        if let Some((first, later, rest)) = self.spatial_split(triples) {
            let mut result = Solutions::unit();
            for triple in &first {
                result = self.spatial_join(result, triple)?;
            }
            result = self.filter_bound(result, filters)?;
            if !rest.is_empty() {
                result = if first.is_empty() {
                    self.bgp(&rest, hints, filters)?
                } else {
                    self.bgp_from(result, &rest, filters)?
                };
            }
            for triple in &later {
                result = self.spatial_join(result, triple)?;
            }
            return self.filter_bound(result, filters);
        }
        // Full-text searches start the joins (`search`).
        if let Some((searches, rest)) = search::split(triples) {
            let mut result = Solutions::unit();
            for search in &searches {
                let start = Instant::now();
                let found = self.search(search)?;
                if self.trace.is_some() {
                    let rows = found.table.len();
                    self.note("text search", format!("{search:?}"), None, rows, start);
                }
                result = self.join(result, found)?;
            }
            result = self.filter_bound(result, filters)?;
            return self.bgp_from(result, &rest, filters);
        }
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
        // A string test on a large pattern's object: the terms that pass, from the
        // dictionary first (`strings`), as a hint.
        let string_hints: Vec<ranges::Hint> = if self.as_written {
            Vec::new()
        } else {
            // The smallest pattern, with its range hint if it has one: a pattern many times
            // larger is joined by probing from the rows before it, never scanned, and a
            // dictionary pass for it would be wasted.
            let smallest = scans
                .iter()
                .map(|s| {
                    let hinted = match &s.slots[2] {
                        Slot::Var(object) if s.in_default_graph() && !s.repeats_variable() => hints
                            .iter()
                            .find(|h| &h.variable == object)
                            .and_then(|hint| {
                                let permutation = s.permutation_for(Some(object));
                                (s.first_free(permutation) == Some(2))
                                    .then(|| self.count_ranges(s, permutation, &hint.ranges))
                            }),
                        _ => None,
                    };
                    hinted
                        .unwrap_or_else(|| self.snapshot.estimate_in(self.model, &s.quad_pattern()))
                })
                .min()
                .unwrap_or(0);
            let mut found: Vec<ranges::Hint> = Vec::new();
            for s in &scans {
                let Slot::Var(object) = &s.slots[2] else {
                    continue;
                };
                if hints.iter().chain(&found).any(|h| &h.variable == object) {
                    continue;
                }
                let Some(condition) = strings::combined(
                    filters
                        .iter()
                        .filter(|(_, read)| read.as_slice() == std::slice::from_ref(object))
                        .map(|(conjunct, _)| *conjunct),
                    object,
                ) else {
                    continue;
                };
                let rows = self.snapshot.estimate_in(self.model, &s.quad_pattern());
                // A prefix found by binary search in the text order costs nothing worth
                // weighing; otherwise the dictionary pass must be worth its size.
                let cheap = condition.is_prefix() && self.snapshot.text_order_ready();
                if (!cheap
                    && rows < self.snapshot.dictionary_bytes() / strings::DICTIONARY_BYTES_PER_ROW)
                    || rows > smallest.saturating_mul(PROBE_FACTOR)
                {
                    continue;
                }
                let start = Instant::now();
                if let Some(ranges) = strings::ranges(&self.snapshot, &condition, rows) {
                    if self.trace.is_some() {
                        let detail = format!("terms {object} can take");
                        // The object's distinct values the conjuncts on it keep (each
                        // passing term is a range of one id or more).
                        let selectivity: f64 = filters
                            .iter()
                            .filter(|(_, read)| read.as_slice() == std::slice::from_ref(object))
                            .map(|(conjunct, _)| pushdown::selectivity(conjunct))
                            .product();
                        let distinct = self.distinct(s, object, rows) as f64;
                        let estimate = Some((distinct * selectivity).round() as u64);
                        let found = ranges.len();
                        self.note("dictionary string test", detail, estimate, found, start);
                    }
                    found.push(ranges::Hint {
                        variable: condition.variable.clone(),
                        ranges,
                    });
                }
            }
            found
        };
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
                let hint = hints
                    .iter()
                    .chain(&string_hints)
                    .find(|h| &h.variable == object)?;
                let permutation = s.permutation_for(Some(object));
                (s.first_free(permutation) == Some(2) && !s.repeats_variable())
                    .then_some((permutation, hint.ranges.as_slice()))
            })
            .collect();
        let counts: Vec<u64> = scans
            .iter()
            .zip(&ranged)
            .map(|(s, ranged)| match ranged {
                // One pattern: nothing to order, and the ranges are read once anyway.
                Some(_) if scans.len() == 1 => {
                    self.snapshot.estimate_in(self.model, &s.quad_pattern())
                }
                Some((permutation, ranges)) => self.count_ranges(s, *permutation, ranges),
                None => self.snapshot.estimate_in(self.model, &s.quad_pattern()),
            })
            .collect();
        let cyclic = ranged.iter().all(Option::is_none);
        // EXPLAIN's estimate of a cyclic BGP samples its join: before the join is timed.
        let estimate = match self.trace {
            Some(_) if cyclic => self
                .cyclic_estimate(&scans, &counts)
                .map(|rows| rows.round() as u64),
            _ => None,
        };
        let start = Instant::now();
        let start_joins = start;
        if cyclic && let Some(solutions) = self.cyclic_bgp(&scans, &counts, estimate)? {
            if self.trace.is_some() {
                let detail = triples.iter().map(ToString::to_string).collect::<Vec<_>>();
                let rows = solutions.table.len();
                self.note("wcoj", detail.join(" . "), estimate, rows, start);
            }
            return Ok(solutions);
        }
        // The order plans with what the filters leave of each pattern (W3b): a pattern
        // whose variables a selective filter reads counts as smaller. Ranged patterns
        // are already counted within their ranges; probing decisions keep exact counts.
        let planned: Vec<u64> = scans
            .iter()
            .zip(&counts)
            .zip(&ranged)
            .map(|((scan, &count), ranged)| {
                if ranged.is_some() || self.as_written {
                    return count;
                }
                let vars = scan.vars();
                let factor: f64 = filters
                    .iter()
                    .filter(|(_, read)| !read.is_empty() && read.iter().all(|v| vars.contains(v)))
                    .map(|(conjunct, _)| pushdown::selectivity(conjunct))
                    .product();
                if count == 0 {
                    0
                } else {
                    ((count as f64 * factor).ceil() as u64).max(1)
                }
            })
            .collect();
        let plan = self.join_order(&scans, &planned, self.trace.is_some());
        let estimate = |step: usize| Some(plan.rows[step].max(0.0).round() as u64);
        let order = &plan.order;
        // Each prefix of the order is a part of the result cache ([`cached::Joins`]): the
        // longest one cached is where the joins start.
        let given = filters.clone();
        let joins = cached::Joins {
            triples,
            scans: &scans,
            ranged: &ranged,
            order,
            filters: &given,
        };
        let reused = self.cached_prefix(&joins, filters, limit.is_some())?;
        let first = order[0];
        let join_var = order.get(1).and_then(|&i| {
            scans[first]
                .vars()
                .into_iter()
                .find(|v| scans[i].vars().contains(v))
        });
        let (mut result, joined) = match reused {
            Some(prefix) => prefix,
            None => {
                let result = match ranged[first] {
                    Some((permutation, ranges)) => {
                        self.scan_ranges(&scans[first], permutation, ranges)?
                    }
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
                (self.filter_bound(result, filters)?, 1)
            }
        };
        // The whole pattern from the cache.
        if joined > 1 && joined == order.len() {
            return Ok(result);
        }
        // Joins from `from` patterns on; the prefixes they make are offered to the cache
        // (`offer`: not for a morsel, whose rows are some of the prefix's).
        let join_rest = |mut result: Solutions,
                         filters: &mut Vec<(&Expression, Vec<Variable>)>,
                         from: usize,
                         offer: bool|
         -> NativeResult<Solutions> {
            for (step, &next) in order.iter().enumerate().skip(from) {
                let start = Instant::now();
                let shared: Vec<Variable> = scans[next]
                    .vars()
                    .into_iter()
                    .filter(|v| result.column(v).is_some())
                    .collect();
                // (A default graph merged from listed graphs is read by scans, which filter.)
                let probe = !shared.is_empty()
                    && self.merge_set.is_none()
                    && (result.table.len() as u64).saturating_mul(PROBE_FACTOR) < counts[next];
                // No shared variable, but `?a = ?b` between them: a join, not a cross
                // product (`equijoin`).
                let linked = (shared.is_empty() && !self.as_written && ranged[next].is_none())
                    .then(|| equijoin::link(filters, &result, &scans[next].vars()))
                    .flatten();
                result = if let Some((a, b, same_term)) = &linked {
                    self.equality_join(result, &scans[next], counts[next], a, b, *same_term)?
                } else if probe {
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
                        _ if linked.is_some() => "equality join",
                        (true, _) => "index join",
                        (false, true) => "cross product",
                        (false, false) => "join",
                    };
                    let mut detail = triples[next].to_string();
                    if probe && linked.is_none() {
                        detail = self.probed_detail(detail);
                    }
                    self.note(operator, detail, estimate(step), result.table.len(), start);
                }
                result = self.filter_bound(result, filters)?;
                if offer {
                    self.offer_prefix(&joins, step + 1, &result, start_joins.elapsed());
                }
            }
            Ok(result)
        };
        // LIMIT (`eval_limited`): any `limit` rows will do. One pattern: its first rows.
        // Several: the first pattern's rows go through the joins in morsels until enough
        // come out. The first morsel is small; each next one is sized by the rows still
        // needed over the rows a first-pattern row has yielded so far (the fan-out), at
        // most eight times the last. Small morsels also make the later joins probe the
        // index instead of scanning whole patterns.
        let Some(limit) = limit.filter(|_| result.table.width() > 0) else {
            return join_rest(result, filters, joined, true);
        };
        // EXPLAIN: the rows the limit lets the pattern stop at, against what it would give.
        let limited = |detail: String, rows: usize, start: Instant| {
            if self.trace.is_some() {
                let estimate = estimate(order.len() - 1).map(|e| e.min(limit as u64));
                self.note("limit pushdown", detail, estimate, rows, start);
            }
        };
        if order.len() == 1 {
            let read = result.table.len();
            if read > limit {
                result.table.slice(0, Some(limit));
            }
            let detail = format!("{limit} wanted: the first of {read} rows of one pattern");
            limited(detail, result.table.len(), start);
            return Ok(result);
        }
        if result.table.len() <= MIN_MORSEL {
            return join_rest(result, filters, joined, false);
        }
        let sorted = result.table.sorted_by().to_vec();
        let mut parts: Vec<Solutions> = Vec::new();
        let (mut rows, mut from, mut size) = (0, 0, MIN_MORSEL);
        let mut applied = None;
        while from < result.table.len() && rows < limit {
            let to = (from + size).min(result.table.len());
            let columns = result
                .table
                .columns()
                .iter()
                .map(|column| column[from..to].to_vec())
                .collect();
            let morsel = Solutions {
                vars: result.vars.clone(),
                table: IdTable::from_columns(columns).assume_sorted_by(sorted.clone()),
                ordered: false,
            };
            // Each morsel applies the same conjuncts at the same steps.
            let mut left = filters.clone();
            let part = join_rest(morsel, &mut left, joined, false)?;
            applied = Some(left);
            rows += part.table.len();
            parts.push(part);
            from = to;
            // Rows still needed over the fan-out so far, with a quarter to spare.
            let wanted = match rows {
                0 => usize::MAX,
                rows => (limit.saturating_sub(rows) as f64 * from as f64 / rows as f64 * 1.25)
                    .ceil() as usize,
            };
            size = wanted.clamp(MIN_MORSEL, size.saturating_mul(8));
        }
        if let Some(applied) = applied {
            *filters = applied;
        }
        let detail = format!(
            "{limit} wanted: {} morsel(s), {from} of {} rows of the first pattern joined",
            parts.len(),
            result.table.len()
        );
        limited(detail, rows, start);
        let vars = parts[0].vars.clone();
        let tables = parts
            .into_iter()
            .map(|part| {
                if part.vars == vars {
                    part.table
                } else {
                    self.project(part, &vars).table
                }
            })
            .collect();
        Ok(Solutions {
            table: IdTable::concat(vars.len(), tables),
            vars,
            ordered: false,
        })
    }

    /// Joins `triples` to `result`, a pattern connected to what is bound first, each by
    /// probing the index where the running result is small.
    fn bgp_from(
        &self,
        mut result: Solutions,
        triples: &[TriplePattern],
        filters: &mut Vec<(&Expression, Vec<Variable>)>,
    ) -> NativeResult<Solutions> {
        // GeoSPARQL relations (`spatial`) join after the other patterns.
        if let Some((first, later, rest)) = self.spatial_split(triples) {
            let mut result = if rest.is_empty() {
                result
            } else {
                self.bgp_from(result, &rest, filters)?
            };
            for triple in first.iter().chain(&later) {
                result = self.spatial_join(result, triple)?;
            }
            let bound: Vec<Variable> = result
                .vars
                .iter()
                .enumerate()
                .filter(|&(c, _)| !result.table.column(c).contains(&UNDEF))
                .map(|(_, v)| v.clone())
                .collect();
            return self.filter_among(result, filters, &bound);
        }
        let mut scans = Vec::with_capacity(triples.len());
        for triple in triples {
            match self.scan_pattern(triple) {
                Some(scan) => scans.push(scan),
                None => {
                    let mut vars = result.vars.clone();
                    for triple in triples {
                        for v in triple_variables(triple) {
                            if !vars.contains(&v) {
                                vars.push(v);
                            }
                        }
                    }
                    let width = vars.len();
                    self.consumed(&result);
                    return Ok(Solutions {
                        vars,
                        table: IdTable::new(width),
                        ordered: false,
                    });
                }
            }
        }
        let counts: Vec<u64> = scans
            .iter()
            .map(|s| self.snapshot.estimate_in(self.model, &s.quad_pattern()))
            .collect();
        // A conjunct runs once its variables are bound: by a scan, or by the seed where no
        // row leaves them unbound (a column alone isn't enough: an OPTIONAL may have left
        // it empty).
        let mut bound: Vec<Variable> = result
            .vars
            .iter()
            .enumerate()
            .filter(|&(c, _)| !result.table.column(c).contains(&UNDEF))
            .map(|(_, v)| v.clone())
            .collect();
        // EXPLAIN's estimates, from the seed's rows on: each pattern multiplies the rows by
        // its count over the distinct values of the variables it shares (independence,
        // as the join orderer; the seed's rows taken as distinct).
        let begun = Instant::now();
        let seed_rows = result.table.len();
        let mut estimated = seed_rows as f64;
        let mut distinct: HashMap<Variable, f64> = result
            .vars
            .iter()
            .map(|v| (v.clone(), estimated.max(1.0)))
            .collect();
        let mut left: Vec<usize> = (0..scans.len()).collect();
        while !left.is_empty() {
            let connected = |i: &usize| scans[*i].vars().iter().any(|v| result.column(v).is_some());
            let at = left
                .iter()
                .enumerate()
                .min_by_key(|(_, i)| (!connected(i), counts[**i]))
                .map(|(at, _)| at)
                .expect("not empty");
            let next = left.remove(at);
            let start = Instant::now();
            let shared: Vec<Variable> = scans[next]
                .vars()
                .into_iter()
                .filter(|v| result.column(v).is_some())
                .collect();
            let probe = !shared.is_empty()
                && self.merge_set.is_none()
                && (result.table.len() as u64).saturating_mul(PROBE_FACTOR) < counts[next];
            if self.trace.is_some() {
                let count = counts[next] as f64;
                let mut divisor = 1.0f64;
                let mut check = true;
                for v in scans[next].vars() {
                    let d = self.distinct(&scans[next], &v, counts[next]) as f64;
                    let joined = match distinct.get(&v) {
                        Some(&seen) => {
                            divisor *= seen.max(d).max(1.0);
                            seen.min(d)
                        }
                        None => {
                            check = false;
                            d
                        }
                    };
                    distinct.insert(v, joined.max(1.0));
                }
                // The orderer's bounds (`plan`): a check keeps at most its input, and a
                // fraction of a row is one.
                let before = estimated;
                estimated = estimated * count / divisor;
                if check {
                    estimated = estimated.min(before);
                }
                if estimated > 0.0 {
                    estimated = estimated.max(1.0);
                }
                for d in distinct.values_mut() {
                    *d = d.min(estimated.max(1.0));
                }
            }
            result = if probe {
                self.probe_join(result, &scans[next], &shared)?
            } else {
                let scanned = self.scan(&scans[next], shared.first())?;
                self.join(result, scanned)?
            };
            if self.trace.is_some() {
                let operator = if probe { "index join" } else { "join" };
                let rows = result.table.len();
                let estimate = Some(estimated.round() as u64);
                let mut detail = triples[next].to_string();
                if probe {
                    detail = self.probed_detail(detail);
                }
                self.note(operator, detail, estimate, rows, start);
            }
            for v in scans[next].vars() {
                if !bound.contains(&v) {
                    bound.push(v);
                }
            }
            result = self.filter_among(result, filters, &bound)?;
        }
        if self.trace.is_some() {
            let detail = format!("{} pattern(s) from {seed_rows} rows", triples.len());
            let estimate = Some(estimated.round() as u64);
            self.note("sideways", detail, estimate, result.table.len(), begun);
        }
        Ok(result)
    }

    /// The matches of `scan` in the object `ranges` of `permutation`, for planning: exact
    /// for a few ranges. For many (the terms a dictionary string test passed), a seek per
    /// range costs more than the scan it plans (YAGO q10: 655 k ranges, 100 ms of seeks),
    /// so the count is estimated: the widest ranges, which can hold most of the matches,
    /// are counted exactly and an even sample of the others stands for them. The
    /// evaluation reads the ranges themselves, so an estimate only moves the plan.
    fn count_ranges(
        &self,
        scan: &ScanPattern,
        permutation: Permutation,
        ranges: &[(TermId, TermId)],
    ) -> u64 {
        let pattern = scan.quad_pattern();
        let count = |&(low, high): &(TermId, TermId)| {
            self.snapshot
                .count_range_in(self.model, &pattern, permutation, low, high)
                .unwrap_or(0)
        };
        if ranges.len() < MANY_RANGES {
            return ranges.iter().map(count).sum();
        }
        let width = |&(low, high): &(TermId, TermId)| high.raw() - low.raw();
        let mut by_width: Vec<&(TermId, TermId)> = ranges.iter().collect();
        let widest = MANY_RANGES / 2;
        by_width.select_nth_unstable_by_key(widest, |range| std::cmp::Reverse(width(range)));
        let (wide, rest) = by_width.split_at(widest);
        let exact: u64 = wide.iter().map(|range| count(range)).sum();
        let step = rest.len().div_ceil(MANY_RANGES - widest);
        let (sampled, sum) = rest
            .iter()
            .step_by(step)
            .fold((0u64, 0u64), |(n, sum), range| (n + 1, sum + count(range)));
        let estimate = exact + (sum as f64 * rest.len() as f64 / sampled as f64).round() as u64;
        estimate.min(self.snapshot.estimate_in(self.model, &pattern))
    }

    /// Applies, and removes from `filters`, the conjuncts whose variables `solutions` binds.
    fn filter_bound(
        &self,
        solutions: Solutions,
        filters: &mut Vec<(&Expression, Vec<Variable>)>,
    ) -> NativeResult<Solutions> {
        let bound = solutions.vars.clone();
        self.filter_among(solutions, filters, &bound)
    }

    /// Applies, and removes from `filters`, the conjuncts whose variables are all in `bound`.
    fn filter_among(
        &self,
        mut solutions: Solutions,
        filters: &mut Vec<(&Expression, Vec<Variable>)>,
        bound: &[Variable],
    ) -> NativeResult<Solutions> {
        let mut index = 0;
        while index < filters.len() {
            if !filters[index].1.iter().all(|v| bound.contains(v)) {
                index += 1;
                continue;
            }
            let (conjunct, _) = filters.remove(index);
            let start = Instant::now();
            let estimate = filtered(solutions.table.len(), conjunct);
            solutions = self.filter(solutions, conjunct)?;
            if self.trace.is_some() {
                let rows = solutions.table.len();
                self.note("filter", conjunct.to_string(), estimate, rows, start);
            }
        }
        Ok(solutions)
    }

    /// The order in which to join a BGP's patterns ([`plan`]; `counts` are exact). Two
    /// patterns start with the smaller one; larger BGPs are planned with distinct counts.
    /// The rows after each step are estimated where `estimated` asks (or the order
    /// depends on them): a two-pattern BGP run without EXPLAIN skips its model.
    fn join_order(&self, scans: &[ScanPattern], counts: &[u64], estimated: bool) -> plan::Plan {
        if scans.is_empty() {
            return plan::Plan {
                order: Vec::new(),
                rows: Vec::new(),
            };
        }
        let mut order: Vec<usize> = (0..scans.len()).collect();
        order.sort_by_key(|&i| counts[i]);
        if scans.len() <= 2 && !estimated {
            let rows = vec![f64::NAN; order.len()];
            return plan::Plan { order, rows };
        }
        let mut vars: Vec<Variable> = Vec::new();
        let inputs = self.plan_inputs(scans, counts, &mut vars);
        if scans.len() <= 2 {
            // The smaller pattern first; the join's rows by the orderer's model (a star's
            // pair by the characteristic sets, an edge and a star on its object by the
            // characteristic pairs, a check bounded by the rows it checks).
            let sets = inputs
                .iter()
                .any(|i| i.star.is_some())
                .then(|| self.snapshot.characteristic_sets_in(self.model))
                .flatten();
            return plan::along(&inputs, vars.len(), PROBE_FACTOR, sets.as_deref(), &order);
        }
        self.order_inputs(&inputs, vars.len())
    }

    /// The join orderer's order of `inputs` over `variables` variables, with the
    /// characteristic sets where an input is part of a star.
    fn order_inputs(&self, inputs: &[plan::Input], variables: usize) -> plan::Plan {
        let sets = inputs
            .iter()
            .any(|i| i.star.is_some())
            .then(|| self.snapshot.characteristic_sets_in(self.model))
            .flatten();
        plan::order(inputs, variables, PROBE_FACTOR, sets.as_deref())
    }

    /// The join orderer's inputs for `scans` with their `counts`: distinct values per
    /// variable (numbered by their place in `vars`, which gets the new ones), stars and
    /// edges for the characteristic sets and pairs.
    fn plan_inputs(
        &self,
        scans: &[ScanPattern],
        counts: &[u64],
        vars: &mut Vec<Variable>,
    ) -> Vec<plan::Input> {
        let mut index_of = |v: Variable| variable_index(vars, v);
        let overlaps = self.overlaps(scans, counts);
        scans
            .iter()
            .zip(counts)
            .zip(overlaps)
            .map(|((scan, &count), overlaps)| {
                let star = match &scan.slots[0] {
                    Slot::Var(v) => self.star(scan, count, index_of(v.clone())),
                    _ => None,
                };
                let edge_object = match (&star, &scan.slots[2]) {
                    (Some(_), Slot::Var(o)) if scan.slots[0] != scan.slots[2] => {
                        Some(index_of(o.clone()))
                    }
                    _ => None,
                };
                plan::Input {
                    count,
                    edge_object,
                    vars: scan
                        .vars()
                        .into_iter()
                        .map(|v| {
                            let d = self.distinct(scan, &v, count).min(count);
                            (index_of(v), d)
                        })
                        .collect(),
                    star,
                    overlaps,
                }
            })
            .collect()
    }

    /// Per pattern of `scans` with one variable, the values it has in common with each
    /// other such pattern on the same variable ([`plan::Input::overlaps`]), where their
    /// `counts` together are at most [`plan::OVERLAP_VALUES`]: both lists are read sorted
    /// from the index and merged.
    fn overlaps(&self, scans: &[ScanPattern], counts: &[u64]) -> Vec<Vec<(usize, u64)>> {
        let mut out = vec![Vec::new(); scans.len()];
        // The single-variable patterns: their variable and its position.
        let single: Vec<Option<(Variable, usize)>> = scans
            .iter()
            .map(|scan| {
                let [v] = &scan.vars()[..] else {
                    return None;
                };
                if !scan.in_default_graph() || scan.repeats_variable() {
                    return None;
                }
                let position = (0..3).find(|&c| scan.slots[c].is_var(v))?;
                Some((v.clone(), position))
            })
            .collect();
        let mut lists: Vec<Option<Vec<u64>>> = vec![None; scans.len()];
        for i in 0..scans.len() {
            for j in i + 1..scans.len() {
                let (Some((a, _)), Some((b, _))) = (&single[i], &single[j]) else {
                    continue;
                };
                if a != b || counts[i].saturating_add(counts[j]) > plan::OVERLAP_VALUES {
                    continue;
                }
                for k in [i, j] {
                    if lists[k].is_none() {
                        let (v, position) = single[k].as_ref().expect("single");
                        let permutation = scans[k].permutation_for(Some(v));
                        let pattern = scans[k].quad_pattern();
                        // A column at a time where the index allows (a few ns a value;
                        // a quad at a time took 50: LUBM-10 q11 0.06 → 0.18 ms).
                        lists[k] = self
                            .snapshot
                            .scan_columns_in(self.model, &pattern, permutation, &[*position])
                            .and_then(|mut columns| columns.pop())
                            .or_else(|| {
                                let quads = self.snapshot.scan_sorted_in(
                                    self.model,
                                    &pattern,
                                    permutation,
                                )?;
                                Some(quads.map(|q| q.components()[*position]).collect())
                            });
                    }
                }
                let (Some(x), Some(y)) = (&lists[i], &lists[j]) else {
                    continue;
                };
                let both = sorted_overlap(x, y);
                out[i].push((j, both));
                out[j].push((i, both));
            }
        }
        out
    }

    /// `scan` (with `count` matches) as part of a star on its subject (the variable with
    /// index `subject`): a variable subject and a constant predicate in the default graph.
    /// A constant object keeps the share of the predicate's statements it matches.
    fn star(&self, scan: &ScanPattern, count: u64, subject: usize) -> Option<plan::Star> {
        let (Slot::Var(_), Slot::Const(predicate)) = (&scan.slots[0], &scan.slots[1]) else {
            return None;
        };
        if !scan.in_default_graph() {
            return None;
        }
        let selectivity = match &scan.slots[2] {
            Slot::Var(_) => 1.0,
            _ => {
                let mut free = scan.clone();
                free.slots[2] = Slot::Var(Variable::new_unchecked("_star_object"));
                let all = self.snapshot.estimate_in(self.model, &free.quad_pattern());
                if all == 0 {
                    0.0
                } else {
                    count as f64 / all as f64
                }
            }
        };
        Some(plan::Star {
            subject,
            predicate: predicate.raw(),
            selectivity,
        })
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

    /// `scans` as a worst-case-optimal join's patterns over their variables, if they are a
    /// cyclic BGP the executor joins so: at least three patterns, all in the default graph.
    fn cyclic_patterns(
        &self,
        scans: &[ScanPattern],
    ) -> Option<(Vec<Variable>, Vec<[wcoj::Pos; 3]>)> {
        if scans.len() < 3 || !scans.iter().all(ScanPattern::in_default_graph) {
            return None;
        }
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
                // Default-graph patterns only: the graph is constant.
                [0, 1, 2].map(|c| match &scan.slots[c] {
                    Slot::Const(id) => wcoj::Pos::Const(id.raw()),
                    Slot::Var(v) => {
                        wcoj::Pos::Var(vars.iter().position(|x| x == v).expect("collected above"))
                    }
                    Slot::Merged => unreachable!("only the graph position is merged"),
                })
            })
            .collect();
        wcoj::cyclic(&patterns, vars.len()).then_some((vars, patterns))
    }

    /// The rows of a cyclic BGP ([`Self::cyclic_patterns`]) by sampling its
    /// worst-case-optimal join ([`wcoj::Query::estimate`]): independence can't see a
    /// cycle closed by a correlated pattern (LUBM-10 q9, students taking a course of
    /// their advisor: 14 estimated for 2,540). `None` if the BGP isn't one.
    fn cyclic_estimate(&self, scans: &[ScanPattern], counts: &[u64]) -> Option<f64> {
        let (vars, patterns) = self.cyclic_patterns(scans)?;
        if counts.contains(&0) {
            return Some(0.0);
        }
        let patterns: Vec<[wcoj::Pos; 3]> = patterns
            .into_iter()
            .filter(|p| p.iter().any(|x| matches!(x, wcoj::Pos::Var(_))))
            .collect();
        let width = vars.len();
        let rows = wcoj::SAMPLE_ROWS;
        let query = wcoj::Query::new(&self.snapshot, self.model, patterns, width, rows);
        Some(query.estimate())
    }

    /// A cyclic BGP by a worst-case-optimal join ([`wcoj`]); `None` if it isn't cyclic.
    /// `estimate` is EXPLAIN's.
    fn cyclic_bgp(
        &self,
        scans: &[ScanPattern],
        counts: &[u64],
        estimate: Option<u64>,
    ) -> NativeResult<Option<Solutions>> {
        let Some((vars, patterns)) = self.cyclic_patterns(scans) else {
            return Ok(None);
        };
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
        let query = wcoj::Query::new(
            &self.snapshot,
            self.model,
            patterns,
            width,
            self.max_rows(width),
        );
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
        self.wcoj_step(&vars, &stats, estimate, table.len() as u64);
        Ok(Some(self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })?))
    }

    /// `COUNT(*)` of a BGP that is cyclic ([`Self::cyclic_patterns`]), by its
    /// worst-case-optimal join counting instead of producing its solutions
    /// ([`wcoj::Query::count_solutions`]); `None` if it isn't one.
    fn cyclic_count(&self, triples: &[TriplePattern]) -> NativeResult<Option<u64>> {
        if triples
            .iter()
            .any(|t| search::is_search(t, triples) || self.is_spatial(t))
        {
            return Ok(None);
        }
        let Some(scans) = triples
            .iter()
            .map(|t| self.scan_pattern(t))
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(None);
        };
        let Some((vars, patterns)) = self.cyclic_patterns(&scans) else {
            return Ok(None);
        };
        let start = Instant::now();
        let counts: Vec<u64> = scans
            .iter()
            .map(|s| self.snapshot.estimate_in(self.model, &s.quad_pattern()))
            .collect();
        let estimate = self
            .trace
            .as_ref()
            .and_then(|_| self.cyclic_estimate(&scans, &counts))
            .map(|rows| rows.round() as u64);
        let mut stats = wcoj::Stats::default();
        let count = if counts.contains(&0) {
            0
        } else {
            let patterns: Vec<[wcoj::Pos; 3]> = patterns
                .into_iter()
                .filter(|p| p.iter().any(|x| matches!(x, wcoj::Pos::Var(_))))
                .collect();
            let query =
                wcoj::Query::new(&self.snapshot, self.model, patterns, vars.len(), usize::MAX);
            let token = self.cancellation.clone();
            let cancelled = move || token.as_ref().is_some_and(CancellationToken::is_cancelled);
            match query.count_solutions(&cancelled, &mut stats) {
                Ok(count) => count,
                Err(wcoj::Stop::Cancelled) => return Err(QueryEvaluationError::Cancelled.into()),
                Err(wcoj::Stop::TooManyRows) => unreachable!("a count keeps no rows"),
            }
        };
        if self.trace.is_some() {
            self.wcoj_step(&vars, &stats, estimate, count);
            let detail = triples.iter().map(ToString::to_string).collect::<Vec<_>>();
            self.note("wcoj", detail.join(" . "), estimate, count as usize, start);
        }
        Ok(Some(count))
    }

    /// EXPLAIN's step for a worst-case-optimal join: its variable order with each one's
    /// candidates, its checks, and for a count the variables counted rather than bound.
    fn wcoj_step(&self, vars: &[Variable], stats: &wcoj::Stats, estimate: Option<u64>, rows: u64) {
        let Some(trace) = &self.trace else {
            return;
        };
        let load = |n: &AtomicUsize| n.load(AtomicOrdering::Relaxed);
        let order: Vec<String> = stats
            .order
            .iter()
            .zip(&stats.candidates)
            .map(|(&v, n)| format!("{} ({} candidates)", vars[v], load(n)))
            .collect();
        let counted = match stats.counted_after {
            Some(depth) if depth < stats.order.len() => {
                let rest: Vec<String> = stats.order[depth..]
                    .iter()
                    .map(|&v| vars[v].to_string())
                    .collect();
                format!(", {} counted as a forest", rest.join(", "))
            }
            _ => String::new(),
        };
        trace.borrow_mut().push(PlanStep {
            depth: self.depth.get() + 1,
            operator: "wcoj order".to_owned(),
            detail: format!(
                "{} | {} lookups ({} seeks, {} from the root), {} candidates skipped{counted}",
                order.join(", "),
                load(&stats.lookups),
                load(&stats.seeks),
                load(&stats.roots),
                load(&stats.skipped)
            ),
            estimated_rows: estimate,
            rows,
            micros: 0,
            cache: None,
        });
    }

    /// `DESCRIBE` (its answer is implementation-defined, §16.4): each term the solutions bind, once, with the
    /// statements of the default graph it is the subject of; a blank node such a statement
    /// has as its object is described in turn.
    fn describe(&self, solutions: &Solutions) -> NativeResult<Vec<nrese_rdf::Triple>> {
        let (predicate, object) = (
            Variable::new_unchecked("described predicate"),
            Variable::new_unchecked("described object"),
        );
        let Some(graph) = self.graph_slot() else {
            return Ok(Vec::new());
        };
        let table = &solutions.table;
        let mut described: HashSet<u64> = HashSet::new();
        let mut todo = Vec::new();
        let mut out = Vec::new();
        for row in 0..table.len() {
            for column in 0..table.width() {
                let id = table.get(row, column);
                if id != UNDEF && computed_index(id).is_none() && described.insert(id) {
                    todo.push(id);
                }
            }
            while let Some(node) = todo.pop() {
                let Some(subject) = self
                    .term(node)
                    .and_then(|t| nrese_rdf::NamedOrBlankNode::try_from(t).ok())
                else {
                    continue;
                };
                let scan = ScanPattern {
                    slots: [
                        Slot::Const(TermId::from_raw(node)),
                        Slot::Var(predicate.clone()),
                        Slot::Var(object.clone()),
                        graph.clone(),
                    ],
                };
                let statements = self.scan(&scan, None)?;
                let (p_column, o_column) = (
                    statements.column(&predicate).expect("scanned"),
                    statements.column(&object).expect("scanned"),
                );
                for r in 0..statements.table.len() {
                    let o_id = statements.table.get(r, o_column);
                    let (Some(Term::NamedNode(p)), Some(o)) = (
                        self.term(statements.table.get(r, p_column)),
                        self.term(o_id),
                    ) else {
                        continue;
                    };
                    if o.is_blank_node() && described.insert(o_id) {
                        todo.push(o_id);
                    }
                    out.push(nrese_rdf::Triple::new(subject.clone(), p, o));
                }
                self.consumed(&statements);
            }
        }
        Ok(out)
    }

    /// The graph position of a triple pattern in the active graph; `None` if the graph
    /// holds nothing.
    fn graph_slot(&self) -> Option<Slot> {
        Some(match &*self.graph.borrow() {
            GraphScope::Default => Slot::Const(TermId::DEFAULT_GRAPH),
            GraphScope::Union => Slot::Merged,
            GraphScope::Named(id) => Slot::Const(*id),
            GraphScope::Variable(v) => Slot::Var(v.clone()),
            GraphScope::Missing => return None,
        })
    }

    fn scan_pattern(&self, triple: &TriplePattern) -> Option<ScanPattern> {
        let slot = |term: &TermPattern| -> Option<Slot> {
            Some(match term {
                TermPattern::Variable(v) => Slot::Var(v.clone()),
                TermPattern::BlankNode(b) => {
                    Slot::Var(Variable::new_unchecked(format!("_bnode_{}", b.as_str())))
                }
                TermPattern::NamedNode(n) => Slot::Const(self.lookup_const(n.as_ref().into())?),
                TermPattern::Literal(l) => Slot::Const(self.lookup_const(l.as_ref().into())?),
                TermPattern::Triple(t) => {
                    let term: Term = nrese_rdf::Triple::try_from(t.as_ref().clone()).ok()?.into();
                    Slot::Const(self.lookup_const(term.as_ref())?)
                }
            })
        };
        let predicate = match &triple.predicate {
            NamedNodePattern::Variable(v) => Slot::Var(v.clone()),
            NamedNodePattern::NamedNode(n) => Slot::Const(self.lookup_const(n.as_ref().into())?),
        };
        let graph = self.graph_slot()?;
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
        let merged = scan.merged();
        // One run, no deletions, nothing to merge: whole columns, decoded a block at a time.
        if !merged && !scan.repeats_variable() {
            let components: Vec<usize> = columns_of.iter().map(|positions| positions[0]).collect();
            if let Some(columns) =
                self.snapshot
                    .scan_columns_in(self.model, &pattern, permutation, &components)
            {
                let table = if vars.is_empty() {
                    let rows = self.snapshot.count_in(self.model, &pattern) as usize;
                    IdTable::from_rows(0, std::iter::repeat_n(&[][..], rows))
                } else {
                    IdTable::from_columns(columns)
                };
                return self.produced(Solutions {
                    vars: vars.clone(),
                    table: table.assume_sorted_by(sorted_columns(scan, permutation, &vars)),
                    ordered: false,
                });
            }
        }
        let mut table = IdTable::new(vars.len());
        let mut row = vec![0u64; vars.len()];
        let quads = self
            .snapshot
            .scan_sorted_in(self.model, &pattern, permutation)
            .expect("permutation_for returns a usable permutation");
        let mut previous: Option<[u64; 3]> = None;
        'quads: for (n, quad) in quads.enumerate() {
            if n % (1 << 16) == 0 {
                self.check()?;
            }
            let components = quad.components();
            if merged {
                if let Some(graphs) = &self.merge_set
                    && !graphs.contains(&quad.graph)
                {
                    continue;
                }
                // Graph-last order: the copies of a statement follow each other.
                let statement = [components[0], components[1], components[2]];
                if previous == Some(statement) {
                    continue;
                }
                previous = Some(statement);
            }
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
        let table = table.assume_sorted_by(sorted_columns(scan, permutation, &vars));
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
        // Many ranges (the terms a dictionary string test passed): one scan of the whole
        // pattern against a bitmap of them beats a seek per range.
        if ranges.len() >= MANY_RANGES
            && let Slot::Var(object) = &scan.slots[2]
        {
            let mut solutions = self.scan(scan, Some(object))?;
            let column = solutions
                .column(object)
                .expect("the pattern binds its object");
            let set = IdSet::new(ranges);
            self.consumed(&solutions);
            solutions
                .table
                .par_retain(|table, row| set.contains(table.get(row, column)));
            return self.produced(solutions);
        }
        // Each range a block and a column at a time where the index can, else quad by quad.
        let mut out: Vec<Vec<u64>> = vec![Vec::new(); vars.len()];
        for &(low, high) in ranges {
            self.check()?;
            if let Some(decoded) = self.snapshot.scan_range_columns_in(
                self.model,
                &pattern,
                permutation,
                low,
                high,
                &columns,
            ) {
                for (column, values) in out.iter_mut().zip(decoded) {
                    column.extend(values);
                }
                continue;
            }
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
                for (column, &c) in out.iter_mut().zip(&columns) {
                    column.push(components[c]);
                }
            }
        }
        let object = vars
            .iter()
            .position(|v| scan.slots[2].is_var(v))
            .expect("ranged object");
        let table = IdTable::from_columns(out).assume_sorted_by(vec![object]);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }

    /// EXPLAIN's detail of an index nested-loop join: the pattern, its seeks in the
    /// index's runs and how many of them searched from the root.
    fn probed_detail(&self, pattern: String) -> String {
        let seeks = self.probed.get();
        format!(
            "{pattern} | {} seeks, {} from the root",
            seeks.seeks, seeks.root_searches
        )
    }

    /// Joins `result` with `scan` by probing the index once per distinct key of `result`.
    fn probe_join(
        &self,
        mut result: Solutions,
        scan: &ScanPattern,
        shared: &[Variable],
    ) -> NativeResult<Solutions> {
        let mut shared = shared.to_vec();
        if !result.ordered {
            // The keys sorted in the order the index reads the probed pattern: the probes
            // then ascend, and its cursor reads each run forward ([`Probe`]).
            let mut shape = scan.clone();
            for slot in &mut shape.slots {
                if matches!(slot, Slot::Var(v) if shared.contains(v)) {
                    *slot = Slot::Const(TermId::DEFAULT_GRAPH);
                }
            }
            let order = shape.quad_pattern().read_order();
            shared.sort_by_key(|v| {
                (0..4)
                    .filter(|&i| scan.slots[i].is_var(v))
                    .filter_map(|i| order.iter().position(|&c| c == i))
                    .min()
            });
        }
        let shared = &shared[..];
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
            snapshot: &self.snapshot,
            model: self.model,
            table: &result.table,
            scan,
            key_columns: &key_columns,
            shared_positions: &shared_positions,
            positions: &positions,
            width: vars.len(),
            max_rows: self.max_rows(vars.len()),
            produced: AtomicUsize::new(0),
            seeks: std::sync::Mutex::default(),
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
        self.probed
            .set(*probe.seeks.lock().unwrap_or_else(|e| e.into_inner()));
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
        let hash_table = self.charge_hash_table(left.table.len().min(right.table.len()))?;
        let max_rows = self.row_limit(vars.len());
        let table = if has_undef(&left.table, &lk) || has_undef(&right.table, &rk) {
            if left.ordered {
                outer_join_with_undef(&left.table, &right.table, &lk, &rk, None, false, max_rows)
            } else {
                join_with_undef(&left.table, &right.table, &lk, &rk, max_rows)
            }
        } else if left.ordered {
            join_keeping_left_order(&left.table, &right.table, &lk, &rk, max_rows)
        } else {
            join(&left.table, &right.table, &lk, &rk, max_rows)
        }
        .map_err(|e| self.too_large(e.max_rows.saturating_add(1), vars.len()))?;
        self.budget.release(hash_table);
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
        if let Some(expression) = expression
            && contains_any_exists(expression)
        {
            return self.left_join_with_exists(left, right, expression);
        }
        let (lk, rk) = shared_columns(&left, &right);
        // A variable that may be unbound on either side (from an earlier OPTIONAL) is
        // compatible with any value and takes it.
        let unbound_keys = has_undef(&left.table, &lk) || has_undef(&right.table, &rk);
        let vars = joined_vars(&left, &right, &rk);
        let hash_table = self.charge_hash_table(right.table.len())?;
        let max_rows = self.row_limit(vars.len());
        let accept = |row: &[u64]| {
            let binding = |v: &Variable| {
                let column = vars.iter().position(|x| x == v)?;
                self.term(row[column])
            };
            expression.is_none_or(|expression| self.evaluator.filter(expression, &binding))
        };
        let accept = expression
            .is_some()
            .then_some(&accept as &dyn Fn(&[u64]) -> bool);
        let table = if unbound_keys {
            outer_join_with_undef(&left.table, &right.table, &lk, &rk, accept, true, max_rows)
        } else {
            left_join(&left.table, &right.table, &lk, &rk, accept, max_rows)
        }
        .map_err(|e| self.too_large(e.max_rows.saturating_add(1), vars.len()))?;
        self.budget.release(hash_table);
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
            // No variables on either side: rows without columns, as many as there are.
            if vars.is_empty() {
                return IdTable::from_rows(0, std::iter::repeat_n(&[][..], s.table.len()));
            }
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
            GraphPattern::Filter { expr, inner } if !contains_any_exists(expr) => {
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
        if self.merge_set.is_some()
            || search::is_search(triple, patterns)
            || self.is_spatial(triple)
        {
            return Ok(None);
        }
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
        let start = Instant::now();
        let places: Vec<Vec<usize>> = vars_unique
            .iter()
            .map(|v| (0..4).filter(|&i| scan.slots[i].is_var(v)).collect())
            .collect();
        let mut out = IdTable::new(width);
        let mut chunk = IdTable::new(width);
        let mut row = vec![0u64; width];
        // The merged default graph is read in a graph-last order, where the copies of a
        // statement are adjacent, and all but the first are skipped.
        let mut quads: Box<dyn Iterator<Item = nrese_engine::EncodedQuad> + '_> = if scan.merged() {
            let sorted = self
                .snapshot
                .scan_sorted_in(self.model, &scan.quad_pattern(), scan.permutation_for(None))
                .expect("permutation_for returns a usable permutation");
            let mut previous: Option<[u64; 3]> = None;
            Box::new(sorted.filter(move |quad| {
                let components = quad.components();
                let statement = [components[0], components[1], components[2]];
                let first = previous != Some(statement);
                previous = Some(statement);
                first
            }))
        } else {
            Box::new(
                self.snapshot
                    .quads_for_pattern_in(self.model, &scan.quad_pattern()),
            )
        };
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
        if self.trace.is_some() {
            let count = self.snapshot.estimate_in(self.model, &scan.quad_pattern()) as f64;
            let kept = filter.map_or(1.0, pushdown::selectivity);
            let estimate = Some(((count * kept).round() as u64).min(limit as u64));
            let detail = format!("{triple} until {limit} rows");
            self.note("limit pushdown", detail, estimate, out.len(), start);
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
        let compiled = fast::compile(expression, &self.snapshot);
        let rows = solutions.table.len();
        let mask = FilterMask {
            snapshot: &self.snapshot,
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
        let snapshot: &Snapshot = &self.snapshot;
        let parts: Vec<Option<Vec<bool>>> = (0..rows.div_ceil(PARALLEL_EXPRESSION_ROWS))
            .into_par_iter()
            .map(|i| {
                if token.is_some_and(CancellationToken::is_cancelled) {
                    return None;
                }
                let decoder = Decoder::new(snapshot, computed);
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
            Expression::And(a, b) if contains_any_exists(expression) => {
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
            // EXISTS inside the expression: a column per EXISTS, which it reads.
            _ if contains_any_exists(expression) => {
                let (mut solutions, plain, added) = self.with_exists(solutions, expression)?;
                let mask = self.filter_mask(&solutions, &plain)?;
                solutions.table.retain_mask(&mask);
                Ok(drop_last_columns(solutions, added))
            }
            _ => {
                let mask = self.filter_mask(&solutions, expression)?;
                solutions.table.retain_mask(&mask);
                Ok(solutions)
            }
        }
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
                memo: RefCell::default(),
                numbers: RefCell::default(),
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
        let (snapshot, evaluator): (&Snapshot, _) = (&self.snapshot, &self.evaluator);
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
                    memo: RefCell::default(),
                    numbers: RefCell::default(),
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

    /// Ranks of `column`'s values in SPARQL's order. With `top = (k, descending)` only the
    /// `k` values that come first in that direction are told apart; the others share the
    /// rank that comes last (they cover at least `k` rows, so no row of the others is
    /// among the first `k`): a selection instead of a sort, for ORDER BY … LIMIT k.
    fn order_ranks(&self, column: &[u64], top: Option<(usize, bool)>) -> Vec<u64> {
        // Plain inline xsd:integers only: their lexical forms are canonical, so their value
        // is their place in the total order. Integers of derived datatypes of one value
        // (`"6"^^xsd:nonNegativeInteger` and `"6"^^xsd:integer`) differ there by datatype,
        // which the general path below takes into account (fuzz campaign, seed 2458).
        let integer = |id: u64| {
            let id = TermId::from_raw(id);
            match id.kind() {
                nrese_engine::TermKind::Integer => id.as_inline_integer(),
                _ => None,
            }
        };
        if column
            .iter()
            .all(|&id| id == UNDEF || integer(id).is_some())
        {
            // By value, above UNDEF, which sorts first in SPARQL.
            return column
                .iter()
                .map(|&id| match integer(id) {
                    Some(value) => (value + (1 << 62)) as u64 + 1,
                    None => 0,
                })
                .collect();
        }
        let mut distinct = column.to_vec();
        distinct.par_sort_unstable();
        distinct.dedup();
        // Stored terms are decoded and their values parsed on every core where there are
        // many (YAGO's 18 k populations, `"+4400"^^xsd:decimal`, are dictionary entries).
        let terms: Vec<value::Sortable> = if distinct.len() >= PARALLEL_RANKS
            && distinct.iter().all(|&id| computed_index(id).is_none())
        {
            let snapshot = &*self.snapshot;
            distinct
                .par_iter()
                .map(|&id| {
                    let term = (id != UNDEF)
                        .then(|| snapshot.decode(TermId::from_raw(id)))
                        .flatten();
                    value::Sortable::new(term)
                })
                .collect()
        } else {
            distinct
                .iter()
                .map(|&id| value::Sortable::new(self.term(id)))
                .collect()
        };
        let mut by_term: Vec<usize> = (0..distinct.len()).collect();
        if let Some((k, descending)) = top
            && k > 0
            && k < distinct.len()
        {
            // The k first distinct values in the direction, in order; the rest last.
            let first = |a: &usize, b: &usize| match descending {
                false => terms[*a].order(&terms[*b]),
                true => terms[*b].order(&terms[*a]),
            };
            by_term.select_nth_unstable_by(k - 1, first);
            // Values equal to the k-th (other terms of the same value, `"0"` and `"00"`)
            // are among the first too: they tie in this key, so the next key decides
            // between their rows, as the full sort would.
            let kth = by_term[k - 1];
            let tail = &mut by_term[k..];
            let mut tied = 0;
            for i in 0..tail.len() {
                if terms[tail[i]].order(&terms[kth]).is_eq() {
                    tail.swap(tied, i);
                    tied += 1;
                }
            }
            by_term.truncate(k + tied);
            by_term.sort_by(first);
            let rest = match descending {
                false => k as u64 + 1,
                true => 0,
            };
            let mut ranks: HashMap<u64, u64> = HashMap::with_capacity(k);
            let mut rank = 0;
            for (i, &d) in by_term.iter().enumerate() {
                if i > 0 && terms[by_term[i - 1]].order(&terms[d]).is_ne() {
                    rank += 1;
                }
                // Ascending ranks follow the values: the first in a descending order are
                // the largest.
                let value_rank = match descending {
                    false => rank + 1,
                    true => k as u64 - rank,
                };
                ranks.insert(distinct[d], value_rank);
            }
            return column
                .iter()
                .map(|id| ranks.get(id).copied().unwrap_or(rest))
                .collect();
        }
        by_term.par_sort_by(|&a, &b| terms[a].order(&terms[b]));
        let mut rank_of = vec![0u64; distinct.len()];
        let mut rank = 0;
        for (i, &d) in by_term.iter().enumerate() {
            if i > 0 && terms[by_term[i - 1]].order(&terms[d]).is_ne() {
                rank += 1;
            }
            rank_of[d] = rank;
        }
        column
            .iter()
            .map(|id| rank_of[distinct.binary_search(id).expect("id is in the column")])
            .collect()
    }

    /// The values of the sort `keys` for `rows` of `solutions`, by position in `rows`. A key
    /// that is a variable sorts by rank ([`Self::order_ranks`]: inline integers by id, so
    /// no term is decoded); others are evaluated per row.
    fn sort_keys(
        &self,
        solutions: &Solutions,
        keys: &[OrderExpression],
        rows: &[usize],
        limit: Option<usize>,
    ) -> Vec<SortKey> {
        keys.iter()
            .enumerate()
            .map(|(i, key)| {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = key;
                if let Expression::Variable(v) = e
                    && let Some(column) = solutions.column(v)
                {
                    let column = solutions.table.column(column);
                    let values: Vec<u64> = rows.iter().map(|&row| column[row]).collect();
                    // With a limit, the first key needs only its first values told apart
                    // when it is the only key (later keys break its ties).
                    let descending = matches!(key, OrderExpression::Desc(_));
                    let top = limit
                        .filter(|_| i == 0 && keys.len() == 1)
                        .map(|k| (k, descending));
                    return SortKey::Ids(self.order_ranks(&values, top));
                }
                SortKey::Terms(
                    rows.iter()
                        .map(|&row| self.evaluator.eval(e, &self.binding(solutions, row)))
                        .collect(),
                )
            })
            .collect()
    }

    fn order_by(
        &self,
        mut solutions: Solutions,
        keys: &[OrderExpression],
        limit: Option<usize>,
    ) -> NativeResult<Solutions> {
        let n = solutions.table.len();
        // The order of positions `a` and `b` by `values` of `keys` (no tie-break).
        let by_keys = |keys: &[OrderExpression], values: &[SortKey], a: usize, b: usize| {
            for (key, values) in keys.iter().zip(values) {
                let ordering = match values {
                    SortKey::Ids(ids) => ids[a].cmp(&ids[b]),
                    SortKey::Terms(terms) => value::order(terms[a].as_ref(), terms[b].as_ref()),
                };
                let ordering = match key {
                    OrderExpression::Asc(_) => ordering,
                    OrderExpression::Desc(_) => ordering.reverse(),
                };
                if ordering.is_ne() {
                    return ordering;
                }
            }
            std::cmp::Ordering::Equal
        };
        // ORDER BY with LIMIT k and several keys: the first key alone decides which rows
        // can be among the first k (those up to the k-th row's value, ties included); the
        // other keys are computed for those only. Olympics q4 ranked 28 k athletes' IRIs
        // to break ties among the top 10 medal counts.
        let mut rows: Vec<usize> = (0..n).collect();
        if let Some(k) = limit
            && k > 0
            && k < n
            && keys.len() > 1
        {
            let first = self.sort_keys(&solutions, &keys[..1], &rows, Some(k));
            let cmp = |a: &usize, b: &usize| by_keys(&keys[..1], &first, *a, *b);
            let mut positions = rows.clone();
            positions.select_nth_unstable_by(k - 1, cmp);
            let pivot = positions[k - 1];
            rows.retain(|row| cmp(row, &pivot).is_le());
        }
        let key_values = self.sort_keys(&solutions, keys, &rows, limit);
        let compare = |a: &usize, b: &usize| {
            by_keys(keys, &key_values, *a, *b).then_with(|| rows[*a].cmp(&rows[*b]))
        };
        let mut order: Vec<usize> = (0..rows.len()).collect();
        match limit {
            Some(k) if k < order.len() => {
                order.select_nth_unstable_by(k, compare);
                order.truncate(k);
                order.sort_unstable_by(compare);
            }
            _ => order.sort_unstable_by(compare),
        }
        let order: Vec<usize> = order.into_iter().map(|position| rows[position]).collect();
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

    /// GROUP BY: evaluated directly, or in morsels where the pattern's rows don't fit the
    /// query's memory ([`stream`]: as a retry after the direct evaluation ran out, its
    /// memory released first).
    fn group(
        &self,
        inner: &GraphPattern,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
        having: Option<&Expression>,
    ) -> NativeResult<Solutions> {
        let streamable = match self.as_written {
            true => None,
            false => stream::Plan::of(inner, aggregates),
        };
        if let Some(plan) = &streamable
            && self.stream_rows.is_some()
        {
            return self.group_streamed(plan, variables, aggregates);
        }
        let before = self.budget.used();
        match self.group_direct(inner, variables, aggregates, having) {
            Err(error) if streamable.is_some() && stream::is_memory_limit(&error) => {
                self.budget
                    .release(self.budget.used().saturating_sub(before));
                let plan = streamable.as_ref().expect("checked");
                self.group_streamed(plan, variables, aggregates)
            }
            result => result,
        }
    }

    fn group_direct(
        &self,
        inner: &GraphPattern,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
        having: Option<&Expression>,
    ) -> NativeResult<Solutions> {
        // COUNT(*) of one triple pattern without GROUP BY: the index knows the answer.
        if variables.is_empty()
            && let [(target, AggregateExpression::CountSolutions { distinct: false })] = aggregates
            && let GraphPattern::Bgp { patterns } = inner
            && let [triple] = patterns.as_slice()
            && !search::is_search(triple, patterns)
            && !self.is_spatial(triple)
        {
            let count = match self.scan_pattern(triple) {
                // The index counts quads; in the merged default graph a statement in
                // several graphs is several quads, so there the scan counts.
                Some(scan) if !scan.repeats_variable() && !scan.merged() => {
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
        // COUNT(*) of a cyclic BGP: its worst-case-optimal join counts the solutions
        // instead of producing them (4-cycles of a social graph: 19.2 M two-paths
        // checked → two lists of 2-paths per node).
        if variables.is_empty()
            && let [(target, AggregateExpression::CountSolutions { distinct: false })] = aggregates
            && let GraphPattern::Bgp { patterns } = inner
            && let Some(count) = self.cyclic_count(patterns)?
        {
            let mut table = IdTable::new(1);
            table.push_row(&[self.id(&integer(count))]);
            return Ok(Solutions {
                vars: vec![target.clone()],
                table,
                ordered: false,
            });
        }
        // COUNT(*) of an open closure (`?a p* ?b`, `?a p+ ?b`): from the closure's size
        // per node, without building its pairs. Its pairs are distinct, so DISTINCT counts
        // the same.
        let begun = Instant::now();
        if variables.is_empty()
            && let [(target, AggregateExpression::CountSolutions { .. })] = aggregates
            && let GraphPattern::Path {
                subject,
                path,
                object,
            } = inner
            && let (Ok(start), Ok(end)) = (self.path_end(subject), self.path_end(object))
            && start != end
            && let Some(count) = self
                .path_evaluator()?
                .count_open(&paths::Path::resolve(path, &self.snapshot))
        {
            if self.trace.is_some() {
                let detail = format!("COUNT of {path} from the closure's size per component");
                self.note("closure", detail, Some(1), 1, begun);
            }
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
            && !search::is_search(triple, patterns)
            && !self.is_spatial(triple)
            && let Some(scan) = self.scan_pattern(triple)
            && !scan.repeats_variable()
            && !scan.merged()
            && aggregates
                .iter()
                .all(|(_, aggregate)| counts_rows(aggregate, &scan))
            && let Some(component) = (0..4).find(|&c| scan.slots[c].is_var(key))
        {
            let permutation = scan.permutation_for(Some(key));
            let start = std::time::Instant::now();
            if scan.first_free(permutation) == Some(component)
                && let Some(groups) =
                    self.snapshot
                        .group_counts_in(self.model, &scan.quad_pattern(), permutation)
            {
                if self.trace.is_some() {
                    let detail = format!("{key} of {triple}");
                    let count = self.snapshot.estimate_in(self.model, &scan.quad_pattern());
                    let estimate = Some(self.distinct(&scan, key, count));
                    self.note("group count", detail, estimate, groups.len(), start);
                }
                let mut columns = vec![
                    groups
                        .iter()
                        .map(|(value, _)| value.raw())
                        .collect::<Vec<_>>(),
                ];
                let counts: Vec<u64> = groups
                    .iter()
                    .map(|&(_, n)| match TermId::inline_integer(n as i64) {
                        Some(id) => id.raw(),
                        None => self.id(&integer(n)),
                    })
                    .collect();
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
        if !self.as_written
            && !variables.is_empty()
            && let Some(solutions) = self.group_crossed(inner, variables, aggregates)?
        {
            return Ok(solutions);
        }
        // Aggregates that ignore duplicates let the pattern be evaluated as a set, and
        // OPTIONALs that only feed them be aggregated apart from the rest (`sets`).
        let solutions = match sets::insensitive_arguments(aggregates) {
            Some(arguments) if sets::joins(inner) && !self.as_written => {
                if !variables.is_empty()
                    && let Some(solutions) =
                        self.group_detached(inner, variables, aggregates, &arguments, having)?
                {
                    return Ok(solutions);
                }
                let mut needed = variables.to_vec();
                for variable in arguments.into_iter().flatten() {
                    if !needed.contains(&variable) {
                        needed.push(variable);
                    }
                }
                self.eval_set(inner, &needed)?
            }
            _ => self.eval(inner)?,
        };
        self.group_solutions(solutions, variables, aggregates)
    }

    /// GROUP BY over a cross product: (filters over) a join of two patterns that share no
    /// variable, with every key bound by one side. The product is made in chunks of that
    /// side's rows, all rows of a key in the same chunk, and each chunk is filtered and
    /// grouped on its own: its groups are complete, and memory holds one chunk instead of
    /// the product. BSBM BI q4 crossed 2.5 k features with 63 k offers (155 M rows, 10 GB).
    /// `None` for other patterns.
    fn group_crossed(
        &self,
        inner: &GraphPattern,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
    ) -> NativeResult<Option<Solutions>> {
        let mut filters = Vec::new();
        let mut pattern = inner;
        while let GraphPattern::Filter { expr, inner } = pattern {
            filters.push(expr);
            pattern = inner;
        }
        let GraphPattern::Join { left, right } = pattern else {
            return Ok(None);
        };
        let (left_vars, right_vars) = (sets::in_scope(left), sets::in_scope(right));
        if sets::shares(&left_vars, &right_vars)
            || as_path(left).is_some()
            || as_path(right).is_some()
            || matches!(**left, GraphPattern::Service { .. })
            || matches!(**right, GraphPattern::Service { .. })
        {
            return Ok(None);
        }
        let (keyed, other) = if variables.iter().all(|v| left_vars.contains(v)) {
            (left, right)
        } else if variables.iter().all(|v| right_vars.contains(v)) {
            (right, left)
        } else {
            return Ok(None);
        };
        let keyed = self.eval(keyed)?;
        let other = self.eval(other)?;
        // Rows of the keyed side per chunk, so that a chunk's product has about
        // CROSS_CHUNK_ROWS rows.
        let per_chunk = (self.cross_chunk_rows / other.table.len().max(1)).max(1);
        // A key in scope but never bound has no column: unbound in every row, it splits no
        // group.
        let key_columns: Vec<usize> = variables.iter().filter_map(|v| keyed.column(v)).collect();
        let mut order: Vec<usize> = (0..keyed.table.len()).collect();
        if keyed.table.len() > per_chunk {
            let key_of = |row: usize| -> Vec<u64> {
                key_columns
                    .iter()
                    .map(|&c| keyed.table.get(row, c))
                    .collect()
            };
            order.sort_by_cached_key(|&row| key_of(row));
        }
        let same_key = |a: usize, b: usize| {
            key_columns
                .iter()
                .all(|&c| keyed.table.get(a, c) == keyed.table.get(b, c))
        };
        let mut parts = Vec::new();
        let mut start = 0;
        while start < order.len() {
            let mut end = (start + per_chunk).min(order.len());
            // A key's rows stay together.
            while end < order.len() && same_key(order[end - 1], order[end]) {
                end += 1;
            }
            let rows = &order[start..end];
            let columns: Vec<Vec<u64>> = keyed
                .table
                .columns()
                .iter()
                .map(|column| rows.iter().map(|&row| column[row]).collect())
                .collect();
            let chunk = self.produced(Solutions {
                vars: keyed.vars.clone(),
                table: if columns.is_empty() {
                    IdTable::from_rows(0, std::iter::repeat_n(&[][..], rows.len()))
                } else {
                    IdTable::from_columns(columns)
                },
                ordered: false,
            })?;
            let copy = self.produced(Solutions {
                vars: other.vars.clone(),
                table: other.table.clone(),
                ordered: false,
            })?;
            let mut crossed = self.join(chunk, copy)?;
            for filter in filters.iter().rev() {
                crossed = self.filter(crossed, filter)?;
            }
            parts.push(self.group_solutions(crossed, variables, aggregates)?);
            start = end;
        }
        self.consumed(&keyed);
        self.consumed(&other);
        let Some(first) = parts.first() else {
            // No keyed rows: no groups (with GROUP BY, an empty input has none).
            let mut vars = variables.to_vec();
            vars.extend(aggregates.iter().map(|(target, _)| target.clone()));
            return Ok(Some(Solutions {
                table: IdTable::new(vars.len()),
                vars,
                ordered: false,
            }));
        };
        let vars = first.vars.clone();
        let width = first.table.width();
        let tables = parts
            .into_iter()
            .map(|part| {
                self.consumed(&part);
                part.table
            })
            .collect();
        Ok(Some(self.produced(Solutions {
            vars,
            table: IdTable::concat(width, tables),
            ordered: false,
        })?))
    }

    /// Every aggregate of every group in one pass over the rows, where each is `COUNT(*)`,
    /// `COUNT` of a variable, or `SUM` or `AVG` of an expression of one variable whose
    /// values are numbers (none DISTINCT): each id's number is found once, and the rows
    /// are read in order instead of per group. `None` otherwise (and where a value isn't a
    /// number: the general path decides, durations included). BSBM BI q4: 154 M rows of
    /// `AVG(xsd:float(xsd:string(?price)))` at 80 ns a row per group, now a pass.
    fn numeric_pass(
        &self,
        solutions: &Solutions,
        group_of: &[u32],
        groups: usize,
        aggregates: &[(Variable, AggregateExpression)],
    ) -> Option<Vec<Vec<Agg>>> {
        enum Plan<'e> {
            Rows,
            Bound(usize),
            Total {
                column: usize,
                expr: &'e Expression,
                average: bool,
            },
        }
        let mut plans = Vec::with_capacity(aggregates.len());
        let mut totals = 0;
        for (_, aggregate) in aggregates {
            plans.push(match aggregate {
                AggregateExpression::CountSolutions { distinct: false } => Plan::Rows,
                AggregateExpression::FunctionCall {
                    name: AggregateFunction::Count,
                    expr: Expression::Variable(variable),
                    distinct: false,
                } => Plan::Bound(solutions.column(variable)?),
                AggregateExpression::FunctionCall {
                    name: name @ (AggregateFunction::Sum | AggregateFunction::Avg),
                    expr,
                    distinct: false,
                } if !pushdown::per_solution(expr) => {
                    let mut columns: Vec<usize> = expression_variables(expr)
                        .iter()
                        .filter_map(|v| solutions.column(v))
                        .collect();
                    columns.sort_unstable();
                    columns.dedup();
                    let [column] = columns[..] else {
                        return None;
                    };
                    totals += 1;
                    Plan::Total {
                        column,
                        expr,
                        average: *name == AggregateFunction::Avg,
                    }
                }
                _ => return None,
            });
        }
        // Only worth it where a total is computed; counts alone take the other paths.
        if totals == 0 {
            return None;
        }
        #[derive(Clone, Copy)]
        struct State {
            count: u64,
            total: Option<Numeric>,
            error: bool,
        }
        let width = plans.len();
        let table = &solutions.table;
        // Every id's number, found once per query: the chunks of a cross product
        // ([`Self::group_crossed`]) share their values.
        for plan in &plans {
            let Plan::Total { column, expr, .. } = plan else {
                continue;
            };
            // The expression and the variable it reads here: with another one bound, the
            // same id may give another value.
            let key = ((*expr).clone(), solutions.vars[*column].clone());
            let mut missing: nrese_exec::IdMap<usize> = nrese_exec::IdMap::default();
            {
                let numbers = self.numbers.borrow();
                let known = numbers.get(&key);
                for (row, &id) in table.column(*column).iter().enumerate() {
                    if !known.is_some_and(|known| known.contains_key(&id)) {
                        missing.entry(id).or_insert(row);
                    }
                }
            }
            let missing: Vec<(u64, usize)> = missing.into_iter().collect();
            let number = |aggregator: &Aggregator<'_>, row: usize| {
                let binding = aggregator.binding(solutions, row);
                match aggregator.evaluator.eval(expr, &binding) {
                    None => Number::Error,
                    Some(term) => match Numeric::of(&term) {
                        Some(number) => Number::Value(number),
                        None => Number::Other,
                    },
                }
            };
            let found: Vec<(u64, Number)> = if missing.len() < 2 * PARALLEL_EXPRESSION_ROWS {
                let term = |id: u64| self.term(id);
                let aggregator = Aggregator {
                    evaluator: &self.evaluator,
                    term: &term,
                    memo: RefCell::default(),
                    numbers: RefCell::default(),
                };
                missing
                    .iter()
                    .map(|&(id, row)| (id, number(&aggregator, row)))
                    .collect()
            } else {
                let computed = self.computed.borrow();
                let computed: &[Term] = &computed;
                let (snapshot, evaluator): (&Snapshot, _) = (&self.snapshot, &self.evaluator);
                missing
                    .par_chunks(PARALLEL_EXPRESSION_ROWS)
                    .flat_map_iter(|part| {
                        let decoder = Decoder::new(snapshot, computed);
                        let term = |id: u64| decoder.term(id);
                        let aggregator = Aggregator {
                            evaluator,
                            term: &term,
                            memo: RefCell::default(),
                            numbers: RefCell::default(),
                        };
                        part.iter()
                            .map(|&(id, row)| (id, number(&aggregator, row)))
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            self.numbers
                .borrow_mut()
                .entry(key)
                .or_default()
                .extend(found);
        }
        let numbers = self.numbers.borrow();
        let known: Vec<Option<&nrese_exec::IdMap<Number>>> = plans
            .iter()
            .map(|plan| match plan {
                Plan::Total { expr, column, .. } => {
                    numbers.get(&((*expr).clone(), solutions.vars[*column].clone()))
                }
                _ => None,
            })
            .collect();
        // The states of groups `range`, from every row in order; `None` once a value isn't
        // a number.
        let pass = |range: std::ops::Range<usize>| {
            let start = State {
                count: 0,
                total: Some(Numeric::Integer(Integer::from(0))),
                error: false,
            };
            let mut states = vec![start; range.len() * width];
            for (row, &group) in group_of.iter().enumerate() {
                let group = group as usize;
                if !range.contains(&group) {
                    continue;
                }
                let at = (group - range.start) * width;
                for (a, plan) in plans.iter().enumerate() {
                    let state = &mut states[at + a];
                    match plan {
                        Plan::Rows => state.count += 1,
                        Plan::Bound(column) => {
                            if table.get(row, *column) != UNDEF {
                                state.count += 1;
                            }
                        }
                        Plan::Total { column, .. } => {
                            let id = table.get(row, *column);
                            state.count += 1;
                            match known[a].and_then(|known| known.get(&id)) {
                                Some(Number::Value(number)) => {
                                    state.total = state.total.and_then(|total| total.add(*number));
                                }
                                Some(Number::Error) => state.error = true,
                                Some(Number::Other) | None => return None,
                            }
                        }
                    }
                }
            }
            Some(states)
        };
        // Parts of the groups on every core where there are many rows: each part reads the
        // rows in order, so every group adds its values in the order the general path does.
        let parts = rayon::current_num_threads().min(groups).max(1);
        let states: Vec<State> = if group_of.len() < 2 * PARALLEL_EXPRESSION_ROWS || parts < 2 {
            pass(0..groups)?
        } else {
            let done: Vec<Option<Vec<State>>> = (0..parts)
                .into_par_iter()
                .map(|part| pass(part * groups / parts..(part + 1) * groups / parts))
                .collect();
            let mut states = Vec::with_capacity(groups * width);
            for part in done {
                states.extend(part?);
            }
            states
        };
        // Kept for the next chunk only while small: a GROUP BY over many distinct values
        // would hold all their numbers to the end of the query.
        drop(known);
        drop(numbers);
        self.numbers
            .borrow_mut()
            .retain(|_, known| known.len() <= NUMBER_MEMO_ENTRIES);
        Some(
            (0..groups)
                .map(|group| {
                    plans
                        .iter()
                        .enumerate()
                        .map(|(a, plan)| {
                            let state = states[group * width + a];
                            match plan {
                                Plan::Rows | Plan::Bound(_) => integer_agg(state.count as i64),
                                Plan::Total { average, .. } => {
                                    finish_total(state.total, state.error, state.count, *average)
                                }
                            }
                        })
                        .collect()
                })
                .collect(),
        )
    }

    /// GROUP BY over evaluated `solutions`.
    fn group_solutions(
        &self,
        solutions: Solutions,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
    ) -> NativeResult<Solutions> {
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
        let one_pass = aggregate_in_one_pass(&solutions, &groups.group_of, group_count, aggregates)
            .or_else(|| self.numeric_pass(&solutions, &groups.group_of, group_count, aggregates));
        let values = match one_pass {
            Some(values) => values,
            None => {
                let mut members: Vec<Vec<usize>> = vec![Vec::new(); group_count];
                for (row, &group) in groups.group_of.iter().enumerate() {
                    members[group as usize].push(row);
                }
                self.aggregate_groups(&solutions, &members, aggregates)?
            }
        };
        let mut columns: Vec<Vec<u64>> = groups.keys.clone().into_columns();
        let mut vars: Vec<Variable> = variables.to_vec();
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
        GraphPattern::Graph { name, .. } => ("graph", name.to_string()),
        GraphPattern::Service { name, .. } => ("service", name.to_string()),
        GraphPattern::Lateral { .. } => ("lateral", String::new()),
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
    /// Whether each row of `rows` passes. Compiled filters read the dictionary under one
    /// read lock for all the rows ([`Snapshot::with_views`]), not one per row.
    fn rows(&self, rows: Range<usize>, term: &dyn Fn(u64) -> Option<Term>) -> Vec<bool> {
        let table = &self.solutions.table;
        self.snapshot.with_views(|view| {
            rows.map(|row| {
                let decided = self.compiled.map(|fast| {
                    let value = |v: &Variable| {
                        self.solutions
                            .column(v)
                            .map_or(UNDEF, |c| table.get(row, c))
                    };
                    fast.eval(&value, view)
                });
                match decided {
                    Some(fast::Tri::True) => true,
                    Some(fast::Tri::False | fast::Tri::Error) => false,
                    Some(fast::Tri::Unknown) | None => {
                        let binding =
                            |v: &Variable| term(table.get(row, self.solutions.column(v)?));
                        self.evaluator.filter(self.expression, &binding)
                    }
                }
            })
            .collect()
        })
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
    /// The index seeks of all chunks.
    seeks: std::sync::Mutex<nrese_engine::SeekStats>,
}

/// Why an index nested-loop join stopped early.
enum ProbeStop {
    Cancelled,
    TooManyRows,
}

impl Probe<'_> {
    /// The joined rows for `table` rows `rows`, in order. One probe cursor reads the index
    /// for all of them: the keys ascend in its order (`probe_join`), so each probe seeks
    /// forward from the last instead of searching every run from the root. The matches
    /// of a key are kept flat, `positions.len()` values each: nothing is allocated per
    /// match.
    fn rows(
        &self,
        rows: Range<usize>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<IdTable, ProbeStop> {
        let table = self.table;
        let width = table.width();
        let stride = self.positions.len();
        let mut out = IdTable::new(self.width);
        let mut cursor = self.snapshot.probe_cursor(self.model);
        // The new variables' values of each match of the current key, and their number
        // (a pattern binding nothing new matches without values).
        let mut matches: Vec<u64> = Vec::new();
        let mut found = 0usize;
        let mut row = vec![0u64; self.width];
        let mut bound = self.scan.clone();
        let first = rows.start;
        let mut counted = 0;
        let finish = |cursor: &nrese_engine::ProbeCursor<'_>| {
            let mut seeks = self.seeks.lock().unwrap_or_else(|e| e.into_inner());
            seeks.add(cursor.stats());
        };
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
                found = 0;
                for (slots, &k) in self.shared_positions.iter().zip(self.key_columns) {
                    let id = TermId::from_raw(table.get(r, k));
                    for &i in slots {
                        bound.slots[i] = Slot::Const(id);
                    }
                }
                cursor.for_each(&bound.quad_pattern(), |quad| {
                    let components = quad.components();
                    let start = matches.len();
                    for places in self.positions {
                        let value = components[places[0]];
                        if places[1..].iter().any(|&p| components[p] != value) {
                            matches.truncate(start);
                            return;
                        }
                        matches.push(value);
                    }
                    found += 1;
                });
                if self.scan.merged() && found > 1 {
                    // A statement in several graphs matched once per graph; its values
                    // are the same each time.
                    let mut distinct: Vec<&[u64]> = match stride {
                        0 => vec![&[]],
                        _ => matches.chunks_exact(stride).collect(),
                    };
                    distinct.sort_unstable();
                    distinct.dedup();
                    found = distinct.len();
                    matches = distinct.concat();
                }
            }
            for m in 0..found {
                for (c, slot) in row.iter_mut().enumerate().take(width) {
                    *slot = table.get(r, c);
                }
                row[width..].copy_from_slice(&matches[m * stride..(m + 1) * stride]);
                out.push_row(&row);
            }
            // One key can match a whole index range: check within large fan-outs too.
            if out.len() - counted > 1 << 16 {
                self.grow(out.len() - counted)?;
                counted = out.len();
            }
        }
        finish(&cursor);
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
    /// The value of an expression over one variable, per (expression, id): groups share
    /// most of their values, and `STR(?x)` of an id is the same in each of them.
    memo: RefCell<HashMap<(usize, u64), Option<Term>>>,
    /// The same for SUM and AVG: the value as a number ([`Aggregator::numeric_total`]).
    numbers: RefCell<HashMap<(usize, u64), Number>>,
}

/// An expression's value for SUM and AVG.
#[derive(Clone, Copy)]
enum Number {
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
                            Some(term) => match Numeric::of(&term) {
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

    fn binding<'s>(
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
fn finish_total(total: Option<Numeric>, error: bool, count: u64, average: bool) -> Agg {
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

/// An `xsd:integer` result as an inline id where it fits, else as a term.
fn integer_agg(value: i64) -> Agg {
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
fn aggregate_in_one_pass(
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
        total = total.add(Numeric::of(value)?)?;
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

/// The values, if the first is a year-month or day-time duration (`SUM` and `AVG` of
/// durations, SEP-0002); `None` for numbers.
fn durations(values: &[Term]) -> Option<Vec<Value>> {
    let first = value::Value::of(values.first()?);
    if !calendar::is_summable_duration(&first) {
        return None;
    }
    Some(values.iter().map(value::Value::of).collect())
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

/// `solutions` without its last `count` columns.
fn drop_last_columns(mut solutions: Solutions, count: usize) -> Solutions {
    if count == 0 {
        return solutions;
    }
    let rows = solutions.table.len();
    let mut columns = std::mem::take(&mut solutions.table).into_columns();
    columns.truncate(columns.len() - count);
    solutions.vars.truncate(solutions.vars.len() - count);
    solutions.table = if columns.is_empty() {
        IdTable::from_rows(0, std::iter::repeat_n(&[][..], rows))
    } else {
        IdTable::from_columns(columns)
    };
    solutions
}

/// The distinct values two sorted lists have in common: one merge.
fn sorted_overlap(a: &[u64], b: &[u64]) -> u64 {
    let (mut i, mut j, mut both) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let value = a[i];
                both += 1;
                while i < a.len() && a[i] == value {
                    i += 1;
                }
                while j < b.len() && b[j] == value {
                    j += 1;
                }
            }
        }
    }
    both
}

fn has_undef(table: &IdTable, columns: &[usize]) -> bool {
    columns.iter().any(|&c| table.column(c).contains(&UNDEF))
}

/// True if every conjunct of `expr` is applied while the joins of `patterns` run: it can
/// move (no EXISTS, no draws per row) and the patterns bind all its variables.
fn placed_in(expr: &Expression, patterns: &[TriplePattern]) -> bool {
    let bound: Vec<Variable> = patterns.iter().flat_map(triple_variables).collect();
    let mut conjuncts = Vec::new();
    pushdown::conjuncts_of(expr, &mut conjuncts);
    conjuncts.into_iter().all(|conjunct| {
        !contains_any_exists(conjunct)
            && pushdown::movable(conjunct)
            && expression_variables(conjunct)
                .iter()
                .all(|v| bound.contains(v))
    })
}

/// Sorted, disjoint id ranges for membership tests: single ids of one kind in a bitmap over
/// their payloads, the few wider ranges (whole kinds) in a list.
struct IdSet {
    /// Per kind tag: a bitmap over payloads, if the kind has single ids.
    bits: Vec<Vec<u64>>,
    wide: Vec<(u64, u64)>,
}

impl IdSet {
    fn new(ranges: &[(TermId, TermId)]) -> Self {
        let mut set = IdSet {
            bits: vec![Vec::new(); 16],
            wide: Vec::new(),
        };
        for &(low, high) in ranges {
            if low.kind() != high.kind() || high.payload() - low.payload() >= 64 {
                set.wide.push((low.raw(), high.raw()));
                continue;
            }
            let bits = &mut set.bits[low.kind() as usize];
            for payload in low.payload()..=high.payload() {
                let word = (payload / 64) as usize;
                if bits.len() <= word {
                    bits.resize(word + 1, 0);
                }
                bits[word] |= 1 << (payload % 64);
            }
        }
        set
    }

    #[inline]
    fn contains(&self, id: u64) -> bool {
        let term = TermId::from_raw(id);
        let payload = term.payload();
        let in_bits = self.bits[term.kind() as usize]
            .get((payload / 64) as usize)
            .is_some_and(|word| word & (1 << (payload % 64)) != 0);
        in_bits || self.wide.iter().any(|&(low, high)| low <= id && id <= high)
    }
}

/// The columns (indexes into `vars`) a scan of `scan` in `permutation` is sorted on: its
/// free components in permutation order.
fn sorted_columns(scan: &ScanPattern, permutation: Permutation, vars: &[Variable]) -> Vec<usize> {
    let mut sorted = Vec::new();
    for &component in permutation.order().iter() {
        if let Slot::Var(v) = &scan.slots[component]
            && let Some(column) = vars.iter().position(|x| x == v)
            && !sorted.contains(&column)
        {
            sorted.push(column);
        }
    }
    sorted
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum Slot {
    Var(Variable),
    Const(TermId),
    /// The graph position of a pattern in the merged default graph: any graph, and a
    /// statement that several graphs hold counts once. Reads in this scope use a
    /// graph-last order, where the copies of a statement are adjacent, and drop them.
    Merged,
}

impl Slot {
    fn is_var(&self, variable: &Variable) -> bool {
        matches!(self, Slot::Var(v) if v == variable)
    }
}

/// A triple pattern with constants resolved to ids, in a graph: `slots[3]` is the default
/// graph, a named graph, a graph variable (any named graph, inside `GRAPH ?g`), or
/// [`Slot::Merged`].
#[derive(Clone, Debug)]
struct ScanPattern {
    slots: [Slot; 4],
}

impl ScanPattern {
    fn quad_pattern(&self) -> QuadPattern {
        let constant = |slot: &Slot| match slot {
            Slot::Const(id) => Some(*id),
            Slot::Var(_) | Slot::Merged => None,
        };
        QuadPattern {
            subject: constant(&self.slots[0]),
            predicate: constant(&self.slots[1]),
            object: constant(&self.slots[2]),
            graph: match &self.slots[3] {
                Slot::Const(id) => GraphSelector::Exact(*id),
                Slot::Var(_) => GraphSelector::AnyNamed,
                Slot::Merged => GraphSelector::Any,
            },
        }
    }

    /// True if the pattern is in the default graph (statistics, ranges and the
    /// worst-case-optimal join assume it).
    fn in_default_graph(&self) -> bool {
        matches!(self.slots[3], Slot::Const(id) if id == TermId::DEFAULT_GRAPH)
    }

    /// True if the pattern reads the merged default graph: index reads may then return a
    /// statement once per graph that holds it, and the reader drops the copies.
    fn merged(&self) -> bool {
        matches!(self.slots[3], Slot::Merged)
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
                Slot::Const(_) | Slot::Merged => None,
            })
            .collect();
        let before = names.len();
        names.sort_by_key(|v| v.as_str());
        names.dedup();
        names.len() != before
    }

    /// A permutation whose free part starts with `sort_var`, if one exists; any usable one
    /// otherwise. A constant graph takes a graph-first order; a graph variable and the
    /// merged default graph take a graph-last one (their graphs are not a prefix, and in a
    /// graph-last order the copies of a statement are adjacent).
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
            Slot::Var(_) | Slot::Merged => &GRAPH_LAST,
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

/// An `INSERT` template's term with the row's values: template blank nodes fresh per row
/// (`fresh` maps their labels), triple terms filled part by part. `None` if a variable is
/// unbound or a triple term would get a subject that is no IRI or blank node.
fn fill_template(
    t: &TermPattern,
    value: &dyn Fn(&Variable) -> Option<Term>,
    fresh: &mut HashMap<String, nrese_rdf::BlankNode>,
) -> Option<Term> {
    Some(match t {
        TermPattern::NamedNode(n) => n.clone().into(),
        TermPattern::Literal(l) => l.clone().into(),
        TermPattern::BlankNode(b) => fresh
            .entry(b.as_str().to_owned())
            .or_default()
            .clone()
            .into(),
        TermPattern::Variable(v) => value(v)?,
        TermPattern::Triple(t) => {
            let subject = match fill_template(&t.subject, value, fresh)? {
                Term::NamedNode(n) => nrese_rdf::NamedOrBlankNode::from(n),
                Term::BlankNode(b) => b.into(),
                Term::Literal(_) | Term::Triple(_) => return None,
            };
            let predicate = match &t.predicate {
                NamedNodePattern::NamedNode(n) => n.clone(),
                NamedNodePattern::Variable(v) => match value(v)? {
                    Term::NamedNode(n) => n,
                    _ => return None,
                },
            };
            nrese_rdf::Triple::new(subject, predicate, fill_template(&t.object, value, fresh)?)
                .into()
        }
    })
}

/// A `DELETE` template's term with the row's values; `None` if a variable is unbound or a
/// triple term would get a subject that is no IRI or blank node.
fn fill_ground(
    t: &nrese_sparql_syntax::term::GroundTermPattern,
    value: &dyn Fn(&Variable) -> Option<Term>,
) -> Option<Term> {
    Some(match t {
        nrese_sparql_syntax::term::GroundTermPattern::NamedNode(n) => Term::from(n.clone()),
        nrese_sparql_syntax::term::GroundTermPattern::Literal(l) => Term::from(l.clone()),
        nrese_sparql_syntax::term::GroundTermPattern::Variable(v) => value(v)?,
        nrese_sparql_syntax::term::GroundTermPattern::Triple(t) => {
            let subject = match fill_ground(&t.subject, value)? {
                Term::NamedNode(n) => nrese_rdf::NamedOrBlankNode::from(n),
                Term::BlankNode(b) => b.into(),
                Term::Literal(_) | Term::Triple(_) => return None,
            };
            let predicate = match &t.predicate {
                nrese_sparql_syntax::term::NamedNodePattern::NamedNode(n) => n.clone(),
                nrese_sparql_syntax::term::NamedNodePattern::Variable(v) => {
                    match value(&v.clone())? {
                        Term::NamedNode(n) => n,
                        _ => return None,
                    }
                }
            };
            nrese_rdf::Triple::new(subject, predicate, fill_ground(&t.object, value)?).into()
        }
    })
}
