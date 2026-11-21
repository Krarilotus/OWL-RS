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
//! node shape, the focus node. `?failure` true is a validation failure. `sh:resultMessage` is
//! `?message` if it is bound to a literal, else the `sh:message`s of the constraint (of the
//! validator, else of the component), their `{?name}` and `{$name}` blocks filled in
//! ([`Template`]); a result with neither has its shape's messages, if any (§2.1.5).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use nrese_engine::TermId;
use nrese_rdf::{Literal, NamedNode, Term, Variable};
use nrese_sparql::{
    QueryEvaluationError, QueryOptions, QueryResults, QuerySolution, QuerySolutionIter, ReadView,
    Resources,
};
use nrese_sparql_syntax::algebra::{Expression, GraphPattern, PropertyPathExpression};

use crate::graph::Selection;
use crate::model::{Path, SparqlCheck};

/// The variables every SHACL-SPARQL query may have pre-bound.
pub(crate) const THIS: &str = "this";
pub(crate) const CURRENT_SHAPE: &str = "currentShape";
pub(crate) const SHAPES_GRAPH: &str = "shapesGraph";
pub(crate) const VALUE: &str = "value";
/// The variable a SELECT query binds a result's message to (SHACL §5.3.2).
const MESSAGE: &str = "message";
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

/// A solution of a SELECT check, as a result: the value, path and message it names, if any
/// (taken out of the solution, not copied), the bindings its check's message templates name
/// ([`SparqlCheck::template_variables`]), and whether it reports a failure.
pub(crate) struct Found {
    pub(crate) value: Option<Term>,
    pub(crate) path: Option<NamedNode>,
    pub(crate) message: Option<Literal>,
    pub(crate) bindings: Vec<(String, Term)>,
    pub(crate) failure: bool,
}

/// An `sh:message` with `{?name}` or `{$name}` blocks (SHACL §5.3.2): "These {?varName} and
/// {$varName} blocks SHOULD be replaced with suitable string representations of the values
/// of said variables." Its literal's language tag (and direction) or datatype is kept.
#[derive(Debug, Clone)]
pub(crate) struct Template {
    literal: Literal,
    parts: Vec<Part>,
}

#[derive(Debug, Clone)]
enum Part {
    Text(String),
    /// A block: its variable's name, and the block as written (`{?name}` or `{$name}`).
    Variable {
        name: String,
        block: String,
    },
}

impl Template {
    /// The template of `literal`, if it has a block: `{`, `?` or `$`, a SPARQL `VARNAME`
    /// ([`Variable::new`]), `}`. One pass: a block's name is read up to the next `}`, or up to
    /// the next `{`, where the search goes on, so no stretch of the text is read more than
    /// twice.
    pub(crate) fn parse(literal: &Literal) -> Option<Self> {
        let text = literal.value();
        let mut parts = Vec::new();
        let mut plain = 0;
        let mut at = 0;
        while let Some(open) = text[at..].find('{').map(|i| at + i) {
            // `{` and the sigil are one byte each.
            let start = open + 2;
            let sigil = matches!(text.as_bytes().get(open + 1), Some(b'?' | b'$'));
            let end = text[(open + 1).min(text.len())..]
                .find(['{', '}'])
                .map(|i| open + 1 + i);
            let block = match end {
                Some(close) if sigil && close > start && text.as_bytes()[close] == b'}' => {
                    let name = &text[start..close];
                    Variable::new(name).is_ok().then_some((name, close))
                }
                _ => None,
            };
            match block {
                Some((name, close)) => {
                    if plain < open {
                        parts.push(Part::Text(text[plain..open].to_owned()));
                    }
                    at = close + 1;
                    parts.push(Part::Variable {
                        name: name.to_owned(),
                        block: text[open..at].to_owned(),
                    });
                    plain = at;
                }
                // Not a block: the search goes on at the next `{`, if any, or past the `}`.
                None => match end {
                    Some(next) if text.as_bytes()[next] == b'{' => at = next,
                    Some(next) => at = next + 1,
                    None => break,
                },
            }
        }
        if parts.is_empty() {
            return None;
        }
        if plain < text.len() {
            parts.push(Part::Text(text[plain..].to_owned()));
        }
        Some(Self {
            literal: literal.clone(),
            parts,
        })
    }

