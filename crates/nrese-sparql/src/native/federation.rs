//! `SERVICE` ([`crate::service`]): the block goes to the endpoint as `SELECT *` under the
//! query's `BASE`, with the values bound so far as `VALUES` when it is joined to them.
//! `SERVICE ?endpoint` calls each endpoint the solutions it is joined to bind, with their
//! rows. An answer's blank nodes are its own: they join to nothing of another answer or
//! the store.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern};

use nrese_exec::{IdTable, Reservation, UNDEF};
use nrese_rdf::{BlankNode, NamedOrBlankNode, Term, Triple, Variable};

use crate::results::{CancellationToken, QueryEvaluationError};

use super::{Context, NativeError, NativeResult, Solutions, bound_variables};
use crate::service::{BIND_CHUNK, BIND_LIMIT, ServiceCall};

/// How a `SERVICE` joins what the query has bound before it.
pub(super) enum Attach<'e> {
    Join,
    /// `OPTIONAL`, with its filter.
    Optional(Option<&'e Expression>),
}

/// The rows of a `SERVICE SILENT ?e` with no endpoint to call: those whose value in
/// `column` is one of `values` (no IRI, or unbound).
struct Uncalled {
    column: usize,
    values: HashSet<u64>,
}

impl Context<'_> {
    /// `SERVICE name { inner }` on its own; with `silent`, a failing endpoint gives one
    /// solution without bindings.
    pub(super) fn service(
        &self,
        name: &NamedNodePattern,
        inner: &GraphPattern,
        silent: bool,
    ) -> NativeResult<Solutions> {
        Ok(self.service_found(name, inner, silent, None)?.0)
    }

    /// `SERVICE name { inner }` joined to `bound` as `attach` says. Under SILENT, the rows
    /// whose endpoint variable holds no IRI are joined to one empty solution, as a failed
    /// call's are: kept once, and joined to no endpoint's answer.
    pub(super) fn service_attached(
        &self,
        name: &NamedNodePattern,
        inner: &GraphPattern,
        silent: bool,
        bound: Solutions,
        attach: Attach<'_>,
    ) -> NativeResult<Solutions> {
        let (found, uncalled) = self.service_found(name, inner, silent, Some(&bound))?;
        let (bound, kept) = match uncalled {
            Some(uncalled) => {
                let (called, kept) = self.split(bound, &uncalled)?;
                (called, Some(kept))
            }
            None => (bound, None),
        };
        let joined = match attach {
            Attach::Join => self.join(bound, found)?,
            Attach::Optional(expression) => self.left_join(bound, found, expression)?,
        };
        match kept {
            Some(kept) => self.union(joined, kept),
            None => Ok(joined),
        }
    }

    /// `bound` split into the rows `uncalled` doesn't name and those it does, these charged
    /// (with the mask that splits them) before they are made.
    fn split(
        &self,
        mut bound: Solutions,
        uncalled: &Uncalled,
    ) -> NativeResult<(Solutions, Solutions)> {
        let (len, width) = (bound.table.len(), bound.table.width());
        let is_kept = |row| {
            uncalled
                .values
                .contains(&bound.table.get(row, uncalled.column))
        };
        let rows = (0..len).filter(|&row| is_kept(row)).count();
        let mut held = Reservation::new(Arc::clone(&self.budget));
        held.charge(len.saturating_add(rows.saturating_mul(width).saturating_mul(8)))
            .map_err(QueryEvaluationError::MemoryLimit)?;
        let called: Vec<bool> = (0..len).map(|row| !is_kept(row)).collect();
        let mut kept = IdTable::with_capacity(width, rows);
        for row in (0..len).filter(|&row| !called[row]) {
            kept.push_row(&bound.table.row(row));
        }
        bound.table.retain_mask(&called);
        drop(called);
        // From here the table's charge, released as it is consumed.
        held.resize(kept.memory_bytes())
            .map_err(QueryEvaluationError::MemoryLimit)?;
        held.transfer();
        let kept = Solutions {
            vars: bound.vars.clone(),
            table: kept,
            ordered: false,
        };
        Ok((bound, kept))
    }

    /// The endpoints' answer to `SERVICE name { inner }`, joined to `bound` if given, and
    /// the rows of `bound` that SILENT keeps without a call, if any.
    fn service_found(
        &self,
        name: &NamedNodePattern,
        inner: &GraphPattern,
        silent: bool,
        bound: Option<&Solutions>,
    ) -> NativeResult<(Solutions, Option<Uncalled>)> {
        if super::vectors::is_search(name) {
            return Ok((self.vector_service(inner, bound)?, None));
        }
        let variable = match name {
            NamedNodePattern::NamedNode(endpoint) => {
                let found = self.call_silently(endpoint.as_str(), inner, bound, silent)?;
                return Ok((found, None));
            }
            NamedNodePattern::Variable(v) => v,
        };
        // Joined to no solutions: none names an endpoint, and the join has none.
        if bound.is_some_and(|b| b.table.is_empty()) {
            return Ok((Solutions::unit(), None));
        }
        // The endpoints are the values the joined solutions have for the variable.
        let unbound = || -> NativeResult<Solutions> {
            if silent {
                Ok(Solutions::unit())
            } else {
                Err(QueryEvaluationError::Service(
                    format!("SERVICE {variable}: the variable has no value").into(),
                )
                .into())
            }
        };
        let Some((bound, column)) = bound.and_then(|b| Some((b, b.column(variable)?))) else {
            return Ok((unbound()?, None));
        };
        let mut rows_of: HashMap<u64, Vec<usize>> = HashMap::new();
        for row in 0..bound.table.len() {
            rows_of
                .entry(bound.table.get(row, column))
                .or_default()
                .push(row);
        }
        let mut endpoints: Vec<(u64, Vec<usize>)> = rows_of.into_iter().collect();
        endpoints.sort_unstable_by_key(|(id, _)| *id);
        let mut all: Option<Solutions> = None;
        let mut uncalled = HashSet::new();
        for (id, rows) in endpoints {
            let endpoint = match (id != UNDEF).then(|| self.term(id)).flatten() {
                Some(Term::NamedNode(n)) => n,
                _ if silent => {
                    uncalled.insert(id);
                    continue;
                }
                _ => {
                    return Err(QueryEvaluationError::Service(
                        format!("SERVICE {variable}: an endpoint must be an IRI").into(),
                    )
                    .into());
                }
            };
            let mut subset = IdTable::new(bound.table.width());
            for &row in &rows {
                subset.push_row(&bound.table.row(row));
            }
            let subset = Solutions {
                vars: bound.vars.clone(),
                table: subset,
                ordered: false,
            };
            let found = self.call_silently(endpoint.as_str(), inner, Some(&subset), silent)?;
            // The endpoint's rows carry the endpoint, so they join to the rows that named it.
            let found = if found.vars.contains(variable) {
                found
            } else {
                let mut vars = found.vars.clone();
                vars.push(variable.clone());
                let mut table = IdTable::new(vars.len());
                let mut line = vec![id; vars.len()];
                for row in 0..found.table.len() {
                    line[..found.vars.len()].copy_from_slice(&found.table.row(row));
                    table.push_row(&line);
                }
                Solutions {
                    vars,
                    table,
                    ordered: false,
                }
            };
            all = Some(match all {
                Some(all) => self.union(all, found)?,
                None => found,
            });
        }
        let uncalled = (!uncalled.is_empty()).then_some(Uncalled {
            column,
            values: uncalled,
        });
        let found = match all {
            Some(all) => all,
            None => unbound()?,
        };
        Ok((found, uncalled))
    }

    /// A call's error: one that failed once the query was cancelled was stopped by the
    /// cancellation, whatever the client says of it.
    fn stopped(&self, error: QueryEvaluationError) -> QueryEvaluationError {
        let cancelled = self
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        match error {
            QueryEvaluationError::Service(_) if cancelled => QueryEvaluationError::Cancelled,
            error => error,
        }
    }

    /// One endpoint's answer; with `silent`, the endpoint's failure gives one solution
    /// without bindings. The query's own errors, its cancellation and a refusal of memory,
    /// are never ignored.
    fn call_silently(
        &self,
        endpoint: &str,
        inner: &GraphPattern,
        bound: Option<&Solutions>,
        silent: bool,
    ) -> NativeResult<Solutions> {
        match self.call_service(endpoint, inner, bound) {
            Err(NativeError::Evaluation(QueryEvaluationError::Service(_))) if silent => {
                Ok(Solutions::unit())
            }
            other => other,
        }
    }

    fn call_service(
        &self,
        endpoint: &str,
        inner: &GraphPattern,
        bound: Option<&Solutions>,
    ) -> NativeResult<Solutions> {
        if self.service_denied {
            return Err(QueryEvaluationError::Service(
                format!("SERVICE <{endpoint}>: the requester may not call other endpoints").into(),
            )
            .into());
        }
        let Some(services) = &self.services else {
            return Err(QueryEvaluationError::Service(
                format!("SERVICE <{endpoint}>: federation is not enabled on this server").into(),
            )
            .into());
        };
        // Under canonical answers a bound value stands for its class, and the endpoint names
        // each identity as stored ([`super::canonical`]).
        let (chunks, _held) = match (bound, &self.canonical) {
            (Some(bound), None) => (bind_values(bound, inner, self), None),
            (Some(bound), Some(names)) => match self.bind_classes(bound, inner, names)? {
                Some((chunks, held)) => (Some(chunks), Some(held)),
                None => (None, None),
            },
            (None, _) => (None, None),
        };
        let patterns = chunks.unwrap_or_else(|| vec![inner.clone()]);
        let mut all: Option<Solutions> = None;
        for pattern in patterns {
            self.check()?;
            // Under the query's BASE: an `IRI()` the endpoint evaluates resolves as here.
            let query = Query::Select {
                dataset: None,
                pattern,
                base_iri: self.evaluator.base().cloned(),
            }
            .to_string();
            // Declared before the answer: released once the answer is freed.
            let mut held = Reservation::new(Arc::clone(&self.budget));
            let call = ServiceCall {
                cancellation: self.cancellation.as_ref(),
                budget: &self.budget,
                answer: &mut held,
            };
            let results = services
                .0
                .select(endpoint, &query, call)
                .map_err(|error| self.stopped(error))?;
            // The endpoint's terms, its blank nodes its own, as canonical answers name them.
            let scope = Scope::new();
            let table = match &self.canonical {
                None => import(&results, |term| self.id(&scope.of(term))),
                Some(names) => import(&results, |term| {
                    let term = scope.of(term);
                    names
                        .id(&term)
                        .unwrap_or_else(|| self.id(&names.term(term.into_owned())))
                }),
            };
            let solutions = self.produced(Solutions {
                vars: results.variables,
                table,
                ordered: false,
            })?;
            all = Some(match all {
                Some(all) => self.union(all, solutions)?,
                None => solutions,
            });
        }
        Ok(all.unwrap_or_else(Solutions::unit))
    }
}

