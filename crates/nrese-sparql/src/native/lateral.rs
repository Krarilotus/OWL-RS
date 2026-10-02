//! `LATERAL` (SEP-0006): `left LATERAL { right }` evaluates `right` for each solution of
//! `left`, with that solution's values put in for the variables `right` mentions.
//!
//! As for `EXISTS` by substitution ([`super::substitute`]), `right` is evaluated once per
//! distinct value of the left variables it mentions, not once per row: each result gets
//! those values as columns, the results are put together, and one join with `left` gives
//! every row the results of its values. A `LIMIT`, an aggregate or an `ORDER BY` inside
//! `right` therefore applies per left solution, which is what `LATERAL` is for. A left
//! variable that is unbound in a row isn't put in: `right` binds it freely there, and the
//! join takes its value.

use std::collections::HashMap;

use nrese_exec::{IdTable, UNDEF};
use nrese_rdf::{Term, Variable};
use nrese_sparql_syntax::algebra::GraphPattern;

use super::{Context, GraphScope, NativeResult, Solutions, exists::mentioned};

impl Context<'_> {
    pub(super) fn lateral(&self, left: Solutions, right: &GraphPattern) -> NativeResult<Solutions> {
        let visible = mentioned(right);
        let columns: Vec<(usize, Variable)> = left
            .vars
            .iter()
            .enumerate()
            .filter(|(_, v)| visible.contains(v))
            .map(|(i, v)| (i, v.clone()))
            .collect();
        // Under `GRAPH ?g` evaluated for all graphs at once, each row's graph is its `?g`.
        let graph_column = match &*self.graph.borrow() {
            GraphScope::Variable(v) => left.column(v),
            _ => None,
        };
        if columns.is_empty() && graph_column.is_none() {
            let right = self.eval(right)?;
            return self.join(left, right);
        }
        // Each distinct key once, in the order the rows first have it.
        let mut seen: HashMap<Vec<u64>, ()> = HashMap::new();
        let mut keys: Vec<Vec<u64>> = Vec::new();
        for row in 0..left.table.len() {
            let mut key: Vec<u64> = columns
                .iter()
                .map(|(c, _)| left.table.get(row, *c))
                .collect();
            key.push(graph_column.map_or(UNDEF, |c| left.table.get(row, c)));
            if seen.insert(key.clone(), ()).is_none() {
                keys.push(key);
            }
        }
        let mut results: Option<Solutions> = None;
        for key in keys {
            self.check()?;
            let mut terms = HashMap::new();
            let mut put_in: Vec<(Variable, u64)> = Vec::new();
            for ((_, variable), &id) in columns.iter().zip(&key) {
                if id == UNDEF {
                    continue;
                }
                if let Some(term) = self.term(id) {
                    if let Term::BlankNode(b) = &term {
                        self.register_alias(super::substitute::alias(b.as_str()).as_str(), id);
                    }
                    terms.insert(variable.clone(), term);
                    put_in.push((variable.clone(), id));
                }
            }
            let substituted = super::substitute::Values { terms: &terms }.pattern(right);
            let graph = key.last().copied().filter(|&id| id != UNDEF);
            let mut found = match graph {
                Some(id) => self.in_graph(
                    GraphScope::Named(nrese_engine::TermId::from_raw(id)),
                    &substituted,
                )?,
                None => self.eval(&substituted)?,
            };
            // The values put in, as columns: what joins the results to their rows.
            let rows = found.table.len();
            let mut added: Vec<(Variable, u64)> = put_in;
            if let (Some(id), GraphScope::Variable(g)) = (graph, &*self.graph.borrow()) {
                added.push((g.clone(), id));
            }
            for (variable, id) in added {
                if found.column(&variable).is_some() {
                    continue;
                }
                let mut columns = std::mem::take(&mut found.table).into_columns();
                columns.push(vec![id; rows]);
                found.table = IdTable::from_columns(columns);
                found.vars.push(variable);
            }
            results = Some(match results {
                None => found,
                Some(results) => self.union(results, found)?,
            });
        }
        match results {
            Some(results) => self.join(left, results),
            None => Ok(left),
        }
    }
}
