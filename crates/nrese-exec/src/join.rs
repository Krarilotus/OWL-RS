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
//! | [`outer_join_with_undef`] | a hash table for rows with bound keys, scans for rows with UNDEF; left order kept | OPTIONAL, and joins that keep the left order, where a key may be unbound |
//! | [`compatible_mask`] | the same | MINUS and (NOT) EXISTS where a key may be unbound |
//!
//! Key values must not be [`UNDEF`](crate::UNDEF) except in the `*_with_undef` functions,
//! in [`compatible_mask`], and on the right side of [`left_join`]'s output.
//!
//! Joins that can multiply rows take `max_rows` and stop with [`TooManyRows`] once their
//! output would exceed it, before the memory is taken: a cross product of three 2,000-row
//! tables would otherwise ask for 64 GB at once.

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

use hashbrown::HashMap;
use rayon::prelude::*;

use crate::UNDEF;
use crate::table::IdTable;

type Hasher = foldhash::fast::FixedState;

/// Probe rows per parallel task; smaller joins run on one thread.
const PARALLEL_ROWS: usize = 1 << 14;

/// Output rows a sink produces between two checks of the shared row limit.
const LIMIT_STEP: usize = 1024;

/// A join's output would have more rows than its `max_rows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooManyRows {
    pub max_rows: usize,
}

impl std::fmt::Display for TooManyRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "join output exceeds {} rows", self.max_rows)
    }
}

impl std::error::Error for TooManyRows {}

/// How far a join may go: at most `max_rows` output rows, and no further once `stop` is set
/// (the query was cancelled). Both are checked every [`LIMIT_STEP`] output rows, so a
/// join that would run for hours stops within a few thousand rows of either.
#[derive(Debug, Clone, Default)]
pub struct RowLimit {
    pub max_rows: usize,
    pub stop: Option<Arc<AtomicBool>>,
}

impl From<usize> for RowLimit {
    fn from(max_rows: usize) -> Self {
        Self {
            max_rows,
            stop: None,
        }
    }
}

/// The output rows of one join so far, shared by its parallel parts.
struct Limit {
    max_rows: usize,
    rows: AtomicUsize,
    stop: Option<Arc<AtomicBool>>,
}

impl Limit {
    fn new(limit: impl Into<RowLimit>) -> Self {
        let limit = limit.into();
        Self {
            max_rows: limit.max_rows,
            rows: AtomicUsize::new(0),
            stop: limit.stop,
        }
    }

    fn stopped(&self) -> bool {
        self.stop
            .as_ref()
            .is_some_and(|stop| stop.load(AtomicOrdering::Relaxed))
    }

    fn grow(&self, rows: usize) -> Result<(), TooManyRows> {
        let total = self.rows.fetch_add(rows, AtomicOrdering::Relaxed) + rows;
        if total > self.max_rows || self.stopped() {
            return Err(TooManyRows {
                max_rows: self.max_rows,
            });
        }
        Ok(())
    }
}

/// An output table that charges its rows against a [`Limit`] every [`LIMIT_STEP`] rows.
struct Sink<'a> {
    out: IdTable,
    pending: usize,
    limit: &'a Limit,
}

impl<'a> Sink<'a> {
    fn new(width: usize, limit: &'a Limit) -> Self {
        Self {
            out: IdTable::new(width),
            pending: 0,
            limit,
        }
    }

    #[inline]
    fn push(&mut self, row: &[u64]) -> Result<(), TooManyRows> {
        self.pending += 1;
        if self.pending == LIMIT_STEP {
            self.limit.grow(LIMIT_STEP)?;
            self.pending = 0;
        }
        self.out.push_row(row);
        Ok(())
    }

    fn finish(self) -> Result<IdTable, TooManyRows> {
        self.limit.grow(self.pending)?;
        Ok(self.out)
    }
}

