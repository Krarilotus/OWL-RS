//! Joins on key columns.
//!
//! Every join has the same output schema: all columns of `left`, then the columns of
//! `right` that aren't join keys, in order ([`output_columns`]). A key column appears once.
//!
//! | Function | Algorithm | Use |
//! |---|---|---|
//! | [`join`] | merge join with galloping if both inputs are sorted on the keys, else a hash join building on the smaller input | inner joins without UNDEF keys |
//! | [`left_join`] | hash join, probing with every left row | OPTIONAL; a filter over the combined row decides which matches count |
//! | [`anti_join`] | hash set of right keys | MINUS and FILTER NOT EXISTS with shared variables |
//! | [`join_with_undef`] | the fast join for rows with bound keys, nested loops for rows with UNDEF | inner joins where a key may be unbound (after OPTIONAL) |
//!
//! Key values must not be [`UNDEF`](crate::UNDEF) except in [`join_with_undef`] and on the
//! right side of [`left_join`]'s output.

use std::cmp::Ordering;

use hashbrown::HashMap;

use crate::UNDEF;
use crate::table::IdTable;

type Hasher = foldhash::fast::FixedState;

/// A predicate over a combined output row (the filter of an OPTIONAL).
pub type RowFilter<'a> = &'a dyn Fn(&[u64]) -> bool;

/// `(side, column)` pairs of the output in order: `false` = left, `true` = right.
pub fn output_columns(
    left_width: usize,
    right_width: usize,
    right_keys: &[usize],
) -> Vec<(bool, usize)> {
    (0..left_width)
        .map(|c| (false, c))
        .chain(
            (0..right_width)
                .filter(|c| !right_keys.contains(c))
                .map(|c| (true, c)),
        )
        .collect()
}

fn right_payload(right_width: usize, right_keys: &[usize]) -> Vec<usize> {
    (0..right_width)
        .filter(|c| !right_keys.contains(c))
        .collect()
}