/// The endpoint's rows as an id table, each term's id by `id`.
fn import(results: &crate::ServiceResults, id: impl Fn(&Term) -> u64) -> IdTable {
    let mut table = IdTable::new(results.variables.len());
    let mut row = vec![UNDEF; results.variables.len()];
    for values in &results.rows {
        for (slot, value) in row.iter_mut().zip(values) {
            *slot = value.as_ref().map_or(UNDEF, &id);
        }
        table.push_row(&row);
    }
    table
}

/// The blank nodes of one endpoint answer, named apart from every other answer's and the
/// store's: a results document's labels are its own (SPARQL 1.1 Query Results JSON §3.2.2,
/// XML §2.3.1). `_:l` becomes `_:r<s>xl`, `s` random per answer (128 bits), inside triple
/// terms too; the same label within the answer stays one node.
struct Scope(u128);

impl Scope {
    fn new() -> Self {
        Self(rand::random())
    }

    fn of<'t>(&self, term: &'t Term) -> Cow<'t, Term> {
        match term {
            Term::BlankNode(b) => Cow::Owned(self.blank(b).into()),
            Term::Triple(t) => Cow::Owned(Term::Triple(Box::new(self.triple(t)))),
            Term::NamedNode(_) | Term::Literal(_) => Cow::Borrowed(term),
        }
    }

    fn blank(&self, b: &BlankNode) -> BlankNode {
        BlankNode::new_unchecked(format!("r{:x}x{}", self.0, b.as_str()))
    }

    fn triple(&self, t: &Triple) -> Triple {
        Triple {
            subject: match &t.subject {
                NamedOrBlankNode::BlankNode(b) => self.blank(b).into(),
                named @ NamedOrBlankNode::NamedNode(_) => named.clone(),
            },
            predicate: t.predicate.clone(),
            object: self.of(&t.object).into_owned(),
        }
    }
}

