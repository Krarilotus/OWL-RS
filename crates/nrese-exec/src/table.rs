//! [`IdTable`]: the one intermediate-result representation.
//!
//! Column-major, like QLever's: each column is a contiguous `Vec<u64>`, so scans, filters
//! and joins touch only the columns they need, and the compiler can vectorise loops over
//! them. Rows are addressed by index. A table records which columns it's sorted on
//! ([`sorted_by`](IdTable::sorted_by)), so a join can skip sorting inputs that already arrive
//! in the right order (index scans produce sorted output).
//!
//! A table owns the charge of its column storage: the columns grow through one path,
//! [`IdTable::reserve_rows`], which charges the new capacity to the table's budget before it
//! allocates ([`ChargedVec`](crate::ChargedVec)'s growth, for all columns at once), and the
//! charge is released when the table drops, after its columns are freed. A table of zero width
//! holds no charge and no budget; a nonzero-width empty table still owns its header. Rows are written into reserved room
//! ([`IdTable::push_reserved`]): one check per row, no allocation, no atomics.
//!
//! Tables made by the older constructors (`with_capacity`, `from_columns`, `push_row`)
//! grow uncharged; the executor charges what they hold when it produces them
//! ([`IdTable::settle`]) until it builds every table through the charged path. Either way
//! a table owns its charge, and its columns taken out ([`IdTable::into_columns`]) carry it.

use std::sync::Arc;

use rayon::prelude::*;

use crate::{Budget, BudgetExceeded, Reservation, Scratch};

/// Row count from which sorting runs in parallel.
const PARALLEL_SORT_ROWS: usize = 1 << 16;

/// Columns of `rows` values each, with the charge of the memory they hold: a table's taken
/// out of it ([`IdTable::into_columns`]), or made one at a time ([`Columns::new`],
/// [`Columns::push`], each charged before it is made). Their values can be read and
/// changed in place; columns arrive through charged constructors or [`Columns::append`],
/// so none comes uncharged. Their charge is released after them, or
/// taken over by the table they make ([`IdTable::from_columns`], which settles it to what
/// it holds when it is produced).
#[derive(Debug, Default)]
pub struct Columns {
    /// Before `held`: fields drop in order, the columns first.
    columns: Vec<Vec<u64>>,
    rows: usize,
    header: usize,
    held: Option<Reservation<Arc<Budget>>>,
}

impl Columns {
    /// No columns yet, for columns of `rows` values.
    pub fn new(rows: usize) -> Self {
        Self {
            columns: Vec::new(),
            rows,
            header: 0,
            held: None,
        }
    }

    /// Moves one operation's charged columns, preserving their pointers and capacities.
    /// O(width): only Vec headers and tokens move; no payload is copied or readmitted.
    /// The new outer header is reserved before allocation. The caller must keep any old
    /// Scratch-list header charged until the consumed iterator frees that allocation.
    /// Zero columns preserve `rows` without retaining a budget; zero rows with columns
    /// still require the outer header and retain each column's capacity charge.
    ///
    /// # Panics
    /// If a column has a different row count or actual Budget, the exact-size iterator
    /// lies about its length, or (in measurement builds) tokens have different origins.
    /// Inputs must come from one operation; this does not combine arbitrary origins.
    pub fn from_scratch<'b>(
        rows: usize,
        mut columns: impl ExactSizeIterator<Item = Scratch<'b, u64>>,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let width = columns.len();
        if width == 0 {
            assert!(columns.next().is_none(), "exact column count");
            return Ok(Self::new(rows));
        }
        let header = std::alloc::Layout::array::<Vec<u64>>(width)
            .map_err(|_| budget.refusal(usize::MAX))?
            .size();
        // Keep both the incoming pair and the assembled payload before their token in
        // field-drop order, including a refusal, a failed check or an iterator unwind.
        let first = columns.next().expect("exact column count").into_parts();
        let mut made = (Vec::new(), first);
        assert_eq!(made.1.0.len(), rows, "a column of every row");
        assert!(std::ptr::eq(made.1.1.budget(), &**budget), "one budget");
        made.1.1.charge(header)?;
        made.0 = Vec::with_capacity(width);
        made.0.push(std::mem::take(&mut made.1.0));
        // Vec::with_capacity requests exactly this many non-ZST headers.
        debug_assert_eq!(made.0.capacity() * size_of::<Vec<u64>>(), header);
        for _ in 1..width {
            let mut column = columns.next().expect("exact column count").into_parts();
            assert_eq!(column.0.len(), rows, "a column of every row");
            made.1.1.absorb_same_origin(&mut column.1);
            // The exact width bounds every push: no unreserved header growth.
            made.0.push(column.0);
        }
        assert!(columns.next().is_none(), "exact column count");
        let (columns, (_, held)) = made;
        Ok(Self {
            columns,
            rows,
            header,
            // Identity was checked before charging and is preserved by the merge.
            held: Some(held.into_shared(budget)),
        })
    }

    /// The values per column.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// `bytes` more charged to the columns' budget (`budget` while they hold none).
    fn charge(&mut self, bytes: usize, budget: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        self.held
            .get_or_insert_with(|| Reservation::new(Arc::clone(budget)))
            .charge(bytes)
    }

    /// A column of `values` added (the first `rows` of them; panics on fewer), charged
    /// before it is made: the column, and the vector of columns where it grows. Fails
    /// without adding it.
    pub fn push(
        &mut self,
        values: impl IntoIterator<Item = u64>,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        let header = match self.columns.len() == self.columns.capacity() {
            true => (self.columns.capacity() * 2).max(4) * size_of::<Vec<u64>>(),
            false => 0,
        };
        let column = self
            .rows
            .checked_mul(8)
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        self.charge(column + header, budget)?;
        self.header += header;
        let mut made = Vec::with_capacity(self.rows);
        made.extend(values.into_iter().take(self.rows));
        assert_eq!(made.len(), self.rows, "a column of every row");
        self.columns.push(made);
        Ok(())
    }

    /// A copy of column `index` added, charged before it is made ([`Self::push`]).
    pub fn push_copy(&mut self, index: usize, budget: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        let rows = self.rows;
        let header = match self.columns.len() == self.columns.capacity() {
            true => (self.columns.capacity() * 2).max(4) * size_of::<Vec<u64>>(),
            false => 0,
        };
        self.charge(rows * 8 + header, budget)?;
        self.header += header;
        let mut made = Vec::with_capacity(rows);
        made.extend_from_slice(&self.columns[index]);
        self.columns.push(made);
        Ok(())
    }

    /// A column of `values` added where each may fail, charged before it is made
    /// ([`Self::push`]): the first error is returned, the column dropped and its charge
    /// released.
    pub fn push_try<E: From<BudgetExceeded>>(
        &mut self,
        values: impl IntoIterator<Item = Result<u64, E>>,
        budget: &Arc<Budget>,
    ) -> Result<(), E> {
        let rows = self.rows;
        let header = match self.columns.len() == self.columns.capacity() {
            true => (self.columns.capacity() * 2).max(4) * size_of::<Vec<u64>>(),
            false => 0,
        };
        let column = rows
            .checked_mul(8)
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        self.charge(column, budget)?;
        let mut made = Vec::with_capacity(rows);
        for value in values.into_iter().take(rows) {
            match value {
                Ok(value) => made.push(value),
                Err(error) => {
                    drop(made);
                    if let Some(held) = &mut self.held {
                        held.release(column);
                    }
                    return Err(error);
                }
            }
        }
        assert_eq!(made.len(), rows, "a column of every row");
        self.charge(header, budget)?;
        self.header += header;
        self.columns.push(made);
        Ok(())
    }

    /// Column `index`'s values, to change in place.
    pub fn column_mut(&mut self, index: usize) -> &mut [u64] {
        &mut self.columns[index]
    }

    /// `other`'s columns added after these, with their charge (to the same budget): nothing
    /// is charged or released, the vector of columns growth aside.
    pub fn append(
        &mut self,
        mut other: Columns,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        assert!(
            other.columns.is_empty() || other.rows == self.rows,
            "columns of every row"
        );
        let more = other.columns.len();
        let header = match self.columns.capacity() - self.columns.len() < more {
            true => (self.columns.len() + more) * size_of::<Vec<u64>>(),
            false => 0,
        };
        self.charge(header, budget)?;
        self.header += header;
        if let Some(theirs) = other.held.take() {
            match &mut self.held {
                Some(ours) => ours.absorb(theirs),
                None => self.held = Some(theirs),
            }
        }
        self.columns.append(&mut other.columns);
        Ok(())
    }

    /// The first `width` columns kept; the others dropped (their charge stays until the
    /// table they make is settled).
    pub fn truncate(&mut self, width: usize) {
        self.columns.truncate(width);
    }

    /// The columns' values, to change in place.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut [u64]> {
        self.columns.iter_mut().map(Vec::as_mut_slice)
    }
}