    /// The names of the variables its blocks name.
    pub(crate) fn variables(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|part| match part {
            Part::Variable { name, .. } => Some(name.as_str()),
            Part::Text(_) => None,
        })
    }

    /// Writes the filled-in text to `out`, with `value` giving the variables' terms; another
    /// term than a literal or an IRI is written as N-Triples writes it, streamed, not made
    /// first.
    fn write<'t>(
        &self,
        value: &impl Fn(&str) -> Option<&'t Term>,
        out: &mut impl fmt::Write,
    ) -> fmt::Result {
        for part in &self.parts {
            match part {
                Part::Text(plain) => out.write_str(plain)?,
                Part::Variable { name, block } => match value(name) {
                    Some(Term::Literal(literal)) => out.write_str(literal.value())?,
                    Some(Term::NamedNode(iri)) => out.write_str(iri.as_str())?,
                    Some(other) => write!(out, "{other}")?,
                    None => out.write_str(block)?,
                },
            }
        }
        Ok(())
    }

    /// The length of the text [`Template::fill`] makes from `value`, counted without making
    /// it or any part of it, so that it can be charged first.
    pub(crate) fn filled_len<'t>(&self, value: &impl Fn(&str) -> Option<&'t Term>) -> usize {
        struct Count(usize);
        impl fmt::Write for Count {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                self.0 += text.len();
                Ok(())
            }
        }
        let mut count = Count(0);
        let _ = self.write(value, &mut count);
        count.0
    }

    /// The message with each block replaced by the text of `value`'s term for its variable: a
    /// literal's lexical form, an IRI's text, any other term as N-Triples writes it. A block
    /// whose variable has no value is left as written. Its text holds exactly
    /// [`Template::filled_len`] bytes, with no spare capacity.
    pub(crate) fn fill<'t>(&self, value: &impl Fn(&str) -> Option<&'t Term>) -> Literal {
        let mut text = String::with_capacity(self.filled_len(value));
        let _ = self.write(value, &mut text);
        let literal = &self.literal;
        match (literal.language(), literal.direction()) {
            (Some(language), Some(direction)) => {
                Literal::new_directional_language_tagged_literal_unchecked(
                    text, language, direction,
                )
            }
            (Some(language), None) => {
                Literal::new_language_tagged_literal_unchecked(text, language)
            }
            (None, _) => Literal::new_typed_literal(text, literal.datatype().into_owned()),
        }
    }
}

/// What a check found for one focus node: per value node (ASK) or per solution (SELECT).
pub(crate) enum Outcome<'a> {
    /// The value nodes an ASK validator answered false for.
    Failed(Vec<TermId>),
    /// A SELECT's solutions, decoded as they are read ([`found`]): none is kept here.
    Solutions(QuerySolutionIter<'a>),
}

/// Why a check has no outcome.
pub(crate) enum Problem {
    /// The validation's resources stopped the query: the cancellation fired, or the envelope
    /// refused a charge. No verdict on the data: the whole validation stops with it.
    Stopped(QueryEvaluationError),
    /// The check couldn't be evaluated: a failure of this constraint.
    Failed(String),
}

impl From<String> for Problem {
    fn from(problem: String) -> Self {
        Self::Failed(problem)
    }
}

impl From<QueryEvaluationError> for Problem {
    fn from(error: QueryEvaluationError) -> Self {
        match error {
            QueryEvaluationError::Cancelled | QueryEvaluationError::MemoryLimit(_) => {
                Self::Stopped(error)
            }
            other => Self::Failed(other.to_string()),
        }
    }
}