impl Context<'_> {
    /// [`bind_values`] under canonical answers: every combination of the identities of
    /// the classes a row's values stand for. Counted before anything is made: past
    /// [`BIND_LIMIT`] rows the block goes without `VALUES` (`None`), as a bind join would
    /// past it. The rows are reserved before they are made, the reservation held with the
    /// patterns made of them, and the query's cancellation is checked as they are.
    fn bind_classes(
        &self,
        bound: &Solutions,
        inner: &GraphPattern,
        names: &super::canonical::Names,
    ) -> NativeResult<Option<(Vec<GraphPattern>, nrese_exec::Reservation)>> {
        let mut in_scope = Vec::new();
        bound_variables(inner, &mut in_scope);
        let shared: Vec<(Variable, usize)> = in_scope
            .into_iter()
            .filter_map(|v| bound.column(&v).map(|c| (v, c)))
            .collect();
        if shared.is_empty() {
            return Ok(None);
        }
        let mut keys: Vec<Vec<u64>> = Vec::new();
        let mut seen: HashSet<Vec<u64>> = HashSet::new();
        let mut rows = 0usize;
        for r in 0..bound.table.len() {
            let key: Vec<u64> = shared.iter().map(|&(_, c)| bound.table.get(r, c)).collect();
            if seen.contains(&key) {
                continue;
            }
            let combinations = key
                .iter()
                .try_fold(1usize, |n, id| n.checked_mul(names.members(id).len()));
            match combinations.and_then(|n| rows.checked_add(n)) {
                Some(total) if total <= BIND_LIMIT => rows = total,
                _ => return Ok(None),
            }
            seen.insert(key.clone());
            keys.push(key);
        }
        // A row's terms, each a term of its own (an estimate per term, as the endpoint's
        // query text is made from them).
        let width = shared.len();
        let variables: Vec<Variable> = shared.iter().map(|(v, _)| v.clone()).collect();
        let mut held = nrese_exec::Reservation::new(std::sync::Arc::clone(&self.budget));
        held.charge(rows.saturating_mul(width) * (std::mem::size_of::<GroundTerm>() + 64))
            .map_err(QueryEvaluationError::MemoryLimit)?;
        let mut out: Vec<Vec<Option<GroundTerm>>> = Vec::with_capacity(rows.min(BIND_CHUNK));
        let mut patterns = Vec::new();
        for key in &keys {
            self.check()?;
            let mut terms: Vec<Vec<GroundTerm>> = Vec::with_capacity(width);
            for id in key {
                let mut class = Vec::new();
                for &member in names.members(id) {
                    match ground(self, member) {
                        Some(term) => class.push(term),
                        None => return Ok(None),
                    }
                }
                terms.push(class);
            }
            // Odometer order over the identities, the last value fastest.
            let mut at = vec![0usize; width];
            loop {
                out.push(
                    at.iter()
                        .zip(&terms)
                        .map(|(&i, class)| Some(class[i].clone()))
                        .collect(),
                );
                if out.len() == BIND_CHUNK {
                    patterns.push(values_join(&variables, std::mem::take(&mut out), inner));
                }
                let Some(column) = (0..width).rev().find(|&c| at[c] + 1 < terms[c].len()) else {
                    break;
                };
                at[column] += 1;
                at[column + 1..].fill(0);
            }
        }
        if !out.is_empty() {
            patterns.push(values_join(&variables, out, inner));
        }
        Ok(Some((patterns, held)))
    }
}

