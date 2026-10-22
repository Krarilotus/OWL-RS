//! `SERVICE` ([`crate::service`]): the block goes to the endpoint as `SELECT *`, with the
//! values bound so far as `VALUES` when it is joined to them. `SERVICE ?endpoint` calls
//! each endpoint the solutions it is joined to bind, with their rows.

use std::collections::HashSet;

mod response;

use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern};

use nrese_exec::{IdTable, UNDEF};
use nrese_rdf::{Term, Variable};

use crate::results::QueryEvaluationError;

use super::{Context, NativeError, NativeResult, Solutions, bound_variables};
use crate::service::{BIND_CHUNK, BIND_LIMIT};

#[derive(Clone, Copy)]
pub(super) enum ServiceJoin<'a> {
    Inner,
    Left(Option<&'a Expression>),
}

impl Context<'_> {
    /// `SERVICE name { inner }`, joined to `bound` if given; with `silent`, a failing
    /// endpoint gives one solution without bindings.
    pub(super) fn service(
        &self,
        name: &NamedNodePattern,
        inner: &GraphPattern,
        silent: bool,
        bound: Option<&Solutions>,
    ) -> NativeResult<Solutions> {
        if super::vectors::is_search(name) {
            return self.vector_service(inner, bound);
        }
        match name {
            NamedNodePattern::NamedNode(endpoint) => {
                self.call_silently(endpoint.as_str(), inner, bound, silent)
            }
            NamedNodePattern::Variable(variable) if !silent => Err(QueryEvaluationError::Service(
                format!("SERVICE {variable}: the variable has no value").into(),
            )
            .into()),
            NamedNodePattern::Variable(_) => Ok(Solutions::unit()),
        }
    }

    /// A bound SERVICE variable joins only the local group that selected its endpoint.
    /// In particular, UNDEF rows never join another group's remote answer.
    pub(super) fn bound_service(
        &self,
        name: &NamedNodePattern,
        inner: &GraphPattern,
        silent: bool,
        bound: Solutions,
        join: ServiceJoin<'_>,
    ) -> NativeResult<Solutions> {
        self.check()?;
        let finish = |left, right| match join {
            ServiceJoin::Inner => self.join(left, right),
            ServiceJoin::Left(expression) => self.left_join(left, right, expression),
        };
        let NamedNodePattern::Variable(variable) = name else {
            let found = self.service(name, inner, silent, Some(&bound))?;
            return finish(bound, found);
        };
        if bound.table.is_empty() {
            return Ok(bound);
        }
        let column = bound.column(variable);
        // Charge row-index storage before growth; sorting the pairs needs no heap scratch.
        let mut groups = nrese_exec::ChargedVec::new(std::sync::Arc::clone(&self.budget));
        let stop = nrese_exec::StopSource::new(
            self.cancellation
                .as_ref()
                .map(crate::CancellationToken::flag),
            None,
        );
        let mut ticker = stop.ticker().map_err(|_| QueryEvaluationError::Cancelled)?;
        for row in 0..bound.table.len() {
            ticker
                .tick(1)
                .map_err(|_| QueryEvaluationError::Cancelled)?;
            groups
                .push((column.map_or(UNDEF, |c| bound.table.get(row, c)), row))
                .map_err(QueryEvaluationError::MemoryLimit)?;
        }
        groups.sort_unstable();
        let mut all = None;
        let mut at = 0;
        while at < groups.len() {
            self.check()?;
            let id = groups[at].0;
            let mut end = at + 1;
            while end < groups.len() && groups[end].0 == id {
                ticker
                    .tick(1)
                    .map_err(|_| QueryEvaluationError::Cancelled)?;
                end += 1;
            }
            let mut table = IdTable::charged(bound.table.width(), 0, &self.budget)?;
            for &(_, row) in &groups[at..end] {
                ticker
                    .tick(1)
                    .map_err(|_| QueryEvaluationError::Cancelled)?;
                table.push_row(&bound.table.row(row), &self.budget)?;
            }
            // CT owns table/header admission; produced retains the existing operator
            // bookkeeping before the group joins through the normal owner.
            let subset = self.produced(Solutions {
                vars: bound.vars.clone(),
                table,
                ordered: false,
            })?;
            let found = match (id != UNDEF).then(|| self.term(id)).flatten() {
                Some(Term::NamedNode(endpoint)) => {
                    self.call_silently(endpoint.as_str(), inner, Some(&subset), silent)?
                }
                _ if silent => Solutions::unit(),
                _ => {
                    return Err(QueryEvaluationError::Service(
                        format!("SERVICE {variable}: an endpoint must be an IRI").into(),
                    )
                    .into());
                }
            };
            let found = finish(subset, found)?;
            all = Some(match all {
                Some(all) => self.union(all, found)?,
                None => found,
            });
            at = end;
        }
        self.consumed(bound);
        self.check()?;
        Ok(all.expect("a nonempty bound table has an endpoint group"))
    }

    /// One endpoint's answer; with `silent`, a failure gives one solution without bindings.
    fn call_silently(
        &self,
        endpoint: &str,
        inner: &GraphPattern,
        bound: Option<&Solutions>,
        silent: bool,
    ) -> NativeResult<Solutions> {
        let result = self.call_service(endpoint, inner, bound);
        self.check()?;
        match result {
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
            let query = Query::Select {
                dataset: None,
                pattern,
                base_iri: self.evaluator.base().cloned(),
            }
            .to_string();
            let results = services
                .0
                .select(endpoint, &query, self.cancellation.as_ref());
            // The old boxed client API can report a stop as an ordinary endpoint error.
            // The caller's reason wins before SILENT may see either a result or an error.
            self.check()?;
            let results =
                results.map_err(|error| match error.downcast::<QueryEvaluationError>() {
                    Ok(error) => *error,
                    Err(error) => QueryEvaluationError::Service(error),
                })?;
            // The endpoint's terms as canonical answers name them.
            let table = match &self.canonical {
                None => import(self, &results, |term| self.id(term))?,
                Some(names) => import(self, &results, |term| {
                    names
                        .id(term)
                        .map_or_else(|| self.id(&names.term(term.clone())), Ok)
                })?,
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
fn import(
    ctx: &Context<'_>,
    results: &crate::ServiceResults,
    id: impl Fn(&Term) -> NativeResult<u64>,
) -> NativeResult<IdTable> {
    let scope = response::Scope::new(ctx, results)?;
    let mut table = IdTable::charged(results.variables.len(), 0, &ctx.budget)?;
    let mut row = vec![UNDEF; results.variables.len()];
    for values in &results.rows {
        ctx.check()?;
        if values.len() != row.len() {
            return Err(
                QueryEvaluationError::Service("a SERVICE row has the wrong width".into()).into(),
            );
        }
        for (slot, value) in row.iter_mut().zip(values) {
            *slot = match value {
                None => UNDEF,
                Some(term) => match &scope {
                    Some(scope) => scope.with_term(ctx, term, &id)?,
                    None => id(term)?,
                },
            };
        }
        table.push_row(&row, &ctx.budget)?;
    }
    ctx.check()?;
    Ok(table)
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
