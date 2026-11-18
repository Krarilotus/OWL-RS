//! A table a scan builds row by row, or range by range, whose columns are charged to the
//! query's budget before they grow (D16-039 F5): a scan the index can't answer a column at a
//! time (a merged default graph, a variable twice in one pattern, several runs) learns its
//! rows only as it reads them, so each column grows as a [`ChargedVec`]: one budget call a
//! growth, none a row, and near the limit it grows to what fits rather than by a row at a
//! time.

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, IdTable, Reservation};

/// The columns of a table being built, each charged before it grows.
pub(super) struct Columns<'b> {
    columns: Vec<ChargedVec<u64, &'b Budget>>,
}

impl<'b> Columns<'b> {
    /// `width` empty columns on `budget`. A table without columns isn't made here: its
    /// rows hold nothing until something makes a column of them ([`IdTable::memory_bytes`]).
    pub(super) fn new(budget: &'b Budget, width: usize) -> Self {
        debug_assert!(
            width > 0,
            "a table without columns allocates nothing per row"
        );
        Self {
            columns: (0..width).map(|_| ChargedVec::new(budget)).collect(),
        }
    }

    /// One more row: `row[i]` onto column `i`.
    pub(super) fn push_row(&mut self, row: &[u64]) -> Result<(), BudgetExceeded> {
        debug_assert_eq!(row.len(), self.columns.len());
        for (column, &value) in self.columns.iter_mut().zip(row) {
            column.push(value)?;
        }
        Ok(())
    }

    /// The rows of `columns` (one per column of this table, of equal lengths) appended.
    pub(super) fn extend(&mut self, columns: &[Vec<u64>]) -> Result<(), BudgetExceeded> {
        debug_assert_eq!(columns.len(), self.columns.len());
        for (column, values) in self.columns.iter_mut().zip(columns) {
            column.extend_from_slice(values)?;
        }
        Ok(())
    }

    /// The table, and the charge of exactly its bytes, which its holder hands on with it.
    pub(super) fn into_table(self) -> Result<(IdTable, Reservation<&'b Budget>), BudgetExceeded> {
        let mut parts = self.columns.into_iter().map(ChargedVec::into_parts);
        let (first, mut held) = parts.next().expect("a column");
        let mut columns = Vec::with_capacity(parts.len() + 1);
        columns.push(first);
        for (column, charge) in parts {
            held.absorb(charge);
            columns.push(column);
        }
        let table = IdTable::from_columns(columns);
        held.resize(table.memory_bytes())?;
        Ok((table, held))
    }
}
