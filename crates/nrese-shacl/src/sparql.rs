//! SHACL-SPARQL (SHACL §5, §6): constraints and constraint components written in SPARQL.
//!
//! - **`sh:sparql`** on a shape: a SELECT query, run once per focus node with `$this` the
//!   focus node; each solution is a result (`sh:SPARQLConstraintComponent`).
//! - **SPARQL-based constraint components**: a node with `sh:parameter`s and a validator.
//!   A shape with values for all of a component's mandatory parameters is constrained by
//!   it: the parameters are pre-bound as variables named by their predicates' local
//!   names. A SELECT validator works as `sh:sparql`; an ASK validator runs per value node
//!   with `$value`, and a value for which it answers false is a result. Property shapes
//!   use `sh:propertyValidator`, node shapes `sh:nodeValidator`, both fall back to
//!   `sh:validator` (which must be an ASK).
//!
//! **Pre-binding** puts the values in for the variables, throughout the query
//! ([`nrese_sparql::QueryOptions::pre_bound`]): `$this`, `$currentShape`, `$shapesGraph`
//! (if the shapes graph is a named graph), `$value` and the parameters. `$PATH` as the
//! predicate of a triple pattern in a property shape's query is read as its path while the
//! query is parsed, as if the path were written there; any other use is ill-formed. The
//! restrictions of §5.6.2 make a query ill-formed: `MINUS`, `VALUES`, `SERVICE`,
//! binding a pre-bound variable (`AS ?this`), and a subquery that doesn't project every
//! pre-bound variable (`$shapesGraph` and `$currentShape` aside).
//!
//! **Results** map solutions as §5.3.2 says: `sh:focusNode` the focus node, `sh:resultPath`
//! `?path` if it is an IRI else the shape's path, `sh:value` `?value` if bound else, for a
//! node shape, the focus node. `?failure` true is a validation failure.

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::stopped;
use nrese_engine::TermId;
use nrese_exec::{Budget, ChargedVec, Reservation, Ticker, map_bytes};
use nrese_rdf::{GraphName, NamedNode, Term, Variable};
use nrese_sparql::{QueryEvaluationError, QueryOptions, QueryResults, ReadView, Resources};
use nrese_sparql_syntax::algebra::{Expression, GraphPattern, PropertyPathExpression};

use crate::graph::Selection;
use crate::model::{Path, SparqlProgram};

/// The variables every SHACL-SPARQL query may have pre-bound.
pub(crate) const THIS: &str = "this";
pub(crate) const CURRENT_SHAPE: &str = "currentShape";
pub(crate) const SHAPES_GRAPH: &str = "shapesGraph";
pub(crate) const VALUE: &str = "value";
/// The placeholder for a property shape's path (SHACL §5.2): legal only as the predicate of
/// a triple pattern, where it is read as the path.
pub(crate) const PATH: &str = "PATH";

