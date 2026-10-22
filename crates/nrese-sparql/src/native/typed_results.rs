//! Owned results before decoding. IDs never outlive their dictionary or computed terms.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use nrese_engine::Snapshot;
use nrese_exec::{Budget, IdTable, Reservation, UNDEF, computed_id, computed_index};
use nrese_rdf::{Term, Triple, Variable};

use super::{Holding, Outcome};
use crate::{QueryEvaluationError, QueryResults, QuerySolutionIter, QueryTripleIter};

/// An evaluated query, retaining encoded solutions until their consumer needs terms.
// Keep the common solutions result inline; boxing it would add an allocation to queries.
#[allow(clippy::large_enum_variant)]
pub enum TypedResults {
    Boolean(bool),
    Solutions(SolutionTable),
    Graph(Vec<Triple>, Reservation),
}

/// An ID table and its complete decoding context. Row order and multiplicity are kept.
/// What it holds stays reserved until it drops.
pub struct SolutionTable {
    pub(super) snapshot: Snapshot,
    pub(super) variables: Arc<[Variable]>,
    pub(super) table: IdTable,
    pub(super) computed: Arc<Vec<Term>>,
    pub(super) held: Reservation,
    /// What alignment added, on the budget it was given.
    aligned: Option<Reservation>,
}

impl SolutionTable {
    pub fn variables(&self) -> &[Variable] {
        &self.variables
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// O(columns). Decodes only this row, including query-local computed values.
    pub fn row(&self, row: usize) -> Vec<Option<Term>> {
        (0..self.table.width())
            .map(|column| self.term(self.table.get(row, column)))
            .collect()
    }

    pub fn term(&self, id: u64) -> Option<Term> {
        super::decode(&self.snapshot, &self.computed, id)
    }

    /// Bytes reserved for this result.
    pub fn reserved_bytes(&self) -> usize {
        self.held.bytes()
            + self.table.charge()
            + self.aligned.as_ref().map_or(0, Reservation::bytes)
    }

    /// Align both results to one dictionary/computed-value domain. O(cells + computed
    /// values); dictionary IDs stay untouched when the views share their dictionary.
    /// Unrelated dictionaries are rejected rather than comparing equal raw integers.
    pub fn align(
        mut self,
        mut upper: Self,
        budget: Arc<Budget>,
    ) -> Result<AlignedSolutions, QueryEvaluationError> {
        if self.variables != upper.variables
            || !self.snapshot.shares_dictionary_with(&upper.snapshot)
        {
            return Err(QueryEvaluationError::Argument(
                "bound results need the same variables and dictionary".to_owned(),
            ));
        }
        let computed_bytes = self
            .computed
            .iter()
            .chain(upper.computed.iter())
            .map(|t| crate::cache::term_heap(t).saturating_add(128))
            .sum::<usize>();
        let mut held = Reservation::new(Arc::clone(&budget));
        held.charge(computed_bytes.saturating_mul(3))?;
        let mut values = std::collections::HashMap::new();
        let mut computed = Vec::new();
        let mut remap = |terms: &[Term]| {
            terms
                .iter()
                .map(|term| {
                    if let Some(id) = upper
                        .snapshot
                        .lookup(term.as_ref())
                        .or_else(|| self.snapshot.lookup(term.as_ref()))
                    {
                        return id.raw();
                    }
                    *values.entry(term.clone()).or_insert_with(|| {
                        let id = computed_id(computed.len() as u64);
                        computed.push(term.clone());
                        id
                    })
                })
                .collect::<Vec<_>>()
        };
        let lower_map = remap(&self.computed);
        let upper_map = remap(&upper.computed);
        fn translate(table: IdTable, map: &[u64]) -> IdTable {
            // Keep zero-column cardinality, and don't lose useful order when no ID changes.
            if table.width() == 0
                || map
                    .iter()
                    .enumerate()
                    .all(|(i, &id)| id == computed_id(i as u64))
            {
                return table;
            }
            let mut columns = table.into_columns();
            for column in columns.values_mut() {
                for id in column {
                    if *id != UNDEF
                        && let Some(i) = computed_index(*id)
                    {
                        *id = map[i as usize];
                    }
                }
            }
            IdTable::from_columns(columns)
        }
        self.table = translate(self.table, &lower_map);
        upper.table = translate(upper.table, &upper_map);
        // Both tables held under the aligned results' budget from now on: their growth and
        // working memory (appends, deduplication) are charged to it.
        self.table.recharge(&budget)?;
        upper.table.recharge(&budget)?;
        let computed = Arc::new(computed);
        self.computed = Arc::clone(&computed);
        upper.computed = computed;
        // Upper may know newly interned dictionary terms absent at lower's revision.
        self.snapshot = upper.snapshot.clone();
        Ok(AlignedSolutions {
            lower: self,
            upper,
            held,
        })
    }
}

/// Two solutions whose raw IDs now have the same meaning. Store owns which rows to
/// decide; this boundary owns translation, decoding and retained result storage.
pub struct AlignedSolutions {
    lower: SolutionTable,
    upper: SolutionTable,
    held: Reservation,
}

impl AlignedSolutions {
    pub fn lower(&self) -> &IdTable {
        &self.lower.table
    }
    pub fn upper(&self) -> &IdTable {
        &self.upper.table
    }
    pub fn variables(&self) -> &[Variable] {
        self.lower.variables()
    }
    pub fn term(&self, id: u64) -> Option<Term> {
        self.lower.term(id)
    }
    pub fn upper_row(&self, row: usize) -> Vec<Option<Term>> {
        self.upper.row(row)
    }