/// Runs `part` over consecutive row ranges of a `len`-row input, in parallel for large
/// inputs, and concatenates the parts in input order (so the output order is the
/// sequential one).
fn chunked(
    len: usize,
    width: usize,
    part: impl Fn(Range<usize>) -> Result<IdTable, TooManyRows> + Sync,
) -> Result<IdTable, TooManyRows> {
    if len < 2 * PARALLEL_ROWS {
        return part(0..len);
    }
    let parts: Vec<IdTable> = (0..len.div_ceil(PARALLEL_ROWS))
        .into_par_iter()
        .map(|i| part(i * PARALLEL_ROWS..((i + 1) * PARALLEL_ROWS).min(len)))
        .collect::<Result<_, _>>()?;
    Ok(IdTable::concat(width, parts))
}

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

/// Inner join of `left` and `right` on `left_keys[i] = right_keys[i]`, of at most
/// `max_rows` rows.
pub fn join(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    limit: impl Into<RowLimit>,
) -> Result<IdTable, TooManyRows> {
    assert_eq!(left_keys.len(), right_keys.len(), "join key arity");
    let limit = Limit::new(limit);
    if left_keys.is_empty() {
        return cross_product(left, right, &limit);
    }
    if left.is_sorted_on(left_keys) && right.is_sorted_on(right_keys) {
        merge_join(left, right, left_keys, right_keys, &limit)
    } else if left.len() <= right.len() {
        // Build on the smaller side; the output schema stays left-first either way.
        hash_join(right, left, right_keys, left_keys, true, &limit)
    } else {
        hash_join(left, right, left_keys, right_keys, false, &limit)
    }
}

/// Inner join whose output keeps `left`'s row order (for a left side ordered by ORDER BY):
/// a hash join building on `right` and probing with every left row in order.
pub fn join_keeping_left_order(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    limit: impl Into<RowLimit>,
) -> Result<IdTable, TooManyRows> {
    assert_eq!(left_keys.len(), right_keys.len(), "join key arity");
    let limit = Limit::new(limit);
    if left_keys.is_empty() {
        return cross_product(left, right, &limit);
    }
    hash_join(left, right, left_keys, right_keys, false, &limit)
}

