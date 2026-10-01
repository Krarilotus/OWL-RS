//! `SERVICE` ([`crate::service`]): the block goes to the endpoint as `SELECT *`, with the
//! values bound so far as `VALUES` when it is joined to them. `SERVICE ?endpoint` calls
//! each endpoint the solutions it is joined to bind, with their rows.

use std::collections::{HashMap, HashSet};

use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::GraphPattern;
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern};

use nrese_exec::{IdTable, UNDEF};
use nrese_rdf::{Term, Variable};

use crate::results::QueryEvaluationError;

use super::{Context, NativeError, NativeResult, Solutions, bound_variables};
use crate::service::{BIND_CHUNK, BIND_LIMIT};

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
        let variable = match name {
            NamedNodePattern::NamedNode(endpoint) => {
                return self.call_silently(endpoint.as_str(), inner, bound, silent);
            }
            NamedNodePattern::Variable(v) => v,
        };
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
            return unbound();
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
        for (id, rows) in endpoints {
            let endpoint = match (id != UNDEF).then(|| self.term(id)).flatten() {
                Some(Term::NamedNode(n)) => n,
                _ if silent => continue,
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
        match all {
            Some(all) => Ok(all),
            None => unbound(),
        }
    }

    /// One endpoint's answer; with `silent`, a failure gives one solution without bindings.
    fn call_silently(
        &self,
        endpoint: &str,
        inner: &GraphPattern,
        bound: Option<&Solutions>,
        silent: bool,
    ) -> NativeResult<Solutions> {
        match self.call_service(endpoint, inner, bound) {
            Err(NativeError::Evaluation(error))
                if silent && !matches!(error, QueryEvaluationError::Cancelled) =>
            {
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
        let Some(services) = &self.services else {
            return Err(QueryEvaluationError::Service(
                format!("SERVICE <{endpoint}>: federation is not enabled on this server").into(),
            )
            .into());
        };
        let patterns = match bound.and_then(|bound| bind_values(bound, inner, self)) {
            Some(chunks) => chunks,
            None => vec![inner.clone()],
        };
        let mut all: Option<Solutions> = None;
        for pattern in patterns {
            self.check()?;
            let query = Query::Select {
                dataset: None,
                pattern,
                base_iri: None,
            }
            .to_string();
            let results = services
                .0
                .select(endpoint, &query, self.cancellation.as_ref())
                .map_err(QueryEvaluationError::Service)?;
            let mut table = IdTable::new(results.variables.len());
            let mut row = vec![UNDEF; results.variables.len()];
            for values in &results.rows {
                for (slot, value) in row.iter_mut().zip(values) {
                    *slot = value.as_ref().map_or(UNDEF, |term| self.id(term));
                }
                table.push_row(&row);
            }
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
                left: Box::new(GraphPattern::Values {
                    variables: variables.clone(),
                    bindings: chunk.to_vec(),
                }),
                right: Box::new(inner.clone()),
            })
            .collect(),
    )
}
