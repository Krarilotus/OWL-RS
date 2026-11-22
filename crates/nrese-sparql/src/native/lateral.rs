//! `LATERAL` (SEP-0006): `left LATERAL { right }` evaluates `right` for each solution of
//! `left`, with that solution's values put in for the variables `right` mentions.
//!
//! As for `EXISTS` by substitution ([`super::substitute`]), `right` is evaluated once per
//! distinct value of the left variables it mentions, not once per row: each result gets
//! a private group key as a column, the results are put together, and one join with `left` gives
//! every row the results of its values. A `LIMIT`, an aggregate or an `ORDER BY` inside
//! `right` therefore applies per left solution, which is what `LATERAL` is for. A `right`
//! that draws a value (`RAND()`, `BNODE()`, …) is evaluated once per row instead, so rows
//! with the same values draw apart. A left
//! variable that is unbound in a row isn't put in: `right` binds it freely there, and the
//! join takes its value.

use std::collections::HashMap;
use std::sync::Arc;

use nrese_exec::{ChargedVec, UNDEF};
use nrese_rdf::{Term, Variable};
use nrese_sparql_syntax::algebra::GraphPattern;

use super::{Context, GraphScope, NativeResult, Solutions, exists::mentioned};
use crate::QueryEvaluationError;

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
        // A right side that draws is evaluated once per row, as SEP-0006 says, not per key.
        let per_row = self.draws_in(right);
        if !per_row && columns.is_empty() && graph_column.is_none() {
            let right = self.eval(right)?;
            return self.join(left, right);
        }
        // A row's values put in, and its graph last.
        let key_of = |table: &nrese_exec::IdTable, row: usize| -> Vec<u64> {
            let mut key: Vec<u64> = columns.iter().map(|(c, _)| table.get(row, *c)).collect();
            key.push(graph_column.map_or(UNDEF, |c| table.get(row, c)));
            key
        };
        // Each distinct key once, in the order the rows first have it. Per row, a row's group
        // is its number, and its key is read from it when its turn comes: nothing is kept
        // per row beyond the group column, charged before it is made.
        let mut keys: Vec<Vec<u64>> = Vec::new();
        let group_variable = self.synthetic("lateral-key");
        let rows = left.table.len();
        // A bound group identity distinguishes UNDEF from the other substitutions:
        // ordinary SPARQL join compatibility alone would mix their right-side results.
        let left = self.reshaped(left, rows.saturating_mul(8), |mut left| {
            let groups: Vec<u64> = match per_row {
                true => (0..rows as u64).collect(),
                false => {
                    let mut seen: HashMap<Vec<u64>, u64> = HashMap::new();
                    (0..rows)
                        .map(|row| {
                            let key = key_of(&left.table, row);
                            let next = keys.len() as u64;
                            *seen.entry(key.clone()).or_insert_with(|| {
                                keys.push(key);
                                next
                            })
                        })
                        .collect()
                }
            };
            left.table.push_column(groups);
            left.vars.push(group_variable.clone());
            left
        })?;
        let groups = if per_row { rows } else { keys.len() };
        let mut results = Appended::default();
        let mut keys = keys.into_iter();
        for group in 0..groups {
            self.check()?;
            let key = match per_row {
                true => key_of(&left.table, group),
                false => keys.next().expect("a key per group"),
            };
            let mut terms = HashMap::new();
            let mut put_in: Vec<(Variable, u64)> = Vec::new();
            for ((_, variable), &id) in columns.iter().zip(&key) {
                if id == UNDEF {
                    continue;
                }
                if let Some(term) = self.term(id) {
                    if let Term::BlankNode(b) = &term {
                        self.register_alias(
                            nrese_sparql_syntax::internal::blank_constant(b.as_str()).as_str(),
                            id,
                        );
                    }
                    terms.insert(variable.clone(), term);
                    put_in.push((variable.clone(), id));
                }
            }
            let substituted = super::substitute::Values::new(self, &terms)?.pattern(right);
            self.check_scoped_operators(&substituted)?;
            let graph = key.last().copied().filter(|&id| id != UNDEF);
            let found = match graph {
                Some(id) => self.in_graph(
                    GraphScope::Named(nrese_engine::TermId::from_raw(id)),
                    &substituted,
                )?,
                None => self.eval(&substituted)?,
            };
            // The values put in, as columns: what joins the results to their rows. A column
            // per value the results lack, however many the query puts in, charged before
            // they are made.
            let rows = found.table.len();
            let mut added: Vec<(Variable, u64)> = put_in;
            added.push((group_variable.clone(), group as u64));
            if let (Some(id), GraphScope::Variable(g)) = (graph, &*self.graph.borrow()) {
                added.push((g.clone(), id));
            }
            let mut lacking: Vec<(Variable, u64)> = Vec::with_capacity(added.len());
            for (variable, id) in added {
                if found.column(&variable).is_none() && lacking.iter().all(|(v, _)| *v != variable)
                {
                    lacking.push((variable, id));
                }
            }
            let growth = rows.saturating_mul(8).saturating_mul(lacking.len());
            let found = self.reshaped(found, growth, |mut found| {
                for (variable, id) in lacking {
                    found.table.push_column(vec![id; rows]);
                    found.vars.push(variable);
                }
                found
            })?;
            results.push(self, found)?;
        }
        if results.vars.is_empty() {
            return self.drop_added(left, 1);
        }
        let results = results.into_solutions();
        self.returned_table_charge(results.table.memory_bytes());
        let joined = self.join(left, results)?;
        let variables: Vec<_> = joined
            .vars
            .iter()
            .filter(|v| **v != group_variable)
            .cloned()
            .collect();
        self.project(joined, &variables)
    }
}

/// The right side's results, each appended to columns that are charged before they grow
/// (doubling): copying linear in the rows, not a union of each into all before it.
#[derive(Default)]
struct Appended {
    vars: Vec<Variable>,
    columns: Vec<ChargedVec<u64>>,
    len: usize,
}

impl Appended {
    /// `found`'s rows appended (a variable it lacks unbound there), and its charge released.
    fn push(&mut self, ctx: &Context<'_>, found: Solutions) -> NativeResult<()> {
        let appended = (|| -> Result<(), nrese_exec::BudgetExceeded> {
            for v in &found.vars {
                if !self.vars.contains(v) {
                    let mut column = ChargedVec::new(Arc::clone(&ctx.budget));
                    column.resize(self.len, UNDEF)?;
                    self.vars.push(v.clone());
                    self.columns.push(column);
                }
            }
            let rows = found.table.len();
            for (v, column) in self.vars.iter().zip(&mut self.columns) {
                match found.column(v) {
                    Some(c) => column.extend_from_slice(found.table.column(c))?,
                    None => column.resize(self.len + rows, UNDEF)?,
                }
            }
            self.len += rows;
            Ok(())
        })();
        ctx.consumed(found);
        appended.map_err(|e| QueryEvaluationError::MemoryLimit(e).into())
    }

    /// The rows as one table, which takes the columns' charge.
    fn into_solutions(self) -> Solutions {
        let columns = self
            .columns
            .into_iter()
            .map(|column| {
                let (items, held) = column.into_parts();
                held.transfer();
                items
            })
            .collect();
        Solutions {
            vars: self.vars,
            table: nrese_exec::IdTable::from_columns(columns),
            ordered: false,
        }
    }
}
