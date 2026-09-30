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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdTable {
    columns: Vec<Vec<u64>>,
    len: usize,
    /// The table is sorted lexicographically on these columns (in this order). Empty if
    /// nothing is known.
    sorted_by: Vec<usize>,
}

impl IdTable {
    /// An empty table with `width` columns.
    pub fn new(width: usize) -> Self {
        Self::with_capacity(width, 0)
    }

    pub fn with_capacity(width: usize, rows: usize) -> Self {
        Self {
            columns: (0..width).map(|_| Vec::with_capacity(rows)).collect(),
            len: 0,
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
        for part in &rest {
            out.append(part);
        }
        out.sorted_by.clear();
        out
    }

    /// Bytes held by the column buffers (by capacity), for memory budgets.
    pub fn memory_bytes(&self) -> usize {
        self.columns.iter().map(|c| c.capacity() * 8).sum()
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
        let mask: Vec<bool> = (0..self.len)
            .map(|row| {
                row == 0 || self.compare_rows(row - 1, row, &all) != std::cmp::Ordering::Equal
            })
            .collect();
        self.retain_mask(&mask);
    }

    /// Removes duplicate rows, keeping the first occurrence of each and the row order
    /// (SPARQL DISTINCT after ORDER BY). Sortedness is kept.
    pub fn dedup_preserving_order(&mut self) {
        if self.sorted_by.len() == self.width() && self.width() > 0 {
            self.dedup();
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

    /// A table with the given columns of `self`, in that order. Sortedness is kept for the
    /// longest prefix of the sort order that survives.
    pub fn project(&self, columns: &[usize]) -> IdTable {
        let projected = columns.iter().map(|&c| self.columns[c].clone()).collect();
        let sorted_by = self
            .sorted_by
            .iter()
            .map_while(|key| columns.iter().position(|c| c == key))
            .collect();
        IdTable {
            columns: projected,
            len: self.len,
            sorted_by,
        }
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

    fn table(rows: &[&[u64]]) -> IdTable {
        IdTable::from_rows(rows.first().map_or(0, |r| r.len()), rows.iter().copied())
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
        let p = t.project(&[1]);
        assert_eq!(p.column(0), &[4, 5, 5]);
        assert!(p.sorted_by().is_empty(), "column 1 alone isn't sorted");
        let q = t.project(&[0]);
        assert_eq!(q.sorted_by(), &[0]);
        let mut s = t.clone();
        s.slice(1, Some(1));
        assert_eq!(s.rows().collect::<Vec<_>>(), vec![vec![1, 5]]);
        t.retain(|t, row| t.get(row, 1) == 5);
        assert_eq!(t.len(), 2);
        assert!(t.is_sorted_on(&[0, 1]));
    }
}