    /// Uses the shared ID-table set primitive; only U changes order. Its working memory
    /// (the permutation, the columns gathered, the mask) is charged to the aligned results'
    /// budget, which holds the table since [`SolutionTable::align`], before it is made.
    pub fn deduplicate_upper(&mut self) -> Result<(), QueryEvaluationError> {
        self.upper.table.dedup()?;
        Ok(())
    }

    /// Makes room for `rows` appended rows at once, charged to the aligned results' budget
    /// first (the lower table's growth: the old and the new allocation are held while the
    /// rows move).
    pub fn reserve_appends(&mut self, rows: usize) -> Result<(), QueryEvaluationError> {
        let budget = Arc::clone(self.held.handle());
        self.lower.table.reserve_rows_exact(rows, &budget)?;
        Ok(())
    }

    /// Appends a proved candidate once, preserving every original lower row in place.
    pub fn append_upper(&mut self, row: usize) -> Result<(), QueryEvaluationError> {
        if self.lower.table.room_left() == 0 {
            self.reserve_appends(self.lower.len().max(4))?;
        }
        self.lower.table.push_reserved(&self.upper.table.row(row));
        Ok(())
    }

    pub fn into_lower(mut self) -> SolutionTable {
        self.lower.aligned = Some(self.held);
        self.lower
    }
}

impl TypedResults {
    /// Decode on delivery, without evaluating the query again.
    pub fn into_results(self) -> QueryResults<'static> {
        match self {
            Self::Boolean(value) => QueryResults::Boolean(value),
            Self::Graph(triples, held) => QueryResults::Graph(QueryTripleIter::new(Holding::new(
                triples.into_iter().map(Ok),
                held,
            ))),
            Self::Solutions(solutions) => {
                let variables = Arc::clone(&solutions.variables);
                let rows = (0..solutions.len()).map(move |row| Ok(solutions.row(row)));
                QueryResults::Solutions(QuerySolutionIter::new(variables, rows))
            }
        }
    }
}

/// Evaluates through the same native step as ordinary result delivery. The solutions
/// keep their IDs and snapshot, and their computed terms are reserved too.
pub(crate) fn evaluate(
    snapshot: Snapshot,
    query: &nrese_sparql_syntax::Query,
    options: &crate::QueryOptions,
) -> Result<TypedResults, QueryEvaluationError> {
    let (outcome, held) = super::run(&snapshot, query, options)?;
    Ok(match outcome {
        Outcome::Boolean(value) => TypedResults::Boolean(value),
        Outcome::Graph(triples) => TypedResults::Graph(triples, held),
        // The computed terms were charged as they were made, and `held` took that over;
        // the table holds its own charge.
        Outcome::Solutions(solutions, computed) => TypedResults::Solutions(SolutionTable {
            snapshot,
            variables: solutions.vars.into(),
            table: solutions.table,
            computed: Arc::new(computed),
            held,
            aligned: None,
        }),
    })
}