/// Why `pattern` can't be pre-bound with `pre_bound` (§5.6.2), if it can't: `MINUS`,
/// `VALUES`, `SERVICE`, a pre-bound variable bound by `AS`, or a subquery that leaves out
/// one of `pre_bound` other than `$shapesGraph` and `$currentShape`.
pub(crate) fn prebinding_problem(pattern: &GraphPattern, pre_bound: &[String]) -> Option<String> {
    fn walk(pattern: &GraphPattern, pre_bound: &[String], top: bool) -> Option<String> {
        let inner = |p: &GraphPattern| walk(p, pre_bound, top);
        let below = |p: &GraphPattern| walk(p, pre_bound, false);
        match pattern {
            GraphPattern::Minus { .. } => Some("uses MINUS".into()),
            GraphPattern::Values { .. } => Some("uses VALUES".into()),
            GraphPattern::Service { .. } => Some("uses SERVICE".into()),
            GraphPattern::Extend { inner: p, bindings } => {
                for (variable, expression) in bindings {
                    if pre_bound.iter().any(|v| v == variable.as_str()) {
                        return Some(format!(
                            "binds the pre-bound variable ?{}",
                            variable.as_str()
                        ));
                    }
                    if let Some(problem) = expression_problem(expression, pre_bound) {
                        return Some(problem);
                    }
                }
                inner(p)
            }
            GraphPattern::Project {
                inner: p,
                variables,
            } => {
                if !top {
                    let missing = pre_bound.iter().find(|v| {
                        v.as_str() != SHAPES_GRAPH
                            && v.as_str() != CURRENT_SHAPE
                            && !variables.iter().any(|x| x.as_str() == v.as_str())
                    });
                    if let Some(missing) = missing {
                        return Some(format!("has a subquery that doesn't project ?{missing}"));
                    }
                }
                below(p)
            }
            GraphPattern::Filter { expr, inner: p } => {
                expression_problem(expr, pre_bound).or_else(|| inner(p))
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => expression
                .as_ref()
                .and_then(|e| expression_problem(e, pre_bound))
                .or_else(|| inner(left))
                .or_else(|| inner(right)),
            GraphPattern::Join { inputs } | GraphPattern::Union { inputs } => {
                inputs.iter().find_map(&inner)
            }
            GraphPattern::Graph { inner: p, .. }
            | GraphPattern::Distinct { inner: p }
            | GraphPattern::Reduced { inner: p }
            | GraphPattern::Slice { inner: p, .. }
            | GraphPattern::OrderBy { inner: p, .. }
            | GraphPattern::Group { inner: p, .. } => inner(p),
            _ => None,
        }
    }
    walk(pattern, pre_bound, true)
}

/// The problem of an `EXISTS` inside `expression`, if any.
fn expression_problem(expression: &Expression, pre_bound: &[String]) -> Option<String> {
    match expression {
        Expression::Exists(pattern) => prebinding_problem(pattern, pre_bound),
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
            expression_problem(a, pre_bound).or_else(|| expression_problem(b, pre_bound))
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            expression_problem(a, pre_bound)
        }
        Expression::If(a, b, c) => expression_problem(a, pre_bound)
            .or_else(|| expression_problem(b, pre_bound))
            .or_else(|| expression_problem(c, pre_bound)),
        Expression::In(a, list) => expression_problem(a, pre_bound)
            .or_else(|| list.iter().find_map(|x| expression_problem(x, pre_bound))),
        Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
            list.iter().find_map(|x| expression_problem(x, pre_bound))
        }
        _ => None,
    }
}

/// `path` as a SPARQL property path, with `iri` naming each predicate.
pub(crate) fn path_expression(
    path: &Path,
    iri: &dyn Fn(TermId) -> Option<NamedNode>,
) -> Option<PropertyPathExpression> {
    let parts = |parts: &[Path]| -> Option<Vec<PropertyPathExpression>> {
        parts.iter().map(|p| path_expression(p, iri)).collect()
    };
    let inner = |inner: &Path| path_expression(inner, iri).map(Box::new);
    Some(match path {
        Path::Predicate(predicate) => PropertyPathExpression::NamedNode(iri(*predicate)?),
        Path::Inverse(path) => PropertyPathExpression::Reverse(inner(path)?),
        Path::Sequence(steps) => PropertyPathExpression::sequence(parts(steps)?),
        Path::Alternative(paths) => PropertyPathExpression::alternative(parts(paths)?),
        Path::ZeroOrMore(path) => PropertyPathExpression::ZeroOrMore(inner(path)?),
        Path::OneOrMore(path) => PropertyPathExpression::OneOrMore(inner(path)?),
        Path::ZeroOrOne(path) => PropertyPathExpression::ZeroOrOne(inner(path)?),
    })
}

/// A solution of a SELECT check, as a result: the value and path it names, if any, and
/// whether it reports a failure.
pub(crate) struct Found {
    pub(crate) value: Option<Term>,
    pub(crate) path: Option<NamedNode>,
    pub(crate) failure: bool,
}

/// What a check found for one focus node: per value node (ASK) or per solution (SELECT).
pub(crate) enum Outcome<'b> {
    Failed(ChargedVec<TermId, &'b Budget>),
    Solutions {
        found: ChargedVec<Found, &'b Budget>,
        _held: Reservation<&'b Budget>,
    },
}

