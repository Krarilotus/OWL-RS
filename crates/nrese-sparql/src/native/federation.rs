//! `SERVICE` ([`crate::service`]): the block goes to the endpoint as `SELECT *`, with the
//! values bound so far as `VALUES` when it is joined to them.

use std::collections::HashSet;

use spargebra::Query;
use spargebra::algebra::GraphPattern;
use spargebra::term::{GroundTerm, NamedNodePattern};

use nrese_exec::{IdTable, UNDEF};
use oxrdf::{Term, Variable};
use spareval::QueryEvaluationError;

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
        let NamedNodePattern::NamedNode(endpoint) = name else {
            return Err(NativeError::Fallback);
        };
        match self.call_service(endpoint.as_str(), inner, bound) {
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