fn cross_product(left: &IdTable, right: &IdTable, limit: &Limit) -> Result<IdTable, TooManyRows> {
    // The size is known: refuse before producing anything.
    let rows = left.len().saturating_mul(right.len());
    if rows > limit.max_rows {
        return Err(TooManyRows {
            max_rows: limit.max_rows,
        });
    }
    let width = left.width() + right.width();
    let out = chunked(left.len(), width, |lefts| {
        // Reserve up front, but not unboundedly: an unlimited join still grows gradually.
        let mut out = IdTable::with_capacity(width, (lefts.len() * right.len()).min(1 << 20));
        let mut row = vec![0; width];
        for l in lefts {
            for (i, c) in left.columns().iter().enumerate() {
                row[i] = c[l];
            }
            for r in 0..right.len() {
                for (i, c) in right.columns().iter().enumerate() {
                    row[left.width() + i] = c[r];
                }
                out.push_row(&row);
            }
            if limit.stopped() {
                return Err(TooManyRows {
                    max_rows: limit.max_rows,
                });
            }
        }
        Ok(out)
    })?;
    limit.grow(out.len())?;
    Ok(out)
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

fn merge_join(
    left: &IdTable,
    right: &IdTable,
    lk: &[usize],
    rk: &[usize],
    limit: &Limit,
) -> Result<IdTable, TooManyRows> {
    let width = left.width() + right_payload(right.width(), rk).len();
    // Each chunk of left rows starts at the first right row with its key.
    let out = chunked(left.len(), width, |rows| {
        let start = if rows.start == 0 {
            0
        } else {
            gallop(right, rk, 0, left, rows.start, lk, false)
        };
        merge_rows(left, rows, right, start, lk, rk, limit)
    })?;
    // The output follows the left input's order, so it is sorted on the left keys.
    Ok(out.assume_sorted_by(lk.to_vec()))
}

/// Merge join of the left rows `rows` with the right rows from `r`.
fn merge_rows(
    left: &IdTable,
    rows: Range<usize>,
    right: &IdTable,
    mut r: usize,
    lk: &[usize],
    rk: &[usize],
    limit: &Limit,
) -> Result<IdTable, TooManyRows> {
    let payload = right_payload(right.width(), rk);
    let width = left.width() + payload.len();
    let mut out = Sink::new(width, limit);
    let mut row = vec![0; width];
    let (mut l, end) = (rows.start, rows.end);
    while l < end && r < right.len() {
        match compare_keys(left, l, lk, right, r, rk) {
            Ordering::Less => l = gallop(left, lk, l, right, r, rk, true).min(end),
            Ordering::Greater => r = gallop(right, rk, r, left, l, lk, false),
            Ordering::Equal => {
                let l_end = (l + 1..end)
                    .find(|&x| !same_key(left, l, x, lk))
                    .unwrap_or(end);
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
                        out.push(&row)?;
                    }
                }
                l = l_end;
                r = r_end;
            }
        }
    }
    out.finish()
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
    limit: &Limit,
) -> Result<IdTable, TooManyRows> {
    let table = BuildTable::new(build, build_keys);
    let (left, right, rk) = if build_is_left {
        (build, probe, probe_keys)
    } else {
        (probe, build, build_keys)
    };
    let payload = right_payload(right.width(), rk);
    let width = left.width() + payload.len();
    let out = chunked(probe.len(), width, |rows| {
        let mut out = Sink::new(width, limit);
        let mut row = vec![0; width];
        for p in rows {
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
                out.push(&row)?;
                b = table.next[b as usize];
            }
        }
        out.finish()
    })?;
    Ok(if !build_is_left && probe.is_sorted_on(probe_keys) {
        out.assume_sorted_by(probe_keys.to_vec())
    } else {
        out
    })
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
    limit: impl Into<RowLimit>,
) -> Result<IdTable, TooManyRows> {
    let width = left.width() + right_payload(right.width(), right_keys).len();
    let table = (!left_keys.is_empty()).then(|| BuildTable::new(right, right_keys));
    let limit = Limit::new(limit);
    let inputs = LeftJoin {
        left,
        right,
        left_keys,
        right_keys,
        table: table.as_ref(),
    };
    let out = match accept {
        // The filter may not be thread-safe (it decodes terms through a cache).
        Some(accept) => left_join_rows(&inputs, 0..left.len(), Some(accept), &limit)?,
        None => chunked(left.len(), width, |rows| {
            left_join_rows(&inputs, rows, None, &limit)
        })?,
    };
    Ok(if left.is_sorted_on(left_keys) && !left_keys.is_empty() {
        out.assume_sorted_by(left_keys.to_vec())
    } else {
        out
    })
}

/// The inputs of a [`left_join`], shared by its parallel parts.
struct LeftJoin<'a> {
    left: &'a IdTable,
    right: &'a IdTable,
    left_keys: &'a [usize],
    right_keys: &'a [usize],
    /// `right` hashed on its keys; `None` without keys (every row matches).
    table: Option<&'a BuildTable>,
}