#[inline]
fn compare_keys(
    left: &IdTable,
    l: usize,
    lk: &[usize],
    right: &IdTable,
    r: usize,
    rk: &[usize],
) -> Ordering {
    for (&a, &b) in lk.iter().zip(rk) {
        match left.get(l, a).cmp(&right.get(r, b)) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

#[inline]
fn same_key(table: &IdTable, a: usize, b: usize, keys: &[usize]) -> bool {
    keys.iter().all(|&k| table.get(a, k) == table.get(b, k))
}

/// Inner join of `left` and `right` on `left_keys[i] = right_keys[i]`.
pub fn join(left: &IdTable, right: &IdTable, left_keys: &[usize], right_keys: &[usize]) -> IdTable {
    assert_eq!(left_keys.len(), right_keys.len(), "join key arity");
    if left_keys.is_empty() {
        return cross_product(left, right);
    }
    if left.is_sorted_on(left_keys) && right.is_sorted_on(right_keys) {
        merge_join(left, right, left_keys, right_keys)
    } else if left.len() <= right.len() {
        // Build on the smaller side; the output schema stays left-first either way.
        hash_join(right, left, right_keys, left_keys, true)
    } else {
        hash_join(left, right, left_keys, right_keys, false)
    }
}

/// Inner join whose output keeps `left`'s row order (for a left side ordered by ORDER BY):
/// a hash join building on `right` and probing with every left row in order.
pub fn join_keeping_left_order(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
) -> IdTable {
    assert_eq!(left_keys.len(), right_keys.len(), "join key arity");
    if left_keys.is_empty() {
        return cross_product(left, right);
    }
    hash_join(left, right, left_keys, right_keys, false)
}

fn cross_product(left: &IdTable, right: &IdTable) -> IdTable {
    let mut out = IdTable::with_capacity(left.width() + right.width(), left.len() * right.len());
    let mut row = vec![0; left.width() + right.width()];
    for l in 0..left.len() {
        for (i, c) in left.columns().iter().enumerate() {
            row[i] = c[l];
        }
        for r in 0..right.len() {
            for (i, c) in right.columns().iter().enumerate() {
                row[left.width() + i] = c[r];
            }
            out.push_row(&row);
        }
    }
    out
}

/// First row `>= from` in `table` whose key is not less than `other`'s key at `target`.
/// Exponential then binary search: O(log distance).
fn gallop(
    table: &IdTable,
    keys: &[usize],
    from: usize,
    other: &IdTable,
    target: usize,
    other_keys: &[usize],
    table_is_left: bool,
) -> usize {
    let less = |row: usize| {
        let ord = if table_is_left {
            compare_keys(table, row, keys, other, target, other_keys)
        } else {
            compare_keys(other, target, other_keys, table, row, keys).reverse()
        };
        ord == Ordering::Less
    };
    let len = table.len();
    if from >= len || !less(from) {
        return from;
    }
    let mut step = 1;
    let mut low = from; // less(low) holds
    let mut high = from + 1;
    while high < len && less(high) {
        low = high;
        step *= 2;
        high = (low + step).min(len);
    }
    // less(low) and (high == len or !less(high)): binary search in (low, high]
    let (mut lo, mut hi) = (low + 1, high);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if less(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn merge_join(left: &IdTable, right: &IdTable, lk: &[usize], rk: &[usize]) -> IdTable {
    let payload = right_payload(right.width(), rk);
    let mut out = IdTable::new(left.width() + payload.len());
    let mut row = vec![0; out.width()];
    let (mut l, mut r) = (0, 0);
    while l < left.len() && r < right.len() {
        match compare_keys(left, l, lk, right, r, rk) {
            Ordering::Less => l = gallop(left, lk, l, right, r, rk, true),
            Ordering::Greater => r = gallop(right, rk, r, left, l, lk, false),
            Ordering::Equal => {
                let l_end = (l + 1..left.len())
                    .find(|&x| !same_key(left, l, x, lk))
                    .unwrap_or(left.len());
                let r_end = (r + 1..right.len())
                    .find(|&x| !same_key(right, r, x, rk))
                    .unwrap_or(right.len());
                for li in l..l_end {
                    for (i, c) in left.columns().iter().enumerate() {
                        row[i] = c[li];
                    }
                    for ri in r..r_end {
                        for (i, &c) in payload.iter().enumerate() {
                            row[left.width() + i] = right.get(ri, c);
                        }
                        out.push_row(&row);
                    }
                }
                l = l_end;
                r = r_end;
            }
        }
    }
    // The output follows the left input's order, so it is sorted on the left keys.
    out.assume_sorted_by(lk.to_vec())
}

/// Hash table from key to the chain of `build` rows with that key: `heads[key]` is the first
/// row, `next[row]` the following one (`u32::MAX` ends the chain). No allocation per key.
struct BuildTable {
    heads: HashMap<Vec<u64>, u32, Hasher>,
    single: HashMap<u64, u32, Hasher>,
    next: Vec<u32>,
    one_key: bool,
}

const END: u32 = u32::MAX;

impl BuildTable {
    fn new(build: &IdTable, keys: &[usize]) -> Self {
        assert!(build.len() < END as usize, "join input exceeds u32 rows");
        let one_key = keys.len() == 1;
        let mut table = Self {
            heads: HashMap::with_hasher(Hasher::default()),
            single: HashMap::with_capacity_and_hasher(
                if one_key { build.len() } else { 0 },
                Hasher::default(),
            ),
            next: vec![END; build.len()],
            one_key,
        };
        // Insert in reverse so chains list rows in input order.
        for row in (0..build.len()).rev() {
            let head = if one_key {
                table.single.entry(build.get(row, keys[0])).or_insert(END)
            } else {
                table
                    .heads
                    .entry(keys.iter().map(|&k| build.get(row, k)).collect())
                    .or_insert(END)
            };
            table.next[row] = *head;
            *head = row as u32;
        }
        table
    }

    fn first(&self, probe: &IdTable, row: usize, keys: &[usize]) -> u32 {
        if self.one_key {
            self.single
                .get(&probe.get(row, keys[0]))
                .copied()
                .unwrap_or(END)
        } else {
            let key: Vec<u64> = keys.iter().map(|&k| probe.get(row, k)).collect();
            self.heads.get(&key).copied().unwrap_or(END)
        }
    }
}

/// Hash join building on `build` and probing with `probe`. `build_is_left` says which input
/// is the left one of the output schema. The output follows the probe side's order.
fn hash_join(
    probe: &IdTable,
    build: &IdTable,
    probe_keys: &[usize],
    build_keys: &[usize],
    build_is_left: bool,
) -> IdTable {
    let table = BuildTable::new(build, build_keys);
    let (left, right, rk) = if build_is_left {
        (build, probe, probe_keys)
    } else {
        (probe, build, build_keys)
    };
    let payload = right_payload(right.width(), rk);
    let mut out = IdTable::new(left.width() + payload.len());
    let mut row = vec![0; out.width()];
    for p in 0..probe.len() {
        let mut b = table.first(probe, p, probe_keys);
        while b != END {
            let (li, ri) = if build_is_left {
                (b as usize, p)
            } else {
                (p, b as usize)
            };
            for (i, c) in left.columns().iter().enumerate() {
                row[i] = c[li];
            }
            for (i, &c) in payload.iter().enumerate() {
                row[left.width() + i] = right.get(ri, c);
            }
            out.push_row(&row);
            b = table.next[b as usize];
        }
    }
    if !build_is_left && probe.is_sorted_on(probe_keys) {
        out.assume_sorted_by(probe_keys.to_vec())
    } else {
        out
    }
}

/// Left outer join (OPTIONAL): every left row once per accepted match, or once with UNDEF in
/// the right payload columns if no match is accepted. `accept` sees the combined output row;
/// it implements the filter of `OPTIONAL { … FILTER(…) }`.
pub fn left_join(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    accept: Option<RowFilter<'_>>,
) -> IdTable {
    let payload = right_payload(right.width(), right_keys);
    let mut out = IdTable::new(left.width() + payload.len());
    let mut row = vec![0; out.width()];
    let table = (!left_keys.is_empty()).then(|| BuildTable::new(right, right_keys));
    for l in 0..left.len() {
        for (i, c) in left.columns().iter().enumerate() {
            row[i] = c[l];
        }
        let mut matched = false;
        let mut emit = |r: usize, row: &mut Vec<u64>, out: &mut IdTable| {
            for (i, &c) in payload.iter().enumerate() {
                row[left.width() + i] = right.get(r, c);
            }
            if accept.is_none_or(|f| f(row)) {
                out.push_row(row);
                matched = true;
            }
        };
        match &table {
            Some(table) => {
                let mut r = table.first(left, l, left_keys);
                while r != END {
                    emit(r as usize, &mut row, &mut out);
                    r = table.next[r as usize];
                }
            }
            None => (0..right.len()).for_each(|r| emit(r, &mut row, &mut out)),
        }
        if !matched {
            for i in 0..payload.len() {
                row[left.width() + i] = UNDEF;
            }
            out.push_row(&row);
        }
    }
    if left.is_sorted_on(left_keys) && !left_keys.is_empty() {
        out.assume_sorted_by(left_keys.to_vec())
    } else {
        out
    }
}

/// Left rows whose key doesn't occur in `right` (MINUS, FILTER NOT EXISTS). Order and
/// sortedness of `left` are kept.
pub fn anti_join(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
) -> IdTable {
    let mut out = left.clone();
    if right.is_empty() {
        return out;
    }
    if left_keys.is_empty() {
        out.slice(0, Some(0));
        return out;
    }
    let table = BuildTable::new(right, right_keys);
    out.retain(|t, row| table.first(t, row, left_keys) == END);
    out
}

/// Left rows whose key occurs in `right` (FILTER EXISTS). Order and sortedness of `left` are
/// kept; each left row appears at most once.
pub fn semi_join(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
) -> IdTable {
    let mut out = left.clone();
    if left_keys.is_empty() {
        if right.is_empty() {
            out.slice(0, Some(0));
        }
        return out;
    }
    let table = BuildTable::new(right, right_keys);
    out.retain(|t, row| table.first(t, row, left_keys) != END);
    out
}

/// Inner join where key columns may hold UNDEF (SPARQL compatibility: UNDEF matches any
/// value, and the output takes the bound one). Rows with bound keys on both sides go
/// through [`join`]; rows with an UNDEF key are matched by nested loops.
pub fn join_with_undef(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
) -> IdTable {
    let has_undef =
        |t: &IdTable, row: usize, keys: &[usize]| keys.iter().any(|&k| t.get(row, k) == UNDEF);
    let split = |t: &IdTable, keys: &[usize]| {
        let mut bound = t.clone();
        bound.retain(|t, row| !has_undef(t, row, keys));
        let mut open = t.clone();
        open.retain(|t, row| has_undef(t, row, keys));
        (bound, open)
    };
    let (left_bound, left_open) = split(left, left_keys);
    let (right_bound, right_open) = split(right, right_keys);
    let mut out = join(&left_bound, &right_bound, left_keys, right_keys);
    let payload = right_payload(right.width(), right_keys);
    let mut row = vec![0; out.width()];
    let mut nested = |l_table: &IdTable, r_table: &IdTable| {
        for l in 0..l_table.len() {
            'right: for r in 0..r_table.len() {
                for (&lk, &rk) in left_keys.iter().zip(right_keys) {
                    let (a, b) = (l_table.get(l, lk), r_table.get(r, rk));
                    if a != b && a != UNDEF && b != UNDEF {
                        continue 'right;
                    }
                }
                for (i, c) in l_table.columns().iter().enumerate() {
                    row[i] = c[l];
                }
                for (&lk, &rk) in left_keys.iter().zip(right_keys) {
                    if row[lk] == UNDEF {
                        row[lk] = r_table.get(r, rk);
                    }
                }
                for (i, &c) in payload.iter().enumerate() {
                    row[left.width() + i] = r_table.get(r, c);
                }
                out.push_row(&row);
            }
        }
    };
    nested(&left_open, right);
    nested(&left_bound, &right_open);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64 for test data.
    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)) % n
        }
    }

    fn random_table(rng: &mut Rng, width: usize, rows: usize, domain: u64, undef: bool) -> IdTable {
        let mut t = IdTable::new(width);
        for _ in 0..rows {
            let row: Vec<u64> = (0..width)
                .map(|_| {
                    if undef && rng.below(5) == 0 {
                        UNDEF
                    } else {
                        rng.below(domain)
                    }
                })
                .collect();
            t.push_row(&row);
        }
        t
    }

    /// The naive definition: every compatible pair, output as left columns + right payload.
    fn naive(left: &IdTable, right: &IdTable, lk: &[usize], rk: &[usize]) -> Vec<Vec<u64>> {
        let payload = right_payload(right.width(), rk);
        let mut out = Vec::new();
        for l in left.rows() {
            for r in right.rows() {
                let compatible = lk
                    .iter()
                    .zip(rk)
                    .all(|(&a, &b)| l[a] == r[b] || l[a] == UNDEF || r[b] == UNDEF);
                if compatible {
                    let mut row = l.clone();
                    for (&a, &b) in lk.iter().zip(rk) {
                        if row[a] == UNDEF {
                            row[a] = r[b];
                        }
                    }
                    row.extend(payload.iter().map(|&c| r[c]));
                    out.push(row);
                }
            }
        }
        out.sort();
        out
    }

    fn sorted_rows(t: &IdTable) -> Vec<Vec<u64>> {
        let mut rows: Vec<_> = t.rows().collect();
        rows.sort();
        rows
    }

    #[test]
    fn joins_match_the_naive_definition() {
        let mut rng = Rng(42);
        for case in 0..300 {
            let (lw, rw) = (1 + rng.below(3) as usize, 1 + rng.below(3) as usize);
            let keys = 1 + rng.below(lw.min(rw) as u64) as usize;
            let lk: Vec<usize> = (0..keys).collect();
            let rk: Vec<usize> = (0..keys).map(|i| rw - 1 - i).collect();
            let domain = 1 + rng.below(6);
            let (left_rows, right_rows) = (rng.below(40) as usize, rng.below(40) as usize);
            let mut left = random_table(&mut rng, lw, left_rows, domain, false);
            let mut right = random_table(&mut rng, rw, right_rows, domain, false);
            let expected = naive(&left, &right, &lk, &rk);
            assert_eq!(
                sorted_rows(&join(&left, &right, &lk, &rk)),
                expected,
                "hash, case {case}"
            );
            left.sort_by(&lk);
            right.sort_by(&rk);
            let merged = join(&left, &right, &lk, &rk);
            assert_eq!(sorted_rows(&merged), expected, "merge, case {case}");
            assert!(merged.is_sorted_on(&lk));
        }
    }

    #[test]
    fn undef_joins_match_the_naive_definition() {
        let mut rng = Rng(7);
        for case in 0..300 {
            let (left_rows, right_rows) = (rng.below(20) as usize, rng.below(20) as usize);
            let left = random_table(&mut rng, 2, left_rows, 4, true);
            let right = random_table(&mut rng, 2, right_rows, 4, true);
            let got = join_with_undef(&left, &right, &[0, 1], &[1, 0]);
            assert_eq!(
                sorted_rows(&got),
                naive(&left, &right, &[0, 1], &[1, 0]),
                "case {case}"
            );
        }
    }

    #[test]
    fn left_and_anti_joins() {
        let left = IdTable::from_rows(2, [&[1, 10][..], &[2, 20], &[3, 30]]);
        let right = IdTable::from_rows(2, [&[1, 100][..], &[1, 101], &[3, 300]]);
        let out = left_join(&left, &right, &[0], &[0], None);
        assert_eq!(
            sorted_rows(&out),
            vec![
                vec![1, 10, 100],
                vec![1, 10, 101],
                vec![2, 20, UNDEF],
                vec![3, 30, 300]
            ]
        );
        // The OPTIONAL filter rejects 101 and 300: row 3 falls back to UNDEF.
        let filtered = left_join(
            &left,
            &right,
            &[0],
            &[0],
            Some(&|row: &[u64]| row[2] == 100),
        );
        assert_eq!(
            sorted_rows(&filtered),
            vec![vec![1, 10, 100], vec![2, 20, UNDEF], vec![3, 30, UNDEF]]
        );
        let anti = anti_join(&left, &right, &[0], &[0]);
        assert_eq!(sorted_rows(&anti), vec![vec![2, 20]]);
    }
}