/// The columns, to read.
impl std::ops::Deref for Columns {
    type Target = [Vec<u64>];

    fn deref(&self) -> &[Vec<u64>] {
        &self.columns
    }
}

#[derive(Debug, Default)]
pub struct IdTable {
    columns: Vec<Vec<u64>>,
    len: usize,
    /// The table is sorted lexicographically on these columns (in this order). Empty if
    /// nothing is known.
    sorted_by: Vec<usize>,
    /// Rows every column has room for: at most each column's capacity.
    room: usize,
    /// The bytes of the columns' own vector, charged with the columns since the table's
    /// first charged growth (0 before).
    header: usize,
    /// The charge of the columns' capacity, from the table's first charged growth. After
    /// `columns`: fields drop in order, the columns first.
    held: Option<Reservation<Arc<Budget>>>,
}

/// The columns' rows and order; not their capacity or charge.
impl PartialEq for IdTable {
    fn eq(&self, other: &Self) -> bool {
        (&self.columns, self.len, &self.sorted_by) == (&other.columns, other.len, &other.sorted_by)
    }
}

impl Eq for IdTable {}

/// An uncharged copy ([`IdTable::try_clone`] is the charged one).
impl Clone for IdTable {
    fn clone(&self) -> Self {
        Self::uncharged(self.columns.clone(), self.len, self.sorted_by.clone())
    }
}

impl IdTable {
    /// An empty table with `width` columns. It holds no charge, and no memory but the vector
    /// of its (empty) columns, charged with them when they first grow. Legacy unbudgeted
    /// construction only: budget-aware callers use [`Self::charged`], including zero rows.
    pub fn new(width: usize) -> Self {
        Self::uncharged((0..width).map(|_| Vec::new()).collect(), 0, Vec::new())
    }

    fn uncharged(columns: Vec<Vec<u64>>, len: usize, sorted_by: Vec<usize>) -> Self {
        let mut table = Self {
            columns,
            len,
            sorted_by,
            room: 0,
            header: 0,
            held: None,
        };
        table.room = table.capacity_rows();
        table
    }

    /// The rows every column has room for.
    fn capacity_rows(&self) -> usize {
        self.columns
            .iter()
            .map(Vec::capacity)
            .min()
            .unwrap_or(usize::MAX)
    }

    /// An empty table with room for `rows` rows, header and payload admitted before
    /// allocation. Nonzero width owns its header even when `rows` is zero; zero width
    /// owns no storage or budget handle. The returned table has no written rows.
    pub fn charged(
        width: usize,
        rows: usize,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        if width == 0 {
            return Ok(Self::rows_only(0));
        }
        let header = std::alloc::Layout::array::<Vec<u64>>(width)
            .map_err(|_| budget.refusal(usize::MAX))?
            .size();
        let bytes = Self::bytes(width, rows)
            .and_then(|payload| payload.checked_add(header))
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        // The existing table owner is in place before either header or payload exists.
        // One combined admission, one Arc, no intermediate columns or payload copy.
        let mut table = Self::rows_only(0);
        table.charge_ahead(bytes, budget)?;
        table.header = header;
        table.columns = Vec::with_capacity(width);
        table
            .columns
            .resize_with(width, || Vec::with_capacity(rows));
        table.room = rows;
        Ok(table)
    }

    /// The bytes owned by the table, including its outer column header; 0 for a zero-width
    /// table or a legacy unbudgeted table that has not been settled or grown.
    pub fn charge(&self) -> usize {
        self.held.as_ref().map_or(0, Reservation::bytes)
    }

    /// Room for `rows` more rows, charged to the table's budget (`budget` for a table that
    /// holds no charge yet) before the columns grow: their capacity doubles where the budget
    /// has room for it, else grows by what is asked. Fails without growing.
    pub fn reserve_rows(
        &mut self,
        rows: usize,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        self.reserve_rows_up_to(rows, usize::MAX, budget)
    }

    /// [`Self::reserve_rows`], doubling to no more than `most` rows in all (a LIMIT's).
    pub fn reserve_rows_up_to(
        &mut self,
        rows: usize,
        most: usize,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        let needed = self.needed(rows, budget)?;
        if needed <= self.room {
            return Ok(());
        }
        let doubled = needed.max(self.room.saturating_mul(2).min(most));
        let fits = Self::bytes(self.width(), doubled).is_some_and(|bytes| {
            let budget = self.held.as_ref().map_or(&**budget, |held| held.budget());
            bytes <= budget.remaining()
        });
        self.set_room(if fits { doubled } else { needed }, budget)
    }

    /// `bytes` more charged to the table's budget (`budget` for a table that holds no
    /// charge yet), before a change made outside the charged path allocates them (a
    /// column pushed, columns projected again): [`Self::settle`] then holds exactly what
    /// was made. Fails without charging.
    pub fn charge_ahead(
        &mut self,
        bytes: usize,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        self.held
            .get_or_insert_with(|| Reservation::new(Arc::clone(budget)))
            .charge(bytes)
    }

    /// Room for exactly `rows` more rows, charged first ([`Self::reserve_rows`]).
    pub fn reserve_rows_exact(
        &mut self,
        rows: usize,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        let needed = self.needed(rows, budget)?;
        match needed <= self.room {
            true => Ok(()),
            false => self.set_room(needed, budget),
        }
    }

    /// The rows with `rows` more, if they can be counted.
    fn needed(&self, rows: usize, budget: &Budget) -> Result<usize, BudgetExceeded> {
        self.len
            .checked_add(rows)
            .ok_or_else(|| budget.refusal(usize::MAX))
    }

    /// The bytes of `rows` rows `width` wide, if a table can hold them.
    fn bytes(width: usize, rows: usize) -> Option<usize> {
        let column = std::alloc::Layout::array::<u64>(rows).ok()?.size();
        column.checked_mul(width)
    }

    /// Grows only columns below `rows`: admit their new allocations together,
    /// retaining the unchanged header and larger columns, then release the replaced
    /// capacities after allocation. Keep the existing token and measurement origin.
    fn set_room(&mut self, rows: usize, budget: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        if self.width() == 0 {
            self.room = usize::MAX;
            return Ok(());
        }
        let header = self.columns.capacity() * size_of::<Vec<u64>>();
        let column_bytes = std::alloc::Layout::array::<u64>(rows)
            .map_err(|_| budget.refusal(usize::MAX))?
            .size();
        let mut retained = header;
        let mut new = 0usize;
        let mut replaced = 0usize;
        for column in &self.columns {
            let old = column.capacity() * size_of::<u64>();
            retained = retained
                .checked_add(old)
                .ok_or_else(|| budget.refusal(usize::MAX))?;
            if column.capacity() < rows {
                new = new
                    .checked_add(column_bytes)
                    .ok_or_else(|| budget.refusal(usize::MAX))?;
                replaced += old;
            }
        }
        let owned = self.held.as_ref().map_or(0, Reservation::bytes);
        // Legacy unbudgeted storage is settled at this owner as before. A charged
        // table already owns `retained`, so its one admission is only the new blocks.
        let more = new
            .checked_add(retained.saturating_sub(owned))
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        if let Some(held) = &mut self.held {
            held.charge(more)?;
        } else {
            let mut held = Reservation::new(Arc::clone(budget));
            held.charge(more)?;
            self.held = Some(held);
        }
        for column in &mut self.columns {
            if column.capacity() < rows {
                column.reserve_exact(rows - column.len());
            }
        }
        // Pending charge_ahead/projection slack owns storage already freed, not an
        // unchanged live column. Settle that existing surplus along with old blocks.
        self.held
            .as_mut()
            .unwrap()
            .release(replaced + owned.saturating_sub(retained));
        self.header = header;
        self.room = rows;
        debug_assert_eq!(self.capacity_rows(), rows);
        debug_assert_eq!(self.charge(), self.memory_bytes());
        Ok(())
    }

