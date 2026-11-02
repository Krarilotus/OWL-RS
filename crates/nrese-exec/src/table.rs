//! [`IdTable`]: the one intermediate-result representation.
//!
//! Column-major, like QLever's: each column is a contiguous `Vec<u64>`, so scans, filters
//! and joins touch only the columns they need, and the compiler can vectorise loops over
//! them. Rows are addressed by index. A table records which columns it's sorted on
//! ([`sorted_by`](IdTable::sorted_by)), so a join can skip sorting inputs that already arrive
//! in the right order (index scans produce sorted output).

use rayon::prelude::*;

/// Row count from which sorting runs in parallel.
const PARALLEL_SORT_ROWS: usize = 1 << 16;

/// What a row of a table without columns counts for memory budgets, after its first: one
/// id, the column an operator after it may make for it (an unbound variable's UNDEF, a
/// BIND), as a join's row limit counts it. The table itself holds only its row count, so
/// without this a query could make rows past its budget that hold nothing, and the next
/// operator a column of them, before anything is charged. The first row, the one solution
/// of an empty pattern, counts nothing: a column of it is one id a table, which a table's
/// charge leaves out as it leaves out its list of columns. So making and consuming one
/// solution without bindings, once per row of a substituted EXISTS, charges nothing.
const ROW_WITHOUT_COLUMNS_BYTES: usize = size_of::<u64>();

#[derive(Debug, Clone, Default)]
pub struct IdTable {
    columns: Vec<Vec<u64>>,
    len: usize,
    /// Without columns, the rows the table counts room for, as a column counts its
    /// capacity ([`Self::memory_bytes`]): what was reserved, and the most it held; it
    /// doesn't drop as rows go.
    rows_held: usize,
    /// The table is sorted lexicographically on these columns (in this order). Empty if
    /// nothing is known.
    sorted_by: Vec<usize>,
}

/// Tables are equal by their rows and sort order; what they count for budgets doesn't
/// matter.
impl PartialEq for IdTable {
    fn eq(&self, other: &Self) -> bool {
        self.columns == other.columns && self.len == other.len && self.sorted_by == other.sorted_by
    }
}

impl Eq for IdTable {}

impl IdTable {
    /// An empty table with `width` columns.
    pub fn new(width: usize) -> Self {
        Self::with_capacity(width, 0)
    }

    pub fn with_capacity(width: usize, rows: usize) -> Self {
        Self {
            columns: (0..width).map(|_| Vec::with_capacity(rows)).collect(),
            len: 0,
            rows_held: rows,
            sorted_by: Vec::new(),
        }
    }

    /// A table from equal-length columns.
    ///
    /// # Panics
    /// If the columns differ in length.
    pub fn from_columns(columns: Vec<Vec<u64>>) -> Self {
        let len = columns.first().map_or(0, Vec::len);
        assert!(
            columns.iter().all(|c| c.len() == len),
            "IdTable columns must have equal lengths"
        );
        Self {
            columns,
            len,
            rows_held: 0,
            sorted_by: Vec::new(),
        }
    }