/// The query table/computed terms keep CT's charge. Only copied SHACL bindings and
/// Found payloads are additionally admitted here; evaluator errors remain typed.
#[expect(
    clippy::too_many_arguments,
    reason = "Existing borrowed check inputs plus the same request resources, budget and task ticker."
)]
pub(crate) fn run<'b, V: ReadView>(
    view: &V,
    data: Selection,
    check: &SparqlProgram,
    parameters: &[TermId],
    focus: TermId,
    values: &[TermId],
    shape: TermId,
    shapes_graph: Option<TermId>,
    resources: &Resources,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<Outcome<'b>, QueryEvaluationError> {
    ticker.check().map_err(stopped)?;
    // Exact upper bound on the existing pre-bound map's entries, not on query rows.
    let count = 3usize
        .checked_add(parameters.len())
        .and_then(|n| n.checked_add(usize::from(check.ask)))
        .ok_or_else(|| budget.refusal(usize::MAX))?;
    let mut bound_held = Reservation::new(budget);
    bound_held.charge(map_bytes::<(Variable, Term)>(count))?;
    let mut bound: HashMap<Variable, Term> = HashMap::with_capacity(count);
    let variable = |name: &str| Variable::new_unchecked(name);
    let decode = |id, held: &mut Reservation<&Budget>| -> Result<Term, QueryEvaluationError> {
        view.decode_within(id, held)?.ok_or_else(|| {
            QueryEvaluationError::Unexpected(format!("term #{} isn't known", id.raw()))
        })
    };
    bound.insert(variable(THIS), decode(focus, &mut bound_held)?);
    bound.insert(variable(CURRENT_SHAPE), decode(shape, &mut bound_held)?);
    if let Some(graph) = shapes_graph {
        bound.insert(variable(SHAPES_GRAPH), decode(graph, &mut bound_held)?);
    }
    for (name, value) in check.parameters.iter().zip(parameters) {
        ticker.tick(1).map_err(stopped)?;
        bound.insert(name.clone(), decode(*value, &mut bound_held)?);
    }
    let mut dataset_held = Reservation::new(budget);
    let mut options = QueryOptions {
        resources: resources.clone(),
        ..Default::default()
    };
    data.query_dataset(view, &mut options, &mut dataset_held, ticker)?;
    let evaluate = |bound: HashMap<Variable, Term>, query_held: &mut Reservation<&Budget>| {
        if let Some(dataset) = &options.dataset {
            for names in [
                dataset.default_graph_graphs(),
                dataset.excluded_default_graphs(),
            ]
            .into_iter()
            .flatten()
            {
                query_held.charge(std::mem::size_of_val(names))?;
                for name in names {
                    query_held.charge(match name {
                        GraphName::NamedNode(n) => n.heap_bytes(),
                        GraphName::BlankNode(n) => n.heap_bytes(),
                        GraphName::DefaultGraph => 0,
                    })?;
                }
            }
        }
        let options = QueryOptions {
            pre_bound: Some(Arc::new(bound)),
            ..options.clone()
        };
        nrese_sparql::evaluate_query(view, &check.query, &options)
    };
    if check.ask {
        let mut failed = ChargedVec::new(budget);
        for &value in values {
            ticker.tick(1).map_err(stopped)?;
            // Existing ASK evaluation owns a pre-bound map per query. Its data-term
            // clones are admitted beside the base map; no per-result Arc is introduced.
            let mut query_held = Reservation::new(budget);
            query_held.charge(map_bytes::<(Variable, Term)>(count))?;
            for term in bound.values() {
                query_held.charge(term.heap_bytes())?;
            }
            let mut chosen = HashMap::with_capacity(count);
            chosen.extend(bound.iter().map(|(k, v)| (k.clone(), v.clone())));
            chosen.insert(variable(VALUE), decode(value, &mut query_held)?);
            ticker.check().map_err(stopped)?;
            let result = evaluate(chosen, &mut query_held)?;
            ticker.check().map_err(stopped)?;
            match result {
                QueryResults::Boolean(true) => {}
                QueryResults::Boolean(false) => failed.push(value)?,
                _ => {
                    return Err(QueryEvaluationError::Unexpected(
                        "an ASK validator isn't an ASK query".into(),
                    ));
                }
            }
        }
        return Ok(Outcome::Failed(failed));
    }
    ticker.check().map_err(stopped)?;
    let mut query_held = Reservation::new(budget);
    let evaluated = evaluate(bound, &mut query_held)?;
    ticker.check().map_err(stopped)?;
    let QueryResults::Solutions(solutions) = evaluated else {
        return Err(QueryEvaluationError::Unexpected(
            "a SELECT constraint isn't a SELECT query".into(),
        ));
    };
    // The bound map may be retained by the iterator: keep its lease until it drops.
    let mut held = Reservation::new(budget);
    let mut found = ChargedVec::new(budget);
    for solution in solutions {
        ticker.tick(1).map_err(stopped)?;
        let solution = solution?;
        let failure =
            matches!(solution.get("failure"), Some(Term::Literal(l)) if l.value() == "true");
        // Charge only the data actually copied out of the CT-owned solution.
        if let Some(value) = solution.get(VALUE) {
            held.charge(value.heap_bytes())?;
        }
        let path = match solution.get("path") {
            Some(Term::NamedNode(path)) => {
                held.charge(path.heap_bytes())?;
                Some(path.clone())
            }
            _ => None,
        };
        found.push(Found {
            value: solution.get(VALUE).cloned(),
            path,
            failure,
        })?;
    }
    ticker.finish().map_err(stopped)?;
    Ok(Outcome::Solutions { found, _held: held })
}
