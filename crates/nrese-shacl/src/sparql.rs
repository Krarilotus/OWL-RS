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
//! (if the shapes graph is a named graph), `$value` and the parameters. `$PATH` in a
//! property shape's query is replaced by its path in SPARQL syntax before parsing. The
//! restrictions of §5.6.2 make a query ill-formed: `MINUS`, `VALUES`, `SERVICE`,
//! binding a pre-bound variable (`AS ?this`), and a subquery that doesn't project every
//! pre-bound variable (`$shapesGraph` and `$currentShape` aside).
//!
//! **Results** map solutions as §5.3.2 says: `sh:focusNode` the focus node, `sh:resultPath`
//! `?path` if it is an IRI else the shape's path, `sh:value` `?value` if bound else, for a
//! node shape, the focus node. `?failure` true is a validation failure.

use std::collections::HashMap;
use std::sync::Arc;

use nrese_engine::{GraphSelector, TermId};
use nrese_rdf::{GraphName, NamedNode, Term, Variable};
use nrese_sparql::{QueryDatasetSpecification, QueryOptions, QueryResults, ReadView};
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};

use crate::graph::Selection;
use crate::model::{Path, SparqlCheck};

/// The variables every SHACL-SPARQL query may have pre-bound.
pub(crate) const THIS: &str = "this";
pub(crate) const CURRENT_SHAPE: &str = "currentShape";
pub(crate) const SHAPES_GRAPH: &str = "shapesGraph";
pub(crate) const VALUE: &str = "value";

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
            GraphPattern::Extend {
                inner: p,
                variable,
                expression,
            } => {
                if pre_bound.iter().any(|v| v == variable.as_str()) {
                    return Some(format!(
                        "binds the pre-bound variable ?{}",
                        variable.as_str()
                    ));
                }
                expression_problem(expression, pre_bound).or_else(|| inner(p))
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
            GraphPattern::Join { left, right } | GraphPattern::Union { left, right } => {
                inner(left).or_else(|| inner(right))
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

/// `path` in SPARQL property path syntax, with `iri` naming each predicate.
pub(crate) fn path_text(path: &Path, iri: &dyn Fn(TermId) -> Option<String>) -> Option<String> {
    let joined = |parts: &[Path], separator: &str| -> Option<String> {
        let parts: Option<Vec<String>> = parts.iter().map(|p| path_text(p, iri)).collect();
        Some(format!("({})", parts?.join(separator)))
    };
    Some(match path {
        Path::Predicate(predicate) => format!("<{}>", iri(*predicate)?),
        Path::Inverse(inner) => format!("^({})", path_text(inner, iri)?),
        Path::Sequence(parts) => joined(parts, "/")?,
        Path::Alternative(parts) => joined(parts, "|")?,
        Path::ZeroOrMore(inner) => format!("({})*", path_text(inner, iri)?),
        Path::OneOrMore(inner) => format!("({})+", path_text(inner, iri)?),
        Path::ZeroOrOne(inner) => format!("({})?", path_text(inner, iri)?),
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
pub(crate) enum Outcome {
    /// The value nodes an ASK validator answered false for.
    Failed(Vec<TermId>),
    Solutions(Vec<Found>),
}

/// Runs `check` for `focus` (with `values`, its value nodes, for an ASK validator) over
/// `data`, with the pre-bound variables of SHACL-SPARQL.
pub(crate) fn run<V: ReadView>(
    view: &V,
    data: Selection,
    check: &SparqlCheck,
    focus: TermId,
    values: &[TermId],
    shape: TermId,
    shapes_graph: Option<TermId>,
) -> Result<Outcome, String> {
    let decode = |id: TermId| {
        view.decode(id)
            .ok_or_else(|| format!("term #{} isn't known", id.raw()))
    };
    let mut bound: HashMap<Variable, Term> = HashMap::new();
    let variable = |name: &str| Variable::new_unchecked(name);
    bound.insert(variable(THIS), decode(focus)?);
    bound.insert(variable(CURRENT_SHAPE), decode(shape)?);
    if let Some(graph) = shapes_graph {
        bound.insert(variable(SHAPES_GRAPH), decode(graph)?);
    }
    for (name, value) in &check.parameters {
        bound.insert(name.clone(), decode(*value)?);
    }
    let mut options = QueryOptions {
        read_model: data.model,
        ..QueryOptions::default()
    };
    match data.graphs {
        GraphSelector::Exact(graph) if !graph.is_default_graph() => {
            let mut dataset = QueryDatasetSpecification::new();
            let Term::NamedNode(name) = decode(graph)? else {
                return Err("the data graph isn't an IRI".into());
            };
            dataset.set_default_graph(vec![GraphName::NamedNode(name)]);
            options.dataset = Some(dataset);
        }
        GraphSelector::Exact(_) => {}
        // Every graph: their merge (a shapes graph among them is read too).
        _ => options.union_default_graph = true,
    }
    let evaluate = |bound: HashMap<Variable, Term>| {
        let options = QueryOptions {
            pre_bound: Some(Arc::new(bound)),
            ..options.clone()
        };
        nrese_sparql::evaluate_query(view, &check.query, &options).map_err(|e| e.to_string())
    };
    if check.ask {
        let mut failed = Vec::new();
        for &value in values {
            let mut bound = bound.clone();
            bound.insert(variable(VALUE), decode(value)?);
            match evaluate(bound)? {
                QueryResults::Boolean(true) => {}
                QueryResults::Boolean(false) => failed.push(value),
                _ => return Err("an ASK validator isn't an ASK query".into()),
            }
        }
        return Ok(Outcome::Failed(failed));
    }
    let QueryResults::Solutions(solutions) = evaluate(bound)? else {
        return Err("a SELECT constraint isn't a SELECT query".into());
    };
    let mut found = Vec::new();
    for solution in solutions {
        let solution = solution.map_err(|e| e.to_string())?;
        let failure = matches!(
            solution.get("failure"),
            Some(Term::Literal(l)) if l.value() == "true"
        );
        let path = match solution.get("path") {
            Some(Term::NamedNode(path)) => Some(path.clone()),
            _ => None,
        };
        found.push(Found {
            value: solution.get(VALUE).cloned(),
            path,
            failure,
        });
    }
    Ok(Outcome::Solutions(found))
}