    /// Appends `row` within the room reserved ([`Self::reserve_rows`]): one check, then the
    /// values written. Panics past the room.
    #[inline]
    pub fn push_reserved(&mut self, row: &[u64]) {
        assert!(self.len < self.room, "past the rows reserved");
        debug_assert_eq!(row.len(), self.width(), "row width");
        for (column, &value) in self.columns.iter_mut().zip(row) {
            // SAFETY: `room` is at most every column's capacity, and every column holds
            // `len` values.
            unsafe {
                column.as_mut_ptr().add(self.len).write(value);
                column.set_len(self.len + 1);
            }
        }
        self.len += 1;
        self.sorted_by.clear();
    }

    /// The rows free in the reserved room.
    pub fn room_left(&self) -> usize {
        self.room - self.len
    }

    /// A table of `rows` rows written a column at a time, `columns` yielding each column's
    /// values (at most `rows` are taken): the room charged to `budget` first.
    pub fn gathered<C: IntoIterator<Item = u64>>(
        width: usize,
        rows: usize,
        budget: &Arc<Budget>,
        columns: impl IntoIterator<Item = C>,
    ) -> Result<Self, BudgetExceeded> {
        let mut table = Self::charged(width, rows, budget)?;
        let mut written = 0;
        for (column, values) in table.columns.iter_mut().zip(columns) {
            // Into room for `rows`: the column never grows.
            column.extend(values.into_iter().take(rows));
            assert_eq!(column.len(), rows, "a column of every row");
            written += 1;
        }
        assert_eq!(written, width, "every column");
        table.len = rows;
        debug_assert!(table.capacity_rows() >= table.room);
        Ok(table)
    }

    /// The rows whose `mask` entry is `keep`, copied into a table charged to `budget`.
    pub fn selected(
        &self,
        mask: &[bool],
        keep: bool,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        assert_eq!(mask.len(), self.len, "mask length");
        let rows = mask.iter().filter(|&&m| m == keep).count();
        let mut table = Self::gathered(
            self.width(),
            rows,
            budget,
            self.columns.iter().map(|column| {
                column
                    .iter()
                    .zip(mask)
                    .filter(move |&(_, &m)| m == keep)
                    .map(|(&value, _)| value)
            }),
        )?;
        table.sorted_by = self.sorted_by.clone();
        Ok(table)
    }

    /// A copy charged to `budget` (`Clone` makes an uncharged one).
    pub fn try_clone(&self, budget: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let mut table = Self::gathered(
            self.width(),
            self.len,
            budget,
            self.columns.iter().map(|column| column.iter().copied()),
        )?;
        table.sorted_by = self.sorted_by.clone();
        Ok(table)
    }

    /// The rows of `parts` in order, in one table `width` wide, charged to `budget`: the
    /// first part with rows grows (charged first) to hold every row, and each other part is
    /// taken from `parts` and freed, its charge released, as soon as it is copied. Parts
    /// without rows are skipped.
    pub fn concat_charged(
        width: usize,
        parts: &mut [IdTable],
        budget: &Arc<Budget>,
    ) -> Result<IdTable, BudgetExceeded> {
        let Some(first) = parts.iter().position(|part| !part.is_empty()) else {
            return IdTable::charged(width, 0, budget);
        };
        let rest = &mut parts[first..];
        let extra = rest[1..].iter().map(IdTable::len).sum();
        let mut out = std::mem::take(&mut rest[0]);
        out.reserve_rows_exact(extra, budget)?;
        Ok(out.append_parts(rest[1..].iter_mut().map(std::mem::take)))
    }

    /// Holds the charge of exactly its memory ([`Self::memory_bytes`], the vector of
    /// columns included), to `budget` if it holds none yet: what its columns hold beyond
    /// its charge (grown, or made, outside [`Self::reserve_rows`]) is charged now, what it
    /// holds beyond them released. Fails, holding what it held, where the budget has no
    /// room.
    pub fn settle(&mut self, budget: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        if self.held.is_none() && self.columns.capacity() == 0 && self.memory_bytes() == 0 {
            return Ok(());
        }
        self.header = self.columns.capacity() * size_of::<Vec<u64>>();
        let bytes = self.memory_bytes();
        self.held
            .get_or_insert_with(|| Reservation::new(Arc::clone(budget)))
            .resize(bytes)?;
        self.room = self.capacity_rows();
        Ok(())
    }

    /// A table from equal-length columns, with the charge they come with ([`Columns`]; a
    /// `Vec` of columns comes with none).
    ///
    /// # Panics
    /// If the columns differ in length.
    pub fn from_columns(columns: Columns) -> Self {
        let Columns {
            columns,
            rows,
            header,
            held,
        } = columns;
        assert!(
            columns.iter().all(|c| c.len() == rows),
            "IdTable columns must have equal lengths"
        );
        let mut table = Self::uncharged(columns, rows, Vec::new());
        (table.header, table.held) = (header, held);
        table
    }

    /// A table with one row per entry of `rows`, each `width` wide.
    /// A table of columns another component made (an engine scan's decoded block), charged
    /// as they arrive: the one way a table's memory is charged after it is allocated, for
    /// memory the engine allocates and owns until it hands it over (its scan of a pattern the
    /// planner chose, bounded by that pattern's matches). Fails, freeing them, where the
    /// budget has no room for them.
    pub fn adopted(columns: Vec<Vec<u64>>, budget: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let rows = columns.first().map_or(0, Vec::len);
        let mut table = Self::from_columns(Columns {
            columns,
            rows,
            header: 0,
            held: None,
        });
        table.settle(budget)?;
        Ok(table)
    }

    /// A table of `rows` rows without columns: it holds no memory (the executor's tables of
    /// a pattern without variables, or of a projection to none).
    pub fn rows_only(rows: usize) -> Self {
        Self::uncharged(Vec::new(), rows, Vec::new())
    }