/// Runs `check` with one combination of its `parameters`' values ([`SparqlCheck::each_combination`])
/// for `focus` (with `values`, its value nodes, for an ASK validator) over `data`, with the
/// pre-bound variables of SHACL-SPARQL, on the validation's `resources`.
#[expect(
    clippy::too_many_arguments,
    reason = "one check's inputs: the data, the check and its parameters, its focus and values, its shape's terms and the resources"
)]
pub(crate) fn run<'a, V: ReadView>(
    view: &'a V,
    data: Selection,
    check: &SparqlCheck,
    parameters: &[(Variable, TermId)],
    focus: TermId,
    values: &[TermId],
    shape: TermId,
    shapes_graph: Option<TermId>,
    resources: &Resources,
) -> Result<Outcome<'a>, Problem> {
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
    for (name, value) in parameters {
        bound.insert(name.clone(), decode(*value)?);
    }
    // The statements Core reads, shapes graphs left out ([`Selection::query_dataset`]), on
    // the validation's cancellation and envelope.
    let mut options = QueryOptions {
        resources: resources.clone(),
        ..QueryOptions::default()
    };
    data.query_dataset(view, &mut options)?;
    let evaluate = |bound: HashMap<Variable, Term>| {
        let options = QueryOptions {
            pre_bound: Some(Arc::new(bound)),
            ..options.clone()
        };
        nrese_sparql::evaluate_query(view, &check.query, &options).map_err(Problem::from)
    };
    if check.ask {
        let mut failed = Vec::new();
        for &value in values {
            let mut bound = bound.clone();
            bound.insert(variable(VALUE), decode(value)?);
            match evaluate(bound)? {
                QueryResults::Boolean(true) => {}
                QueryResults::Boolean(false) => failed.push(value),
                _ => {
                    return Err(Problem::Failed(
                        "an ASK validator isn't an ASK query".into(),
                    ));
                }
            }
        }
        return Ok(Outcome::Failed(failed));
    }
    let QueryResults::Solutions(solutions) = evaluate(bound)? else {
        return Err(Problem::Failed(
            "a SELECT constraint isn't a SELECT query".into(),
        ));
    };
    Ok(Outcome::Solutions(solutions))
}

/// A SELECT check's solution, as a result ([`Outcome::Solutions`] yields them as they are
/// read: each is decoded, used and dropped before the next). Its terms are moved out of it,
/// not copied, but for those `check`'s message templates name, which are copied first; a row
/// that reports `?failure` has its terms dropped with it.
pub(crate) fn found(solution: QuerySolution, check: &SparqlCheck) -> Found {
    let at = |name: &str| {
        solution
            .variables()
            .iter()
            .position(|variable| variable.as_str() == name)
    };
    let (failure_at, path_at, value_at, message_at) =
        (at("failure"), at("path"), at(VALUE), at(MESSAGE));
    let mut named: Vec<(String, usize)> = Vec::new();
    for name in check.template_variables() {
        if let Some(at) = at(name)
            && !named.iter().any(|(seen, _)| seen == name)
        {
            named.push((name.to_owned(), at));
        }
    }
    let mut values = solution.into_values();
    let failure = failure_at.and_then(|at| values.get(at)?.as_ref());
    if matches!(failure, Some(Term::Literal(l)) if l.value() == "true") {
        return Found {
            value: None,
            path: None,
            message: None,
            bindings: Vec::new(),
            failure: true,
        };
    }
    let bindings = named
        .into_iter()
        .filter_map(|(name, at)| Some((name, values.get(at)?.clone()?)))
        .collect();
    let mut take = |at: Option<usize>| at.and_then(|at| values.get_mut(at)?.take());
    let path = match take(path_at) {
        Some(Term::NamedNode(path)) => Some(path),
        _ => None,
    };
    // Only a literal bound to `?message` is carried; another term isn't, and the declared
    // messages apply. §5.3.2 gives "The binding for the variable message" with no condition,
    // and §3.6.2.7 doesn't limit a result's messages to literals: it calls them "additional
    // textual details to humans", of which "there should not be two values with the same
    // language tag". This is ours: a report's messages are literals
    // (`ValidationResult::messages`).
    let message = match take(message_at) {
        Some(Term::Literal(message)) => Some(message),
        _ => None,
    };
    Found {
        value: take(value_at),
        path,
        message,
        bindings,
        failure: false,
    }
}