/// [`left_join`] of the left rows `rows`.
fn left_join_rows(
    join: &LeftJoin<'_>,
    rows: Range<usize>,
    accept: Option<RowFilter<'_>>,
    limit: &Limit,
) -> Result<IdTable, TooManyRows> {
    let LeftJoin {
        left,
        right,
        left_keys,
        right_keys,
        table,
    } = *join;
    let payload = right_payload(right.width(), right_keys);
    let width = left.width() + payload.len();
    let mut out = Sink::new(width, limit);
    let mut row = vec![0; width];
    for l in rows {
        for (i, c) in left.columns().iter().enumerate() {
            row[i] = c[l];
        }
        let mut matched = false;
        let mut emit = |r: usize, row: &mut Vec<u64>, out: &mut Sink<'_>| {
            for (i, &c) in payload.iter().enumerate() {
                row[left.width() + i] = right.get(r, c);
            }
            if accept.is_none_or(|f| f(row)) {
                out.push(row)?;
                matched = true;
            }
            Ok(())
        };
        match table {
            Some(table) => {
                let mut r = table.first(left, l, left_keys);
                while r != END {
                    emit(r as usize, &mut row, &mut out)?;
                    r = table.next[r as usize];
                }
            }
            None => {
                for r in 0..right.len() {
                    emit(r, &mut row, &mut out)?;
                }
            }
        }
        if !matched {
            for i in 0..payload.len() {
                row[left.width() + i] = UNDEF;
            }
            out.push(&row)?;
        }
    }
    out.finish()
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
    out.par_retain(|t, row| table.first(t, row, left_keys) == END);
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
    out.par_retain(|t, row| table.first(t, row, left_keys) != END);
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
    limit: impl Into<RowLimit>,
) -> Result<IdTable, TooManyRows> {
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
    let limit = limit.into();
    let joined = join(
        &left_bound,
        &right_bound,
        left_keys,
        right_keys,
        limit.clone(),
    )?;
    let limit = Limit::new(limit);
    limit.grow(joined.len())?;
    let payload = right_payload(right.width(), right_keys);
    let mut row = vec![0; joined.width()];
    let mut out = Sink::new(joined.width(), &limit);
    out.out = joined;
    let mut nested = |l_table: &IdTable, r_table: &IdTable| -> Result<(), TooManyRows> {
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
                out.push(&row)?;
            }
        }
        Ok(())
    };
    nested(&left_open, right)?;
    nested(&left_bound, &right_open)?;
    out.finish()
}

/// `right` for joins whose keys may be UNDEF: its rows with every key bound by key, and
/// the rows with an unbound key, which can match any left row.
struct Compatible<'a> {
    right: &'a IdTable,
    left_keys: &'a [usize],
    right_keys: &'a [usize],
    bound: HashMap<Vec<u64>, Vec<u32>, Hasher>,
    open: Vec<u32>,
}

impl<'a> Compatible<'a> {
    fn new(right: &'a IdTable, left_keys: &'a [usize], right_keys: &'a [usize]) -> Self {
        let mut bound: HashMap<Vec<u64>, Vec<u32>, Hasher> = HashMap::default();
        let mut open = Vec::new();
        for r in 0..right.len() {
            let key: Vec<u64> = right_keys.iter().map(|&k| right.get(r, k)).collect();
            if key.contains(&UNDEF) {
                open.push(r as u32);
            } else {
                bound.entry(key).or_default().push(r as u32);
            }
        }
        Self {
            right,
            left_keys,
            right_keys,
            bound,
            open,
        }
    }

    /// Whether left row `l` and right row `r` agree wherever both bind a key.
    fn agree(&self, left: &IdTable, l: usize, r: usize) -> bool {
        self.left_keys.iter().zip(self.right_keys).all(|(&a, &b)| {
            let (x, y) = (left.get(l, a), self.right.get(r, b));
            x == y || x == UNDEF || y == UNDEF
        })
    }