    /// A table with one row per entry of `rows`, each `width` wide.
    pub fn from_rows<'a>(width: usize, rows: impl IntoIterator<Item = &'a [u64]>) -> Self {
        let mut table = Self::new(width);
        for row in rows {
            table.push_row(row);
        }
        table
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

    pub fn into_columns(self) -> Vec<Vec<u64>> {
        self.columns
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

    #[inline]
    pub fn push_row(&mut self, row: &[u64]) {
        debug_assert_eq!(row.len(), self.width(), "row width");
        for (column, &value) in self.columns.iter_mut().zip(row) {
            column.push(value);
        }
        self.len += 1;
        self.sorted_by.clear();
    }

    /// Adds a column, one value per row.
    pub fn push_column(&mut self, column: Vec<u64>) {
        assert_eq!(column.len(), self.len, "push_column: rows differ");
        self.columns.push(column);
    }

    /// The rows that fit before the table reallocates; without columns, before it counts
    /// more ([`Self::memory_bytes`]).
    pub fn spare_rows(&self) -> usize {
        self.columns
            .iter()
            .map(|c| c.capacity() - c.len())
            .min()
            .unwrap_or(self.rows_held.saturating_sub(self.len))
    }

    /// Room for `rows` more rows, allocated exactly; without columns, counted.
    pub fn reserve_exact(&mut self, rows: usize) {
        for column in &mut self.columns {
            column.reserve_exact(rows);
        }
        self.rows_held = self.rows_held.max(self.len.saturating_add(rows));
    }

    /// What a row of this table counts for memory budgets ([`Self::memory_bytes`]): an id
    /// a column, and one for a table without columns (after its first row).
    pub fn row_bytes(&self) -> usize {
        match self.width() {
            0 => ROW_WITHOUT_COLUMNS_BYTES,
            width => width * size_of::<u64>(),
        }
    }

    /// What this table counts for memory budgets with room for exactly `rows` rows
    /// ([`Self::memory_bytes`]): what to charge before it grows to that room.
    pub fn bytes_for_rows(&self, rows: usize) -> usize {
        match self.width() {
            0 => rows
                .saturating_sub(1)
                .saturating_mul(ROW_WITHOUT_COLUMNS_BYTES),
            width => rows.saturating_mul(width * size_of::<u64>()),
        }
    }

    /// Appends all rows of `other`, which must have the same width. Sortedness is lost.
    pub fn append(&mut self, other: &IdTable) {
        assert_eq!(self.width(), other.width(), "append: widths differ");
        for (column, extra) in self.columns.iter_mut().zip(&other.columns) {
            column.extend_from_slice(extra);
        }
        self.len += other.len;
        self.sorted_by.clear();
    }

    /// The rows of `parts` in order, in one table `width` wide (for parallel operators that
    /// produce one part per chunk of their input).
    pub fn concat(width: usize, parts: Vec<IdTable>) -> IdTable {
        let mut parts = parts.into_iter();
        let Some(mut out) = parts.next() else {
            return IdTable::new(width);
        };
        let rest: Vec<IdTable> = parts.collect();
        let extra: usize = rest.iter().map(IdTable::len).sum();
        for column in &mut out.columns {
            column.reserve_exact(extra);
        }
        // Sorted on what every part is sorted on, where each part starts at or after the
        // row before it (the morsels of a sorted scan, in order).
        let mut sorted = std::mem::take(&mut out.sorted_by);
        // Each part is freed as soon as it is copied: together with the output they
        // would hold the rows twice.
        for part in rest {
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
            out.append(&part);
        }
        out.sorted_by = sorted;
        out
    }

    /// The bytes [`Self::concat`] of `parts` allocates beyond them: each column of the first
    /// part without room for every row, reallocated to hold them all.
    pub fn concat_growth<'a>(parts: impl IntoIterator<Item = &'a IdTable>) -> usize {
        let mut parts = parts.into_iter();
        let Some(first) = parts.next() else {
            return 0;
        };
        let rows = first.len + parts.map(IdTable::len).sum::<usize>();
        let short = first.columns.iter().filter(|c| c.capacity() < rows).count();
        short.saturating_mul(rows).saturating_mul(8)
    }

    /// Bytes held by the column buffers (by capacity), for memory budgets. A table without
    /// columns counts one id a row after the first ([`ROW_WITHOUT_COLUMNS_BYTES`]) of the
    /// rows it has room for: what was reserved, and the most it held, which doesn't drop as
    /// rows go.
    pub fn memory_bytes(&self) -> usize {
        if self.columns.is_empty() {
            return self.bytes_for_rows(self.len.max(self.rows_held));
        }
        self.columns.iter().map(|c| c.capacity() * 8).sum()
    }

    /// Before rows are removed: a table without columns keeps counting them.
    fn hold_rows(&mut self) {
        self.rows_held = self.rows_held.max(self.len);
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

    /// Sorts the rows lexicographically on `keys`, unless already sorted on them. The sort
    /// computes a row permutation once and gathers every column through it, in parallel for
    /// large tables.
    pub fn sort_by(&mut self, keys: &[usize]) {
        if self.is_sorted_on(keys) {
            if self.sorted_by.len() < keys.len() {
                self.sorted_by = keys.to_vec();
            }
            return;
        }
        let order = self.sort_order(keys);
        self.gather(&order);
        self.sorted_by = keys.to_vec();
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
    }

    /// Keeps the rows for which `keep(row)` is true, preserving order and sortedness.
    pub fn retain(&mut self, mut keep: impl FnMut(&IdTable, usize) -> bool) {
        let mask: Vec<bool> = (0..self.len).map(|row| keep(self, row)).collect();
        self.retain_mask(&mask);
    }

    /// Keeps the rows whose `mask` entry is true, preserving order and sortedness.
    /// [`retain`](Self::retain) with a thread-safe predicate, evaluated in parallel for
    /// large tables.
    pub fn par_retain(&mut self, keep: impl Fn(&IdTable, usize) -> bool + Sync) {
        let mask: Vec<bool> = if self.len < PARALLEL_SORT_ROWS {
            (0..self.len).map(|row| keep(self, row)).collect()
        } else {
            (0..self.len)
                .into_par_iter()
                .map(|row| keep(self, row))
                .collect()
        };
        self.retain_mask(&mask);
    }

    pub fn retain_mask(&mut self, mask: &[bool]) {
        assert_eq!(mask.len(), self.len, "mask length");
        self.hold_rows();
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
    pub fn dedup(&mut self) {
        let all: Vec<usize> = (0..self.width()).collect();
        self.sort_by(&all);
        self.dedup_adjacent();
    }

    /// Equal rows are adjacent when any permutation of all columns orders the table.
    fn dedup_adjacent(&mut self) {
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
    }

    /// Removes duplicate rows, keeping the first occurrence of each and the row order
    /// (SPARQL DISTINCT after ORDER BY). Sortedness is kept.
    pub fn dedup_preserving_order(&mut self) {
        if self.width() > 0 && (0..self.width()).all(|column| self.sorted_by.contains(&column)) {
            self.dedup_adjacent();
            return;
        }
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
        self.retain_mask(&mask);
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
        IdTable {
            columns: projected,
            len: self.len,
            rows_held: 0,
            sorted_by,
        }
    }

    /// Keeps rows `offset..offset + limit`.
    pub fn slice(&mut self, offset: usize, limit: Option<usize>) {
        let start = offset.min(self.len);
        let end = limit.map_or(self.len, |l| start.saturating_add(l).min(self.len));
        self.hold_rows();
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

    fn table(rows: &[&[u64]]) -> IdTable {
        IdTable::from_rows(rows.first().map_or(0, |r| r.len()), rows.iter().copied())
    }

    /// Concatenated parts keep the sort order they share where each starts at or after the
    /// row before it, and lose it where one doesn't.
    #[test]
    fn concatenation_keeps_a_shared_order_across_ordered_parts() {
        let a = table(&[&[1, 9], &[2, 8]]).assume_sorted_by(vec![0, 1]);
        let b = table(&[&[2, 9], &[3, 1]]).assume_sorted_by(vec![0]);
        let joined = IdTable::concat(2, vec![a.clone(), b.clone()]);
        assert_eq!(joined.sorted_by(), [0]);
        assert_eq!(joined.len(), 4);
        // Out of order at the boundary: no order.
        let joined = IdTable::concat(2, vec![b, a]);
        assert!(joined.sorted_by().is_empty());
        // An empty part in between changes nothing.
        let c = table(&[&[1, 1]]).assume_sorted_by(vec![0]);
        let d = table(&[&[5, 0]]).assume_sorted_by(vec![0]);
        let joined = IdTable::concat(2, vec![c, IdTable::new(2).assume_sorted_by(vec![0]), d]);
        assert_eq!(joined.sorted_by(), [0]);
    }

    #[test]
    fn sort_is_lexicographic_and_stable_for_equal_keys() {
        let mut t = table(&[&[3, 1], &[1, 9], &[3, 0], &[1, 2]]);
        t.sort_by(&[0, 1]);
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
        let mut t = IdTable::from_rows(2, rows.iter().map(Vec::as_slice));
        t.sort_by(&[1, 0]);
        let mut expected = rows.clone();
        expected.sort_by_key(|r| (r[1], r[0]));
        assert_eq!(t.rows().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn dedup_project_slice_retain() {
        let mut t = table(&[&[2, 5], &[1, 5], &[2, 5], &[1, 4]]);
        t.dedup();
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
        t.retain(|t, row| t.get(row, 1) == 5);
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

    /// Spec (SW1, `notes/2026-11-01-late-budget-r2.md`; first row, PR #84's timing,
    /// notes/2026-11-02-zero-width-budget.md ZW-10): rows without columns count against the
    /// budget where they are made, one id each after the first (the column an operator after
    /// them may make: an unbound variable's UNDEF), so `memory_bytes` reports
    /// `(rows - 1) * 8` for a table with no columns; one solution without bindings counts
    /// nothing. What a table reports doesn't drop while it lives, as a column's capacity
    /// doesn't when rows go: the charge taken for it is the charge released. Room for `n`
    /// rows counts what `bytes_for_rows(n)` says, with columns or without.
    #[test]
    fn rows_without_columns_count_one_id_each() {
        let rows = |n: usize| IdTable::from_rows(0, std::iter::repeat_n(&[][..], n));
        let wide = |n: usize, width: usize| {
            IdTable::from_columns((0..width).map(|_| vec![7; n]).collect())
        };
        assert_eq!(rows(0).memory_bytes(), 0);
        assert_eq!(rows(1).memory_bytes(), 0);
        assert_eq!(rows(2).memory_bytes(), 8);
        assert_eq!(rows(1_000).memory_bytes(), 7_992);
        let mut grown = rows(10);
        grown.append(&rows(990));
        assert_eq!(grown.memory_bytes(), 7_992);
        assert_eq!(
            IdTable::concat(0, vec![rows(400), rows(600)]).memory_bytes(),
            7_992
        );
        assert_eq!(wide(1_000, 2).project(&[]).memory_bytes(), 7_992);
        // Room reserved counts as a column's capacity does, and is what fits before more
        // is counted.
        let mut room = IdTable::with_capacity(0, 100);
        assert_eq!((room.memory_bytes(), room.spare_rows()), (792, 100));
        room.reserve_exact(400);
        assert_eq!((room.memory_bytes(), room.spare_rows()), (3_192, 400));
        room.append(&rows(450));
        assert_eq!((room.memory_bytes(), room.spare_rows()), (3_592, 0));
        for width in [0, 1, 3] {
            let room = IdTable::with_capacity(width, 100);
            assert_eq!(
                room.bytes_for_rows(100),
                room.memory_bytes(),
                "{width} columns"
            );
        }
        let mut cut = rows(1_000);
        cut.retain_mask(&[true, false].repeat(500));
        assert_eq!(cut.len(), 500);
        assert_eq!(cut.memory_bytes(), 7_992);
        cut.slice(100, Some(10));
        assert_eq!(cut.len(), 10);
        assert_eq!(cut.memory_bytes(), 7_992);
    }

    #[test]
    fn ordered_dedup_uses_any_complete_sort_order_without_gathering() {
        let mut input = table(&[&[2, 1], &[2, 1], &[1, 2]]).assume_sorted_by(vec![1, 0]);
        let buffers: Vec<_> = input
            .columns()
            .iter()
            .map(|column| column.as_ptr())
            .collect();
        input.dedup_preserving_order();
        assert_eq!(input.rows().collect::<Vec<_>>(), [vec![2, 1], vec![1, 2]]);
        assert_eq!(input.sorted_by(), [1, 0]);
        for (column, buffer) in input.columns().iter().zip(buffers) {
            assert_eq!(column.as_ptr(), buffer, "dedup must not sort/gather again");
        }

        // Repeating one key does not cover the other column: equal rows need not be
        // adjacent. Keep the hash path's first occurrence and its order in that case.
        let mut partial = table(&[&[1, 2], &[1, 3], &[1, 2]]).assume_sorted_by(vec![0, 0]);
        partial.dedup_preserving_order();
        assert_eq!(partial.rows().collect::<Vec<_>>(), [vec![1, 2], vec![1, 3]]);
    }
}