    /// A table with one row per entry of `rows`, each `width` wide, charged to `budget` as it
    /// grows ([`Self::push_row`]).
    pub fn from_rows<'a>(
        width: usize,
        rows: impl IntoIterator<Item = &'a [u64]>,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        // Rows without columns: only their number (the executor's tables of a pattern
        // without variables, or of a projection to none).
        if width == 0 {
            return Ok(Self::uncharged(
                Vec::new(),
                rows.into_iter().count(),
                Vec::new(),
            ));
        }
        let mut rows = rows.into_iter();
        let Some(first) = rows.next() else {
            return Self::charged(width, 0, budget);
        };
        // Match the former first-row capacity, with one combined admission before the
        // header or payload is allocated. Pulling the first borrowed row copies no data.
        let mut table = Self::charged(width, 1, budget)?;
        table.push_reserved(first);
        for row in rows {
            table.push_row(row, budget)?;
        }
        Ok(table)
    }

    pub fn width(&self) -> usize {
        self.columns.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn column(&self, index: usize) -> &[u64] {
        &self.columns[index]
    }

    pub fn columns(&self) -> &[Vec<u64>] {
        &self.columns
    }

    /// The columns, with the table's charge: it stays with them, into the table they make
    /// ([`Self::from_columns`]), or is released when they are dropped, after them.
    pub fn into_columns(mut self) -> Columns {
        Columns {
            columns: std::mem::take(&mut self.columns),
            rows: self.len,
            header: self.header,
            held: self.held.take(),
        }
    }

    #[inline]
    pub fn get(&self, row: usize, column: usize) -> u64 {
        self.columns[column][row]
    }

    /// Row `row` as a vector; for tests and slow paths. Hot loops read columns.
    pub fn row(&self, row: usize) -> Vec<u64> {
        self.columns.iter().map(|c| c[row]).collect()
    }

    /// Iterates rows as vectors; for tests and slow paths.
    pub fn rows(&self) -> impl Iterator<Item = Vec<u64>> + '_ {
        (0..self.len).map(|row| self.row(row))
    }

    /// Appends `row`, room for it charged to the table's budget (`budget` for a table that
    /// holds no charge yet) first where none is left ([`Self::reserve_rows`]). Fails
    /// without changing the table.
    #[inline]
    pub fn push_row(&mut self, row: &[u64], budget: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        if self.len == self.room {
            self.reserve_rows(1, budget)?;
        }
        self.push_reserved(row);
        Ok(())
    }

    /// Adds a column of `values`, one per row (at most the rows are taken), charged first:
    /// the column, with room for as many rows as the others, and the vector of columns
    /// where it grows (whole: [`Self::settle`] then releases the old one's charge).
    pub fn push_column(
        &mut self,
        values: impl IntoIterator<Item = u64>,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        // As many rows as the other columns have room for (the rows, for a table without
        // columns), so that the room stays what it is.
        let rows = match self.columns.is_empty() {
            true => self.len,
            false => self.room,
        };
        let grows = self.columns.len() == self.columns.capacity();
        let header = match grows {
            true => (self.columns.capacity() * 2).max(4) * size_of::<Vec<u64>>(),
            false => 0,
        };
        let bytes = Self::bytes(1, rows)
            .and_then(|column| column.checked_add(header))
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        self.charge_ahead(bytes, budget)?;
        let mut column = Vec::with_capacity(rows);
        column.extend(values.into_iter().take(self.len));
        assert_eq!(column.len(), self.len, "push_column: rows differ");
        self.columns.push(column);
        Ok(())
    }

    /// Appends all rows of `other`, which must have the same width, room for them charged
    /// first ([`Self::reserve_rows`]). Sortedness is lost. Fails without changing the table.
    pub fn append(&mut self, other: &IdTable, budget: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        assert_eq!(self.width(), other.width(), "append: widths differ");
        self.reserve_rows(other.len, budget)?;
        self.extend_within(other);
        Ok(())
    }

    /// Appends all rows of `other` into the room reserved for them.
    fn extend_within(&mut self, other: &IdTable) {
        debug_assert!(other.len <= self.room_left(), "past the rows reserved");
        for (column, extra) in self.columns.iter_mut().zip(&other.columns) {
            column.extend_from_slice(extra);
        }
        self.len += other.len;
        self.sorted_by.clear();
    }

    /// `rest` appended to `self`, which has room for them, each freed once copied.
    fn append_parts(self, rest: impl IntoIterator<Item = IdTable>) -> IdTable {
        let mut out = self;
        // Sorted on what every part is sorted on, where each part starts at or after the
        // row before it (the morsels of a sorted scan, in order).
        let mut sorted = std::mem::take(&mut out.sorted_by);
        // Each part is freed as soon as it is copied: together with the output they
        // would hold the rows twice.
        for part in rest {
            if part.is_empty() {
                continue;
            }
            let common = sorted
                .iter()
                .zip(&part.sorted_by)
                .take_while(|(a, b)| a == b)
                .count();
            sorted.truncate(common);
            if !sorted.is_empty() && out.len > 0 && part.len > 0 {
                let before = sorted.iter().map(|&c| out.columns[c][out.len - 1]);
                let after = sorted.iter().map(|&c| part.columns[c][0]);
                if before.cmp(after).is_gt() {
                    sorted.clear();
                }
            }
            out.extend_within(&part);
        }
        out.sorted_by = sorted;
        out
    }

    /// Bytes held by the column buffers (by capacity), and by the vector of them once the
    /// table was charged, for memory budgets.
    pub fn memory_bytes(&self) -> usize {
        self.header + self.columns.iter().map(|c| c.capacity() * 8).sum::<usize>()
    }

    /// The columns the table is known to be sorted on, most significant first.
    pub fn sorted_by(&self) -> &[usize] {
        &self.sorted_by
    }

    /// True if the table is known to be sorted on `keys` (a prefix of its sort order).
    pub fn is_sorted_on(&self, keys: &[usize]) -> bool {
        keys.is_empty() || self.len <= 1 || self.sorted_by.starts_with(keys)
    }

    /// Records that the table is sorted on `keys`. The caller guarantees it; debug builds
    /// check.
    pub fn assume_sorted_by(mut self, keys: Vec<usize>) -> Self {
        debug_assert!(self.check_sorted(&keys), "table is not sorted by {keys:?}");
        self.sorted_by = keys;
        self
    }

    fn check_sorted(&self, keys: &[usize]) -> bool {
        (1..self.len)
            .all(|row| self.compare_rows(row - 1, row, keys) != std::cmp::Ordering::Greater)
    }

    #[inline]
    fn compare_rows(&self, a: usize, b: usize, keys: &[usize]) -> std::cmp::Ordering {
        for &key in keys {
            let column = &self.columns[key];
            match column[a].cmp(&column[b]) {
                std::cmp::Ordering::Equal => continue,
                other => return other,
            }
        }
        std::cmp::Ordering::Equal
    }

    /// The table's charge moved to `budget` (a holder that takes the table over under a
    /// budget of its own): charged there first, then released where it was. Fails, keeping
    /// its charge, where `budget` has no room.
    pub fn recharge(&mut self, budget: &Arc<Budget>) -> Result<(), BudgetExceeded> {
        if self.held.is_none() && self.memory_bytes() == 0 {
            return Ok(());
        }
        let mut moved = Reservation::new(Arc::clone(budget));
        moved.charge(self.memory_bytes())?;
        // The old charge released as it drops, after the new one is held.
        self.held = Some(moved);
        Ok(())
    }

    /// Working memory of `bytes` for an operation on the table, charged to its budget first
    /// and released when the returned reservation drops (after the memory, declared before
    /// it). None for a table that holds no charge: it holds no memory of a query (empty, or
    /// a copy the result cache keeps and counts).
    fn working(&self, bytes: usize) -> Result<Option<Reservation<Arc<Budget>>>, BudgetExceeded> {
        let Some(held) = &self.held else {
            return Ok(None);
        };
        let mut working = Reservation::new(Arc::clone(held.handle()));
        working.charge(bytes)?;
        Ok(Some(working))
    }

    /// Sorts the rows lexicographically on `keys`, unless already sorted on them. The sort
    /// computes a row permutation once and gathers every column through it, in parallel for
    /// large tables. Its working memory (the permutation, and the columns gathered anew
    /// beside the old ones) is charged to the table's budget first; refused, the table is as
    /// it was.
    pub fn sort_by(&mut self, keys: &[usize]) -> Result<(), BudgetExceeded> {
        if self.is_sorted_on(keys) {
            if self.sorted_by.len() < keys.len() {
                self.sorted_by = keys.to_vec();
            }
            return Ok(());
        }
        let rows = self.len;
        // One key: (value, row) pairs, then the rows; else the rows alone.
        let order = match keys {
            [_] => rows.saturating_mul(size_of::<(u64, u32)>() + size_of::<u32>()),
            _ => rows.saturating_mul(size_of::<u32>()),
        };
        // A column at a time, or all at once in parallel.
        let gathered = match rows >= PARALLEL_SORT_ROWS {
            true => self.width(),
            false => self.width().min(1),
        };
        let _working = self.working(order.saturating_add(rows.saturating_mul(8 * gathered)))?;
        let order = self.sort_order(keys);
        self.gather(&order);
        self.sorted_by = keys.to_vec();
        Ok(())
    }

    /// The row permutation that sorts the table on `keys`.
    fn sort_order(&self, keys: &[usize]) -> Vec<u32> {
        assert!(self.len <= u32::MAX as usize, "IdTable rows exceed u32");
        let parallel = self.len >= PARALLEL_SORT_ROWS;
        if let [key] = keys {
            // One key: sort (value, row) pairs, which is branch-free and cache friendly.
            let column = &self.columns[*key];
            let mut pairs: Vec<(u64, u32)> = column
                .iter()
                .enumerate()
                .map(|(i, &v)| (v, i as u32))
                .collect();
            if parallel {
                pairs.par_sort_unstable();
            } else {
                pairs.sort_unstable();
            }
            return pairs.into_iter().map(|(_, row)| row).collect();
        }
        let mut order: Vec<u32> = (0..self.len as u32).collect();
        let compare = |a: &u32, b: &u32| {
            self.compare_rows(*a as usize, *b as usize, keys)
                .then(a.cmp(b))
        };
        if parallel {
            order.par_sort_unstable_by(compare);
        } else {
            order.sort_unstable_by(compare);
        }
        order
    }

    fn gather(&mut self, order: &[u32]) {
        let gather = |column: &mut Vec<u64>| {
            let sorted: Vec<u64> = order.iter().map(|&row| column[row as usize]).collect();
            *column = sorted;
        };
        if self.len >= PARALLEL_SORT_ROWS {
            self.columns.par_iter_mut().for_each(gather);
        } else {
            self.columns.iter_mut().for_each(gather);
        }
        self.room = self.capacity_rows();
        // The columns gathered anew hold exactly their rows: no more than before.
        let bytes = self.memory_bytes();
        if let Some(held) = &mut self.held {
            held.resize(bytes.min(held.bytes()))
                .expect("releasing refuses nothing");
        }
    }

    /// Keeps the rows for which `keep(row)` is true, preserving order and sortedness. The
    /// mask of the rows is charged to the table's budget first.
    pub fn retain(
        &mut self,
        mut keep: impl FnMut(&IdTable, usize) -> bool,
    ) -> Result<(), BudgetExceeded> {
        let _working = self.working(self.len)?;
        let mask: Vec<bool> = (0..self.len).map(|row| keep(self, row)).collect();
        self.retain_mask(&mask);
        Ok(())
    }

    /// Keeps the rows whose `mask` entry is true, preserving order and sortedness.
    /// [`retain`](Self::retain) with a thread-safe predicate, evaluated in parallel for
    /// large tables.
    pub fn par_retain(
        &mut self,
        keep: impl Fn(&IdTable, usize) -> bool + Sync,
    ) -> Result<(), BudgetExceeded> {
        let _working = self.working(self.len)?;
        let mask: Vec<bool> = if self.len < PARALLEL_SORT_ROWS {
            (0..self.len).map(|row| keep(self, row)).collect()
        } else {
            (0..self.len)
                .into_par_iter()
                .map(|row| keep(self, row))
                .collect()
        };
        self.retain_mask(&mask);
        Ok(())
    }

    pub fn retain_mask(&mut self, mask: &[bool]) {
        assert_eq!(mask.len(), self.len, "mask length");
        let filter = |column: &mut Vec<u64>| {
            let mut keep = mask.iter();
            column.retain(|_| *keep.next().unwrap());
        };
        if self.len < PARALLEL_SORT_ROWS {
            self.columns.iter_mut().for_each(filter);
        } else {
            self.columns.par_iter_mut().for_each(filter);
        }
        self.len = mask.iter().filter(|&&k| k).count();
    }

    /// Removes duplicate rows. Sorts on all columns first unless already sorted on them.
    /// Working memory is charged to the table's budget first ([`Self::sort_by`]).
    pub fn dedup(&mut self) -> Result<(), BudgetExceeded> {
        let all: Vec<usize> = (0..self.width()).collect();
        self.sort_by(&all)?;
        self.dedup_adjacent()
    }

    /// Equal rows are adjacent when any permutation of all columns orders the table.
    fn dedup_adjacent(&mut self) -> Result<(), BudgetExceeded> {
        let _working = self.working(self.len)?;
        let mask: Vec<bool> = (0..self.len)
            .map(|row| {
                row == 0
                    || self
                        .columns
                        .iter()
                        .any(|column| column[row - 1] != column[row])
            })
            .collect();
        self.retain_mask(&mask);
        Ok(())
    }

    /// Removes duplicate rows, keeping the first occurrence of each and the row order
    /// (SPARQL DISTINCT after ORDER BY). Sortedness is kept. The table of rows seen and the
    /// mask are charged to the table's budget first.
    pub fn dedup_preserving_order(&mut self) -> Result<(), BudgetExceeded> {
        if self.width() > 0 && (0..self.width()).all(|column| self.sorted_by.contains(&column)) {
            return self.dedup_adjacent();
        }
        let _working =
            self.working(crate::map_bytes::<usize>(self.len).saturating_add(self.len))?;
        // A table of row numbers, hashed and compared through the columns: no row is
        // copied.
        use std::hash::{BuildHasher, Hasher};
        let state = foldhash::fast::FixedState::default();
        let columns = &self.columns;
        let hash_of = |row: usize| {
            let mut hasher = state.build_hasher();
            for column in columns {
                hasher.write_u64(column[row]);
            }
            hasher.finish()
        };
        let mut seen: hashbrown::HashTable<usize> = hashbrown::HashTable::with_capacity(self.len);
        let mask: Vec<bool> = (0..self.len)
            .map(|row| {
                let hash = hash_of(row);
                let same = |&other: &usize| columns.iter().all(|c| c[other] == c[row]);
                match seen.entry(hash, same, |&other| hash_of(other)) {
                    hashbrown::hash_table::Entry::Occupied(_) => false,
                    hashbrown::hash_table::Entry::Vacant(slot) => {
                        slot.insert(row);
                        true
                    }
                }
            })
            .collect();
        drop(seen);
        self.retain_mask(&mask);
        Ok(())
    }

    /// Keeps the given columns in that order, moving their buffers. Only repeated columns
    /// are copied. O(width + output width * (sort keys + 1)), plus copied repeated columns.
    /// Sortedness is kept for the longest prefix of the sort order that survives.
    pub fn project(mut self, columns: &[usize]) -> IdTable {
        if columns.len() == self.width() && columns.iter().copied().eq(0..self.width()) {
            return self;
        }
        let sorted_by = self
            .sorted_by
            .iter()
            .map_while(|key| columns.iter().position(|c| c == key))
            .collect();
        let mut remaining = vec![0usize; self.width()];
        for &column in columns {
            remaining[column] += 1;
        }
        let projected = columns
            .iter()
            .map(|&column| {
                remaining[column] -= 1;
                if remaining[column] == 0 {
                    std::mem::take(&mut self.columns[column])
                } else {
                    self.columns[column].clone()
                }
            })
            .collect();
        let mut table = Self::uncharged(projected, self.len, sorted_by);
        // The charge stays with the buffers it covers, dropped after them.
        table.held = self.held.take();
        table.header = self.header;
        table
    }

    /// Keeps rows `offset..offset + limit`.
    pub fn slice(&mut self, offset: usize, limit: Option<usize>) {
        let start = offset.min(self.len);
        let end = limit.map_or(self.len, |l| start.saturating_add(l).min(self.len));
        for column in &mut self.columns {
            column.truncate(end);
            column.drain(..start);
        }
        self.len = end - start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails on r4: charged(width, 0) and empty from_rows allocate the header but return
    /// Ok without admission; nonempty from_rows exposes that allocation before refusal.
    /// Exact headroom also rejects a fix that charges the header twice at first growth.
    #[test]
    fn charged_tables_admit_headers_before_allocation() {
        if !crate::measure::alone() {
            return;
        }
        let width = 2;
        let header = width * size_of::<Vec<u64>>();
        let payload = width * size_of::<u64>();
        for form in 0..4 {
            let budget = Arc::new(Budget::new(13 + header - 1));
            let mut unrelated = Reservation::new(&*budget);
            unrelated.charge(13).unwrap();
            let (result, excess) = crate::measure::measure_within(&budget, || match form {
                0 => IdTable::charged(width, 0, &budget),
                1 => IdTable::charged(width, 1, &budget),
                2 => IdTable::from_rows(width, std::iter::empty(), &budget),
                _ => IdTable::from_rows(width, [&[7, 11][..]], &budget),
            });
            let error = result.expect_err("header refused before allocation, even with no rows");
            let requested = header + if form == 1 || form == 3 { payload } else { 0 };
            assert_eq!(
                (error.requested, error.used, error.shared),
                (requested, 13, false)
            );
            assert_eq!(
                (excess, budget.used(), Arc::strong_count(&budget)),
                (0, 13, 1)
            );
        }
        for form in 0..3 {
            let budget = Arc::new(Budget::new(13 + header + payload));
            let mut unrelated = Reservation::new(&*budget);
            unrelated.charge(13).unwrap();
            let ((), excess) = crate::measure::measure_within(&budget, || {
                let mut table = match form {
                    0 => IdTable::charged(width, 0, &budget).unwrap(),
                    1 => IdTable::charged(width, 1, &budget).unwrap(),
                    _ => IdTable::from_rows(width, [&[7, 11][..]], &budget).unwrap(),
                };
                if form == 0 {
                    assert_eq!(
                        (table.len(), table.charge(), budget.used()),
                        (0, header, 13 + header)
                    );
                }
                let columns = table.columns.as_ptr();
                if form != 2 {
                    table.push_row(&[7, 11], &budget).unwrap();
                }
                assert_eq!(
                    table.columns.as_ptr(),
                    columns,
                    "first growth reuses its admitted header"
                );
                assert_eq!(
                    (table.len(), table.charge(), budget.used()),
                    (1, header + payload, 13 + header + payload)
                );
                assert_eq!((table.get(0, 0), table.get(0, 1)), (7, 11));
                assert_eq!(Arc::strong_count(&budget), 2, "one retained handle");
                drop(table);
                assert_eq!(budget.used(), 13, "unrelated owner remains");
            });
            assert_eq!((excess, budget.used()), (0, 13));
        }
        let budget = Arc::new(Budget::new(0));
        let ((), excess) = crate::measure::measure_within(&budget, || {
            assert!(IdTable::charged(usize::MAX, 0, &budget).is_err());
            let empty = IdTable::charged(0, usize::MAX, &budget).unwrap();
            assert_eq!((empty.len(), empty.width(), empty.charge()), (0, 0, 0));
            let rows = IdTable::from_rows(0, std::iter::repeat_n(&[][..], 17), &budget).unwrap();
            assert_eq!((rows.len(), rows.width(), rows.charge()), (17, 0, 0));
            assert_eq!(Arc::strong_count(&budget), 1);
        });
        assert_eq!((excess, budget.used()), (0, 0));
    }

    /// Uniform capacities are the control; a valid 9/1 transfer must keep the
    /// larger column and its charge when only the smaller one grows to two rows.
    #[test]
    fn growth_keeps_unequal_column_capacity_owned() {
        if !crate::measure::alone() {
            return;
        }
        for capacities in [[1, 1], [9, 1]] {
            let budget = Arc::new(Budget::unlimited());
            let ((), excess) = crate::measure::measure_within(&budget, || {
                let inputs = [
                    scratch(&budget, capacities[0], &[7]),
                    scratch(&budget, 1, &[11]),
                ];
                let mut table = IdTable::from_columns(
                    Columns::from_scratch(1, inputs.into_iter(), &budget).unwrap(),
                );
                let header = table.columns.as_ptr();
                let first = table.columns[0].as_ptr();
                table.reserve_rows(1, &budget).unwrap();
                assert_eq!(table.columns.as_ptr(), header, "unchanged header");
                assert_eq!(table.columns[0].capacity(), capacities[0].max(2));
                if capacities[0] > 2 {
                    assert_eq!(table.columns[0].as_ptr(), first, "larger column stays put");
                }
                assert_eq!(table.columns[1].capacity(), 2);
                assert_eq!((table.room, table.len(), table.room_left()), (2, 1, 1));
                assert_eq!((table.column(0), table.column(1)), (&[7][..], &[11][..]));
                assert_eq!(
                    table.charge(),
                    table.memory_bytes(),
                    "actual retained capacities"
                );
                assert_eq!(budget.used(), table.charge());
                table.push_reserved(&[13, 17]);
                assert_eq!(
                    (table.column(0), table.column(1)),
                    (&[7, 13][..], &[11, 17][..])
                );
                drop(table);
                assert_eq!((budget.used(), Arc::strong_count(&budget)), (0, 1));
            });
            assert_eq!(excess, 0, "inputs and token share the measured operation");
            eprintln!("growth capacity control passed: {capacities:?}");
        }
    }

    /// Only the replacement allocation needs headroom: no unchanged header/larger
    /// payload is readmitted. One byte less must refuse without changing the table.
    #[test]
    fn growth_admits_only_replaced_columns_and_refuses_without_change() {
        if !crate::measure::alone() {
            return;
        }
        let header = 2 * size_of::<Vec<u64>>();
        for capacities in [[1, 1], [9, 1]] {
            let old = header + capacities.iter().sum::<usize>() * size_of::<u64>();
            let growing = capacities.iter().filter(|&&capacity| capacity < 2).count();
            let new = growing * 2 * size_of::<u64>();
            let replaced = growing * size_of::<u64>();
            for refuse in [false, true] {
                for exact in [false, true] {
                    let limit = 13 + old + new - usize::from(refuse);
                    let budget = Arc::new(Budget::new(limit));
                    let mut unrelated = Reservation::new(&*budget);
                    unrelated.charge(13).unwrap();
                    let ((), excess) = crate::measure::measure_within(&budget, || {
                        let inputs = [
                            scratch(&budget, capacities[0], &[7]),
                            scratch(&budget, 1, &[11]),
                        ];
                        let mut table = IdTable::from_columns(
                            Columns::from_scratch(1, inputs.into_iter(), &budget).unwrap(),
                        );
                        let outer = table.columns.as_ptr();
                        let pointers = [table.columns[0].as_ptr(), table.columns[1].as_ptr()];
                        assert_eq!(
                            (table.header, table.charge(), budget.used()),
                            (header, old, 13 + old)
                        );
                        let result = if exact {
                            table.reserve_rows_exact(1, &budget)
                        } else {
                            table.reserve_rows(1, &budget)
                        };
                        if refuse {
                            let error = result.expect_err("replacement must fit before allocation");
                            assert_eq!(
                                (error.requested, error.used, error.limit, error.shared),
                                (new, 13 + old, limit, false)
                            );
                            assert_eq!(
                                (table.room, table.len(), table.charge(), table.header),
                                (1, 1, old, header)
                            );
                            for column in 0..2 {
                                assert_eq!(
                                    (
                                        table.columns[column].as_ptr(),
                                        table.columns[column].capacity()
                                    ),
                                    (pointers[column], capacities[column])
                                );
                            }
                            assert_eq!((budget.used(), budget.peak()), (13 + old, 13 + old));
                        } else {
                            result.expect("exact replacement headroom is enough");
                            assert_eq!((table.room, table.len()), (2, 1));
                            assert_eq!(table.charge(), old + new - replaced);
                            assert_eq!(table.charge(), table.memory_bytes());
                            assert_eq!(
                                (budget.used(), budget.peak()),
                                (13 + table.charge(), limit)
                            );
                            if capacities[0] > 2 {
                                assert_eq!(table.columns[0].as_ptr(), pointers[0]);
                            }
                        }
                        assert_eq!(table.columns.as_ptr(), outer);
                        assert_eq!((table.column(0), table.column(1)), (&[7][..], &[11][..]));
                        assert_eq!(Arc::strong_count(&budget), 2, "original shared handle");
                        drop(table);
                        assert_eq!((budget.used(), Arc::strong_count(&budget)), (13, 1));
                    });
                    assert_eq!(
                        excess, 0,
                        "all payload/header allocations strictly admitted"
                    );
                    drop(unrelated);
                    assert_eq!(budget.used(), 0);
                }
            }
        }
    }

    /// Columns may carry old outer-header slack until the table is settled. Growth
    /// must keep its original token yet finish owning exactly the live allocations.
    #[test]
    fn growth_settles_existing_pending_header_charge() {
        if !crate::measure::alone() {
            return;
        }
        let budget = Arc::new(Budget::unlimited());
        let ((), excess) = crate::measure::measure_within(&budget, || {
            let mut columns = Columns::new(1);
            for value in 0..5 {
                columns.push([value], &budget).unwrap();
            }
            let mut table = IdTable::from_columns(columns);
            let header = table.columns.capacity() * size_of::<Vec<u64>>();
            assert!(table.charge() > header + 5 * size_of::<u64>());
            let pointer = table.columns.as_ptr();
            table.reserve_rows_exact(1, &budget).unwrap();
            assert_eq!(table.columns.as_ptr(), pointer);
            assert_eq!(table.header, header);
            assert_eq!(
                (table.charge(), budget.used()),
                (
                    header + 10 * size_of::<u64>(),
                    header + 10 * size_of::<u64>()
                )
            );
            assert_eq!(table.charge(), table.memory_bytes());
            for column in 0..5 {
                assert_eq!(table.column(column), &[column as u64]);
            }
            drop(table);
            assert_eq!((budget.used(), Arc::strong_count(&budget)), (0, 1));
        });
        assert_eq!(excess, 0);
    }

    fn scratch<'b>(budget: &'b Budget, capacity: usize, values: &[u64]) -> Scratch<'b, u64> {
        let mut column = Scratch::with_capacity(budget, capacity).unwrap();
        column.extend_from_slice(values).unwrap();
        column
    }

    /// The exact limit includes both old and new outer headers. A copy, second payload
    /// admission, length-based charge or extra retained budget handle fails these checks.
    #[test]
    fn scratch_columns_move_with_their_capacity_and_one_shared_handle() {
        if !crate::measure::alone() {
            return;
        }
        for (rows, capacities) in [(3, [9, 5]), (0, [9, 5]), (0, [0, 0])] {
            let header = 2 * size_of::<Vec<u64>>();
            let old_header = 2 * size_of::<Scratch<'_, u64>>();
            let payload = capacities.iter().sum::<usize>() * size_of::<u64>();
            let limit = 13 + old_header + payload + header;
            let budget = Arc::new(Budget::new(limit));
            let mut unrelated = Reservation::new(&*budget);
            unrelated.charge(13).unwrap();
            let ((), excess) = crate::measure::measure_within(&budget, || {
                let mut first = scratch(&budget, capacities[0], &[]);
                let mut second = scratch(&budget, capacities[1], &[]);
                if capacities[0] != 0 {
                    first.extend_from_slice(&[10, 11, 12, 13]).unwrap();
                    second.extend_from_slice(&[20, 21, 22]).unwrap();
                }
                first.truncate(rows);
                if rows == 0 {
                    second.clear();
                }
                let pointers = [first.as_ptr(), second.as_ptr()];
                // This is the caller's separately charged old Vec<Scratch> allocation.
                let (source, working) = Scratch::collect(&*budget, 2, [first, second])
                    .unwrap()
                    .into_parts();
                let source = source.into_iter().inspect(|_| {
                    assert_eq!(Arc::strong_count(&budget), 1, "promote only at handoff");
                });
                let columns = Columns::from_scratch(rows, source, &budget).unwrap();
                assert_eq!(Arc::strong_count(&budget), 2);
                assert_eq!((budget.used(), budget.peak()), (limit, limit));
                assert_eq!(columns.header, header);
                for i in 0..2 {
                    assert_eq!(
                        (columns[i].as_ptr(), columns[i].capacity()),
                        (pointers[i], capacities[i])
                    );
                }
                // The iterator freed its old header; only now may the caller release it.
                drop(working);
                let table = IdTable::from_columns(columns);
                assert_eq!((table.len(), table.width()), (rows, 2));
                assert_eq!(table.charge(), payload + header);
                assert_eq!(budget.used(), 13 + table.charge());
                if rows != 0 {
                    assert_eq!(
                        (table.column(0), table.column(1)),
                        (&[10, 11, 12][..], &[20, 21, 22][..])
                    );
                }
                drop(table);
                assert_eq!((budget.used(), Arc::strong_count(&budget)), (13, 1));
            });
            assert_eq!(excess, 0, "every allocation and release stays covered");
            drop(unrelated);
            assert_eq!(budget.used(), 0);
        }
    }

    /// Inputs predate measurement: any new header allocated before its refusal is an
    /// excess, even if it is immediately freed. Missing admission also loses the Err.
    #[test]
    fn scratch_columns_refuse_the_header_before_allocating_it() {
        if !crate::measure::alone() {
            return;
        }
        let header = 2 * size_of::<Vec<u64>>();
        let payload = (8 + 4) * size_of::<u64>();
        let budget = Arc::new(Budget::new(13 + payload + header - 1));
        let mut unrelated = Reservation::new(&*budget);
        unrelated.charge(13).unwrap();
        let inputs = [scratch(&budget, 8, &[1, 2]), scratch(&budget, 4, &[3, 4])];
        let (result, excess) = crate::measure::measure_within(&budget, || {
            Columns::from_scratch(2, inputs.into_iter(), &budget)
        });
        let error: BudgetExceeded = result.unwrap_err();
        assert_eq!(
            (error.requested, error.used, error.limit, error.shared),
            (header, 13 + payload, budget.limit(), false)
        );
        assert_eq!(
            (excess, budget.used(), Arc::strong_count(&budget)),
            (0, 13, 1)
        );
        drop(unrelated);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn zero_scratch_columns_keep_row_multiplicity_without_a_budget_handle() {
        let budget = Arc::new(Budget::new(0));
        let columns = Columns::from_scratch(17, std::iter::empty(), &budget).unwrap();
        assert_eq!((columns.header, columns.held.is_none()), (0, true));
        let table = IdTable::from_columns(columns);
        assert_eq!((table.len(), table.width(), table.charge()), (17, 0, 0));
        assert_eq!((budget.used(), Arc::strong_count(&budget)), (0, 1));
    }

    /// Check both the first source and a later source, and distinguish sibling parts of
    /// one Envelope. A failed validation must leave only the unrelated live tokens.
    #[test]
    fn scratch_columns_reject_wrong_budgets_and_row_lengths_with_balanced_cleanup() {
        let envelope = crate::Envelope::unlimited();
        let budget = Arc::new(envelope.part());
        let sibling = Arc::new(envelope.part());
        let mut unrelated = Reservation::new(&*budget);
        unrelated.charge(13).unwrap();
        let mut theirs = Reservation::new(&*sibling);
        theirs.charge(7).unwrap();
        for wrong in 0..4 {
            let inputs = [
                scratch(
                    if wrong == 0 { &sibling } else { &budget },
                    4,
                    if wrong == 2 { &[1][..] } else { &[1, 2][..] },
                ),
                scratch(
                    if wrong == 1 { &sibling } else { &budget },
                    5,
                    if wrong == 3 { &[3][..] } else { &[3, 4][..] },
                ),
            ];
            let failed =
                std::panic::catch_unwind(|| Columns::from_scratch(2, inputs.into_iter(), &budget));
            assert!(failed.is_err());
            assert_eq!(
                (budget.used(), sibling.used(), envelope.used()),
                (13, 7, 20)
            );
            assert_eq!(
                (Arc::strong_count(&budget), Arc::strong_count(&sibling)),
                (1, 1)
            );
        }
        drop((unrelated, theirs));
        assert_eq!(envelope.used(), 0);
    }

    /// Same Budget is insufficient: absorb's general retagging semantics would accept
    /// this batch. Test both old/new orders, including cleanup after a partial merge.
    #[test]
    fn scratch_columns_reject_mixed_measurement_origins() {
        if !crate::measure::alone() {
            return;
        }
        let budget = Arc::new(Budget::unlimited());
        let mut unrelated = Reservation::new(&*budget);
        unrelated.charge(13).unwrap();
        for old_first in [true, false] {
            let old = [scratch(&budget, 4, &[1, 2]), scratch(&budget, 6, &[1, 2])];
            let failed = std::panic::catch_unwind(|| {
                crate::measure::measure_within(&budget, || {
                    let new = [scratch(&budget, 5, &[3, 4]), scratch(&budget, 7, &[3, 4])];
                    let inputs = if old_first {
                        let [a, b] = old;
                        let [c, unused] = new;
                        drop(unused);
                        [a, b, c]
                    } else {
                        let [a, b] = new;
                        let [c, unused] = old;
                        drop(unused);
                        [a, b, c]
                    };
                    Columns::from_scratch(2, inputs.into_iter(), &budget)
                })
            });
            assert!(failed.is_err(), "origins must be checked before absorption");
            assert_eq!((budget.used(), Arc::strong_count(&budget)), (13, 1));
        }
        drop(unrelated);
        assert_eq!(budget.used(), 0);
    }

    /// A panic after two moves exposes releasing the aggregate token before its payload.
    /// A separate panic-only measurement identifies the runtime's exception allocation:
    /// require exactly that excess, so even a brief early release adds a failing excess.
    #[test]
    fn a_partial_scratch_iterator_panic_frees_payload_before_releasing_tokens() {
        if !crate::measure::alone() {
            return;
        }
        let budget = Arc::new(Budget::unlimited());
        let mut unrelated = Reservation::new(&*budget);
        unrelated.charge(13).unwrap();
        let (baseline, panic_excess) = crate::measure::measure_within(&budget, || {
            std::panic::catch_unwind(|| {
                std::panic::resume_unwind(Box::new(()));
            })
        });
        assert!(baseline.is_err());
        drop(baseline);
        let (failed, excess) = crate::measure::measure_within(&budget, || {
            std::panic::catch_unwind(|| {
                let inputs =
                    [1024, 2048, 3072, 4096].map(|capacity| scratch(&budget, capacity, &[1]));
                let columns = inputs.into_iter().enumerate().map(|(i, column)| {
                    if i == 2 {
                        // Skip the panic hook and its text allocation. The unwinder's
                        // exception allocation is identified by the separate baseline.
                        std::panic::resume_unwind(Box::new(()));
                    }
                    column
                });
                Columns::from_scratch(1, columns, &budget)
            })
        });
        assert!(failed.is_err());
        drop(failed);
        assert_eq!(excess, panic_excess, "no additional excess during cleanup");
        assert_eq!((budget.used(), Arc::strong_count(&budget)), (13, 1));
        drop(unrelated);
        assert_eq!(budget.used(), 0);
    }

    /// A forged width cannot trigger implicit header growth, skip owned inputs or turn
    /// an overflowing layout into an allocator panic instead of a typed refusal.
    #[test]
    fn scratch_columns_check_the_exact_width_and_refuse_impossible_headers() {
        struct Width<I>(I, usize);
        impl<I: Iterator> Iterator for Width<I> {
            type Item = I::Item;

            fn next(&mut self) -> Option<Self::Item> {
                self.0.next()
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                (self.1, Some(self.1))
            }
        }
        impl<I: Iterator> ExactSizeIterator for Width<I> {}

        let budget = Arc::new(Budget::unlimited());
        let mut unrelated = Reservation::new(&*budget);
        unrelated.charge(13).unwrap();
        for width in [0, 1, 4, usize::MAX] {
            let inputs = [4, 5, 6].map(|capacity| scratch(&budget, capacity, &[1]));
            let result = std::panic::catch_unwind(|| {
                Columns::from_scratch(1, Width(inputs.into_iter(), width), &budget)
            });
            if width == usize::MAX {
                let error: BudgetExceeded = result.unwrap().unwrap_err();
                assert_eq!(error.requested, usize::MAX);
            } else {
                assert!(
                    result.is_err(),
                    "the iterator must match its declared width"
                );
            }
            assert_eq!((budget.used(), Arc::strong_count(&budget)), (13, 1));
        }
        drop(unrelated);
        assert_eq!(budget.used(), 0);
    }

    /// A table built through the charged path holds no more than it charges, however many
    /// rows: its growth, copies, selections and concatenations, and the working memory of
    /// its sorts, deduplications and filters are charged before they allocate, and released
    /// after what they cover is freed.
    #[test]
    fn a_charged_table_holds_no_more_than_it_charges() {
        if !crate::measure::alone() {
            return;
        }
        let held = |rows: usize| {
            let budget = Arc::new(Budget::unlimited());
            let mut odd = crate::ChargedVec::new(&*budget);
            odd.extend_bounded(rows, (0..rows).map(|row| row % 2 == 1))
                .unwrap();
            let mut parts = [IdTable::new(3), IdTable::new(3), IdTable::new(3)];
            let ((), excess) = crate::measure::measure_within(&budget, || {
                let table = &mut parts[0];
                for i in 0..rows as u64 {
                    if table.room_left() == 0 {
                        table.reserve_rows(1, &budget).unwrap();
                    }
                    table.push_reserved(&[i, i, i]);
                }
                parts[1] = parts[0].try_clone(&budget).unwrap();
                parts[2] = parts[1].selected(&odd, true, &budget).unwrap();
                let mut all = IdTable::concat_charged(3, &mut parts, &budget).unwrap();
                assert_eq!(all.len(), rows * 5 / 2);
                // Its working memory: a sort on one key (pairs) and on all (the rows'
                // order), the columns gathered anew, the rows seen, and masks.
                all.sort_by(&[2]).unwrap();
                all.dedup_preserving_order().unwrap();
                all.retain(|table, row| table.get(row, 0) % 3 != 0).unwrap();
                all.dedup().unwrap();
                assert_eq!(budget.used(), odd.charged() + all.charge());
            });
            drop(odd);
            assert_eq!(budget.used(), 0);
            excess
        };
        assert_eq!((held(10), held(100_000)), (0, 0));
    }

    fn table(rows: &[&[u64]]) -> IdTable {
        IdTable::from_rows(
            rows.first().map_or(0, |r| r.len()),
            rows.iter().copied(),
            &crate::test_budget(),
        )
        .unwrap()
    }

    /// Concatenated parts keep the sort order they share where each starts at or after the
    /// row before it, and lose it where one doesn't.
    #[test]
    fn concatenation_keeps_a_shared_order_across_ordered_parts() {
        let a = table(&[&[1, 9], &[2, 8]]).assume_sorted_by(vec![0, 1]);
        let b = table(&[&[2, 9], &[3, 1]]).assume_sorted_by(vec![0]);
        let concat = |parts: &mut [IdTable]| {
            IdTable::concat_charged(2, parts, &crate::test_budget()).unwrap()
        };
        let joined = concat(&mut [a.clone(), b.clone()]);
        assert_eq!(joined.sorted_by(), [0]);
        assert_eq!(joined.len(), 4);
        // Out of order at the boundary: no order.
        let joined = concat(&mut [b, a]);
        assert!(joined.sorted_by().is_empty());
        // An empty part in between changes nothing.
        let c = table(&[&[1, 1]]).assume_sorted_by(vec![0]);
        let d = table(&[&[5, 0]]).assume_sorted_by(vec![0]);
        let joined = concat(&mut [c, IdTable::new(2).assume_sorted_by(vec![0]), d]);
        assert_eq!(joined.sorted_by(), [0]);
    }

    #[test]
    fn sort_is_lexicographic_and_stable_for_equal_keys() {
        let mut t = table(&[&[3, 1], &[1, 9], &[3, 0], &[1, 2]]);
        t.sort_by(&[0, 1]).unwrap();
        assert_eq!(
            t.rows().collect::<Vec<_>>(),
            vec![vec![1, 2], vec![1, 9], vec![3, 0], vec![3, 1]]
        );
        assert!(t.is_sorted_on(&[0]));
        assert!(t.is_sorted_on(&[0, 1]));
        assert!(!t.is_sorted_on(&[1]));
    }

    #[test]
    fn large_sorts_match_std() {
        let mut state = 7u64;
        let rows: Vec<Vec<u64>> = (0..200_000)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                vec![(state >> 40) % 1000, state >> 20 & 0xffff]
            })
            .collect();
        let mut t =
            IdTable::from_rows(2, rows.iter().map(Vec::as_slice), &crate::test_budget()).unwrap();
        t.sort_by(&[1, 0]).unwrap();
        let mut expected = rows.clone();
        expected.sort_by_key(|r| (r[1], r[0]));
        assert_eq!(t.rows().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn dedup_project_slice_retain() {
        let mut t = table(&[&[2, 5], &[1, 5], &[2, 5], &[1, 4]]);
        t.dedup().unwrap();
        assert_eq!(
            t.rows().collect::<Vec<_>>(),
            vec![vec![1, 4], vec![1, 5], vec![2, 5]]
        );
        let p = t.clone().project(&[1]);
        assert_eq!(p.column(0), &[4, 5, 5]);
        assert!(p.sorted_by().is_empty(), "column 1 alone isn't sorted");
        let q = t.clone().project(&[0]);
        assert_eq!(q.sorted_by(), &[0]);
        let mut s = t.clone();
        s.slice(1, Some(1));
        assert_eq!(s.rows().collect::<Vec<_>>(), vec![vec![1, 5]]);
        t.retain(|t, row| t.get(row, 1) == 5).unwrap();
        assert_eq!(t.len(), 2);
        assert!(t.is_sorted_on(&[0, 1]));
    }

    #[test]
    fn projection_moves_buffers_and_preserves_order_and_empty_rows() {
        let original = table(&[&[1, 4], &[1, 5], &[2, 5]]).assume_sorted_by(vec![0, 1]);
        for columns in [
            vec![],
            vec![0],
            vec![1],
            vec![1, 0],
            vec![0, 1],
            vec![1, 0, 1],
        ] {
            let input = original.clone();
            let buffers: Vec<_> = input.columns().iter().map(|c| c.as_ptr()).collect();
            let expected: Vec<Vec<_>> = original
                .rows()
                .map(|row| columns.iter().map(|&c| row[c]).collect())
                .collect();
            let projected = input.project(&columns);
            assert_eq!(projected.rows().collect::<Vec<_>>(), expected);
            assert_eq!(projected.len(), original.len());
            for (at, &column) in columns.iter().enumerate() {
                if !columns[at + 1..].contains(&column) {
                    assert_eq!(projected.column(at).as_ptr(), buffers[column]);
                }
            }
            assert!(projected.check_sorted(projected.sorted_by()));
        }
    }

    #[test]
    fn ordered_dedup_uses_any_complete_sort_order_without_gathering() {
        let mut input = table(&[&[2, 1], &[2, 1], &[1, 2]]).assume_sorted_by(vec![1, 0]);
        let buffers: Vec<_> = input
            .columns()
            .iter()
            .map(|column| column.as_ptr())
            .collect();
        input.dedup_preserving_order().unwrap();
        assert_eq!(input.rows().collect::<Vec<_>>(), [vec![2, 1], vec![1, 2]]);
        assert_eq!(input.sorted_by(), [1, 0]);
        for (column, buffer) in input.columns().iter().zip(buffers) {
            assert_eq!(column.as_ptr(), buffer, "dedup must not sort/gather again");
        }

        // Repeating one key does not cover the other column: equal rows need not be
        // adjacent. Keep the hash path's first occurrence and its order in that case.
        let mut partial = table(&[&[1, 2], &[1, 3], &[1, 2]]).assume_sorted_by(vec![0, 0]);
        partial.dedup_preserving_order().unwrap();
        assert_eq!(partial.rows().collect::<Vec<_>>(), [vec![1, 2], vec![1, 3]]);
    }
}