    /// Calls `found` with each right row compatible with left row `l`, in right order for
    /// a left row with an unbound key; stops when `found` returns `false`.
    fn each(
        &self,
        left: &IdTable,
        l: usize,
        mut found: impl FnMut(usize) -> Result<bool, TooManyRows>,
    ) -> Result<(), TooManyRows> {
        let key: Vec<u64> = self.left_keys.iter().map(|&k| left.get(l, k)).collect();
        if key.contains(&UNDEF) {
            for r in 0..self.right.len() {
                if self.agree(left, l, r) && !found(r)? {
                    return Ok(());
                }
            }
            return Ok(());
        }
        for &r in self.bound.get(&key).into_iter().flatten() {
            if !found(r as usize)? {
                return Ok(());
            }
        }
        for &r in &self.open {
            if self.agree(left, l, r as usize) && !found(r as usize)? {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// A join in `left`'s row order where key columns may hold UNDEF (SPARQL compatibility:
/// UNDEF matches any value, and the output takes the bound one). With `keep_unmatched`
/// it is the left outer join (OPTIONAL): a left row without an accepted match appears
/// once, its right payload UNDEF. `accept` sees the combined output row.
pub fn outer_join_with_undef(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    accept: Option<RowFilter<'_>>,
    keep_unmatched: bool,
    limit: impl Into<RowLimit>,
) -> Result<IdTable, TooManyRows> {
    let payload = right_payload(right.width(), right_keys);
    let width = left.width() + payload.len();
    let limit = Limit::new(limit);
    let mut out = Sink::new(width, &limit);
    let compatible = Compatible::new(right, left_keys, right_keys);
    let mut row = vec![0; width];
    for l in 0..left.len() {
        let mut matched = false;
        compatible.each(left, l, |r| {
            for (i, c) in left.columns().iter().enumerate() {
                row[i] = c[l];
            }
            for (&a, &b) in left_keys.iter().zip(right_keys) {
                if row[a] == UNDEF {
                    row[a] = right.get(r, b);
                }
            }
            for (i, &c) in payload.iter().enumerate() {
                row[left.width() + i] = right.get(r, c);
            }
            if accept.is_none_or(|f| f(&row)) {
                out.push(&row)?;
                matched = true;
            }
            Ok(true)
        })?;
        if !matched && keep_unmatched {
            for (i, c) in left.columns().iter().enumerate() {
                row[i] = c[l];
            }
            for value in &mut row[left.width()..] {
                *value = UNDEF;
            }
            out.push(&row)?;
        }
    }
    out.finish()
}

/// Per left row, whether some right row is compatible with it on the keys, which may hold
/// UNDEF on either side. With `bound_in_both`, the two rows must also share a key that
/// both bind: SPARQL's MINUS removes a row only for a solution it has a variable in
/// common with. Without it, this is (NOT) EXISTS.
pub fn compatible_mask(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    bound_in_both: bool,
) -> Vec<bool> {
    let compatible = Compatible::new(right, left_keys, right_keys);
    (0..left.len())
        .map(|l| {
            let mut any = false;
            let _ = compatible.each(left, l, |r| {
                let shares = !bound_in_both
                    || left_keys
                        .iter()
                        .zip(right_keys)
                        .any(|(&a, &b)| left.get(l, a) != UNDEF && right.get(r, b) != UNDEF);
                any |= shares;
                Ok(!any)
            });
            any
        })
        .collect()
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
                sorted_rows(&join(&left, &right, &lk, &rk, usize::MAX).unwrap()),
                expected,
                "hash, case {case}"
            );
            left.sort_by(&lk);
            right.sort_by(&rk);
            let merged = join(&left, &right, &lk, &rk, usize::MAX).unwrap();
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
            let got = join_with_undef(&left, &right, &[0, 1], &[1, 0], usize::MAX).unwrap();
            assert_eq!(
                sorted_rows(&got),
                naive(&left, &right, &[0, 1], &[1, 0]),
                "case {case}"
            );
        }
    }

    /// The joins over keys that may be unbound, against their definitions: every compatible
    /// pair (UNDEF matches anything, the bound value is kept), left rows without a match
    /// once for the outer join, and for MINUS a compatible row that shares a bound key.
    #[test]
    fn outer_joins_and_masks_with_undef_match_the_definitions() {
        let mut rng = Rng(11);
        for case in 0..400 {
            let (left_rows, right_rows) = (rng.below(16) as usize, rng.below(16) as usize);
            let left = random_table(&mut rng, 3, left_rows, 3, true);
            let right = random_table(&mut rng, 3, right_rows, 3, true);
            let (lk, rk) = ([0usize, 1], [1usize, 0]);
            let compatible = |l: &[u64], r: &[u64]| {
                lk.iter()
                    .zip(&rk)
                    .all(|(&a, &b)| l[a] == r[b] || l[a] == UNDEF || r[b] == UNDEF)
            };
            // The condition of an OPTIONAL, on the combined row.
            let accept = |row: &[u64]| row[2] != row[3];
            let filter: Option<RowFilter<'_>> = (case % 2 == 0).then_some(&accept);
            let mut inner = Vec::new();
            let mut outer = Vec::new();
            for l in left.rows() {
                let mut matched = false;
                for r in right.rows().filter(|r| compatible(&l, r)) {
                    let mut row = l.clone();
                    for (&a, &b) in lk.iter().zip(&rk) {
                        if row[a] == UNDEF {
                            row[a] = r[b];
                        }
                    }
                    row.push(r[2]);
                    if filter.is_none_or(|f| f(&row)) {
                        inner.push(row.clone());
                        outer.push(row);
                        matched = true;
                    }
                }
                if !matched {
                    let mut row = l.clone();
                    row.push(UNDEF);
                    outer.push(row);
                }
            }
            let sorted = |mut rows: Vec<Vec<u64>>| {
                rows.sort();
                rows
            };
            let got = outer_join_with_undef(&left, &right, &lk, &rk, filter, true, usize::MAX);
            assert_eq!(
                sorted_rows(&got.unwrap()),
                sorted(outer),
                "outer, case {case}"
            );
            let got = outer_join_with_undef(&left, &right, &lk, &rk, filter, false, usize::MAX);
            let got = got.unwrap();
            assert_eq!(sorted_rows(&got), sorted(inner), "inner, case {case}");
            // The left order is kept: the left columns of the output follow the input.
            if filter.is_none() {
                let mut position = 0;
                for row in got.rows() {
                    while !left
                        .row(position)
                        .iter()
                        .zip(&row)
                        .all(|(l, o)| l == o || *l == UNDEF)
                    {
                        position += 1;
                    }
                }
            }

            for bound_in_both in [false, true] {
                let expected: Vec<bool> = left
                    .rows()
                    .map(|l| {
                        right.rows().any(|r| {
                            compatible(&l, &r)
                                && (!bound_in_both
                                    || lk
                                        .iter()
                                        .zip(&rk)
                                        .any(|(&a, &b)| l[a] != UNDEF && r[b] != UNDEF))
                        })
                    })
                    .collect();
                assert_eq!(
                    compatible_mask(&left, &right, &lk, &rk, bound_in_both),
                    expected,
                    "mask (bound in both: {bound_in_both}), case {case}"
                );
            }
        }
        // The row limit applies.
        let many = IdTable::from_rows(1, (0..100).map(|_| &[UNDEF][..]));
        assert!(outer_join_with_undef(&many, &many, &[0], &[0], None, true, 99).is_err());
    }

    #[test]
    fn left_and_anti_joins() {
        let left = IdTable::from_rows(2, [&[1, 10][..], &[2, 20], &[3, 30]]);
        let right = IdTable::from_rows(2, [&[1, 100][..], &[1, 101], &[3, 300]]);
        let out = left_join(&left, &right, &[0], &[0], None, usize::MAX).unwrap();
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
            usize::MAX,
        )
        .unwrap();
        assert_eq!(
            sorted_rows(&filtered),
            vec![vec![1, 10, 100], vec![2, 20, UNDEF], vec![3, 30, UNDEF]]
        );
        let anti = anti_join(&left, &right, &[0], &[0]);
        assert_eq!(sorted_rows(&anti), vec![vec![2, 20]]);
    }