/// A stored term as a `VALUES` term; `None` for a blank node (no bind join then).
fn ground(ctx: &Context<'_>, id: u64) -> Option<GroundTerm> {
    Some(match ctx.term(id)? {
        Term::NamedNode(n) => GroundTerm::NamedNode(n),
        Term::Literal(l) => GroundTerm::Literal(l),
        // A triple term goes along if it holds no blank node.
        triple @ Term::Triple(_) => GroundTerm::try_from(triple).ok()?,
        Term::BlankNode(_) => return None,
    })
}

/// The `SERVICE` block joined to `rows` as `VALUES`.
fn values_join(
    variables: &[Variable],
    rows: Vec<Vec<Option<GroundTerm>>>,
    inner: &GraphPattern,
) -> GraphPattern {
    GraphPattern::Join {
        inputs: vec![
            GraphPattern::Values {
                variables: variables.to_vec(),
                bindings: rows,
            },
            inner.clone(),
        ],
    }
}

/// The `SERVICE` block with `VALUES` of the distinct values `bound` has for the
/// variables it shares with it, one pattern per [`BIND_CHUNK`] rows; `None` where a bind
/// join doesn't apply (nothing shared, a value unbound or a blank node, too many values).
fn bind_values(
    bound: &Solutions,
    inner: &GraphPattern,
    ctx: &Context<'_>,
) -> Option<Vec<GraphPattern>> {
    let mut in_scope = Vec::new();
    bound_variables(inner, &mut in_scope);
    let shared: Vec<(Variable, usize)> = in_scope
        .into_iter()
        .filter_map(|v| bound.column(&v).map(|c| (v, c)))
        .collect();
    if shared.is_empty() {
        return None;
    }
    let mut seen: HashSet<Vec<u64>> = HashSet::new();
    let mut rows: Vec<Vec<Option<GroundTerm>>> = Vec::new();
    for r in 0..bound.table.len() {
        let key: Vec<u64> = shared.iter().map(|&(_, c)| bound.table.get(r, c)).collect();
        if !seen.insert(key.clone()) {
            continue;
        }
        if seen.len() > BIND_LIMIT {
            return None;
        }
        let mut row = Vec::with_capacity(key.len());
        for id in key {
            row.push(Some(match ctx.term(id)? {
                Term::NamedNode(n) => GroundTerm::NamedNode(n),
                Term::Literal(l) => GroundTerm::Literal(l),
                // A triple term goes along if it holds no blank node.
                triple @ Term::Triple(_) => GroundTerm::try_from(triple).ok()?,
                Term::BlankNode(_) => return None,
            }));
        }
        rows.push(row);
    }
    let variables: Vec<Variable> = shared.into_iter().map(|(v, _)| v).collect();
    Some(
        rows.chunks(BIND_CHUNK)
            .map(|chunk| GraphPattern::Join {
                inputs: vec![
                    GraphPattern::Values {
                        variables: variables.clone(),
                        bindings: chunk.to_vec(),
                    },
                    inner.clone(),
                ],
            })
            .collect(),
    )
}