    /// Inputs large enough to run in parallel chunks give the sequential output, in the
    /// sequential order.
    #[test]
    fn parallel_joins_equal_sequential() {
        let mut rng = Rng(11);
        let rows = 5 * PARALLEL_ROWS;
        let mut left = random_table(&mut rng, 2, rows, rows as u64, false);
        let mut right = random_table(&mut rng, 2, rows / 2, rows as u64, false);
        let (lk, rk) = ([0], [1]);

        let hashed = join(&left, &right, &lk, &rk, usize::MAX).unwrap();
        left.sort_by(&lk);
        right.sort_by(&rk);
        let merged = join(&left, &right, &lk, &rk, usize::MAX).unwrap();
        assert_eq!(
            merged,
            merge_rows(&left, 0..rows, &right, 0, &lk, &rk, &Limit::new(usize::MAX))
                .unwrap()
                .assume_sorted_by(lk.to_vec())
        );
        assert!(merged.len() > rows / 4, "the join must produce rows");
        assert_eq!(sorted_rows(&hashed), sorted_rows(&merged));

        let accept_all: &dyn Fn(&[u64]) -> bool = &|_| true;
        assert_eq!(
            left_join(&left, &right, &lk, &rk, None, usize::MAX).unwrap(),
            left_join(&left, &right, &lk, &rk, Some(accept_all), usize::MAX).unwrap()
        );

        let table = BuildTable::new(&right, &rk);
        let mut anti = left.clone();
        anti.retain(|t, row| table.first(t, row, &lk) == END);
        assert_eq!(anti_join(&left, &right, &lk, &rk), anti);
        let mut semi = left.clone();
        semi.retain(|t, row| table.first(t, row, &lk) != END);
        assert_eq!(semi_join(&left, &right, &lk, &rk), semi);
    }

    #[test]
    fn joins_stop_at_their_row_limit() {
        let mut rng = Rng(3);
        let left = random_table(&mut rng, 2, 3000, 4, false);
        let right = random_table(&mut rng, 2, 3000, 4, false);
        let full = join(&left, &right, &[0], &[0], usize::MAX).unwrap();
        assert!(full.len() > 1_000_000);
        assert_eq!(
            join(&left, &right, &[0], &[0], 100_000),
            Err(TooManyRows { max_rows: 100_000 })
        );
        assert_eq!(
            join(&left, &right, &[], &[], 1_000_000).map(|t| t.len()),
            Err(TooManyRows {
                max_rows: 1_000_000
            })
        );
        assert_eq!(
            join(&left, &right, &[0], &[0], full.len()).map(|t| t.len()),
            Ok(full.len())
        );
        assert!(left_join(&left, &right, &[0], &[0], None, 1000).is_err());
        assert!(join_with_undef(&left, &right, &[0], &[0], 1000).is_err());
    }

    #[test]
    fn a_set_stop_flag_stops_every_join() {
        let mut rng = Rng(5);
        let left = random_table(&mut rng, 2, 3000, 4, false);
        let right = random_table(&mut rng, 2, 3000, 4, false);
        let stopped = || RowLimit {
            max_rows: usize::MAX,
            stop: Some(Arc::new(AtomicBool::new(true))),
        };
        assert!(join(&left, &right, &[0], &[0], stopped()).is_err());
        assert!(join(&left, &right, &[], &[], stopped()).is_err());
        assert!(join_keeping_left_order(&left, &right, &[0], &[0], stopped()).is_err());
        assert!(left_join(&left, &right, &[0], &[0], None, stopped()).is_err());
        assert!(join_with_undef(&left, &right, &[0], &[0], stopped()).is_err());
        // An unset flag changes nothing.
        let running = RowLimit {
            max_rows: usize::MAX,
            stop: Some(Arc::new(AtomicBool::new(false))),
        };
        assert_eq!(
            join(&left, &right, &[0], &[0], running).map(|t| t.len()),
            join(&left, &right, &[0], &[0], usize::MAX).map(|t| t.len())
        );
    }
}
