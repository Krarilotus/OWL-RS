//! Joins on key columns.
//!
//! Every join has the same output schema: all columns of `left`, then the columns of
//! `right` that aren't join keys, in order ([`output_columns`]). A key column appears once.
//!
//! | Function | Algorithm | Use |
//! |---|---|---|
//! | [`join`] | merge join with galloping if both inputs are sorted on the keys, else a hash join building on the smaller input | inner joins without UNDEF keys |
//! | [`left_join`] | hash join, probing with every left row | OPTIONAL; a filter over the combined row decides which matches count |
//! | [`anti_join`], [`semi_join`] | a merge if both inputs are sorted on the keys, else a hash set of right keys | MINUS and FILTER NOT EXISTS, FILTER EXISTS, with shared variables |
//! | [`join_with_undef`] | the fast join for rows with bound keys, nested loops for rows with UNDEF | inner joins where a key may be unbound (after OPTIONAL) |
//! | [`outer_join_with_undef`] | a hash table for rows with bound keys, scans for rows with UNDEF; left order kept | OPTIONAL, and joins that keep the left order, where a key may be unbound |
//! | [`compatible_mask`] | the same | MINUS and (NOT) EXISTS where a key may be unbound |
//!
//! Key values must not be [`UNDEF`](crate::UNDEF) except in the `*_with_undef` functions,
//! in [`compatible_mask`], and on the right side of [`left_join`]'s output.
//!
//! Their output tables and their working memory (hash chains, pairs of row numbers, masks)
//! are charged to the `budget` they are given before they grow ([`IdTable::reserve_rows`],
//! [`Scratch`]): a join the budget has no room for stops with [`JoinStop::Memory`] before it
//! allocates. Joins that can multiply rows also take a row limit and stop with
//! [`JoinStop::Rows`] past it, or once the query is cancelled.

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

use hashbrown::HashMap;
use rayon::prelude::*;

use crate::search::partition_range_from;
use crate::table::IdTable;
use crate::{Budget, BudgetExceeded, ChargedVec, Reservation, Scratch, UNDEF, map_bytes};

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

/// Why a join stopped before its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinStop {
    /// Past its row limit, or cancelled.
    Rows(TooManyRows),
    /// Its budget has no room for its output or working memory.
    Memory(BudgetExceeded),
}

impl From<TooManyRows> for JoinStop {
    fn from(rows: TooManyRows) -> Self {
        Self::Rows(rows)
    }
}

impl From<BudgetExceeded> for JoinStop {
    fn from(memory: BudgetExceeded) -> Self {
        Self::Memory(memory)
    }
}

impl std::fmt::Display for JoinStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rows(rows) => rows.fmt(f),
            Self::Memory(memory) => memory.fmt(f),
        }
    }
}

impl std::error::Error for JoinStop {}

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

/// An output table written into room reserved (and charged) ahead, its rows counted against
/// a [`Limit`] every [`LIMIT_STEP`] rows: a row costs one comparison, and the room and the
/// limit are seen to between steps.
struct Sink<'a> {
    out: IdTable,
    /// Rows counted against the limit.
    counted: usize,
    /// The row at which the next step is due: at most the room, and `LIMIT_STEP` past the
    /// last count.
    due: usize,
    limit: &'a Limit,
    budget: &'a Arc<Budget>,
}

impl<'a> Sink<'a> {
    fn new(
        width: usize,
        limit: &'a Limit,
        budget: &'a Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        Ok(Self::after(
            IdTable::charged(width, 0, budget)?,
            limit,
            budget,
        ))
    }

    /// Rows appended to `out`, whose own rows are counted already.
    fn after(out: IdTable, limit: &'a Limit, budget: &'a Arc<Budget>) -> Self {
        let counted = out.len();
        Self {
            out,
            counted,
            due: counted,
            limit,
            budget,
        }
    }

    #[inline]
    fn push(&mut self, row: &[u64]) -> Result<(), JoinStop> {
        if self.out.len() == self.due {
            self.step()?;
        }
        self.out.push_reserved(row);
        Ok(())
    }

    /// The rows since the last step counted, and room reserved where none is left.
    #[cold]
    fn step(&mut self) -> Result<(), JoinStop> {
        let len = self.out.len();
        self.limit.grow(len - self.counted)?;
        self.counted = len;
        if self.out.room_left() == 0 {
            self.out.reserve_rows(LIMIT_STEP, self.budget)?;
        }
        self.due = len + self.out.room_left().min(LIMIT_STEP);
        Ok(())
    }

    fn finish(self) -> Result<IdTable, JoinStop> {
        self.limit.grow(self.out.len() - self.counted)?;
        Ok(self.out)
    }
}

/// Runs `part` over consecutive row ranges of a `len`-row input, in parallel for large
/// inputs, and concatenates the parts in input order (so the output order is the
/// sequential one), charged to `budget`.
fn chunked(
    len: usize,
    width: usize,
    budget: &Arc<Budget>,
    part: impl Fn(Range<usize>) -> Result<IdTable, JoinStop> + Sync,
) -> Result<IdTable, JoinStop> {
    if len < 2 * PARALLEL_ROWS {
        return part(0..len);
    }
    let count = len.div_ceil(PARALLEL_ROWS);
    let mut parts: Scratch<'_, IdTable> = ChargedVec::with_capacity(&**budget, count)?;
    parts.extend_bounded(count, std::iter::repeat_with(IdTable::default))?;
    parts.par_iter_mut().enumerate().try_for_each(|(i, slot)| {
        let rows = part(i * PARALLEL_ROWS..((i + 1) * PARALLEL_ROWS).min(len))?;
        // A part without rows is dropped here: it never grew, and holds nothing.
        if !rows.is_empty() {
            *slot = rows;
        }
        Ok::<_, JoinStop>(())
    })?;
    Ok(IdTable::concat_charged(width, &mut parts, budget)?)
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

/// Inner join of `left` and `right` on `left_keys[i] = right_keys[i]`, charged to `budget`,
/// of at most `limit` rows.
pub fn join(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    budget: &Arc<Budget>,
    limit: impl Into<RowLimit>,
) -> Result<IdTable, JoinStop> {
    assert_eq!(left_keys.len(), right_keys.len(), "join key arity");
    let limit = Limit::new(limit);
    if left_keys.is_empty() {
        return cross_product(left, right, budget, &limit);
    }
    if left.is_sorted_on(left_keys) && right.is_sorted_on(right_keys) {
        merge_join(left, right, left_keys, right_keys, budget, &limit)
    } else if left.len() <= right.len() {
        // Build on the smaller side; the output schema stays left-first either way.
        hash_join(right, left, right_keys, left_keys, true, budget, &limit)
    } else {
        hash_join(left, right, left_keys, right_keys, false, budget, &limit)
    }
}

/// Inner join whose output keeps `left`'s row order (for a left side ordered by ORDER BY):
/// a hash join building on `right` and probing with every left row in order.
pub fn join_keeping_left_order(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    budget: &Arc<Budget>,
    limit: impl Into<RowLimit>,
) -> Result<IdTable, JoinStop> {
    assert_eq!(left_keys.len(), right_keys.len(), "join key arity");
    let limit = Limit::new(limit);
    if left_keys.is_empty() {
        return cross_product(left, right, budget, &limit);
    }
    hash_join(left, right, left_keys, right_keys, false, budget, &limit)
}

fn cross_product(
    left: &IdTable,
    right: &IdTable,
    budget: &Arc<Budget>,
    limit: &Limit,
) -> Result<IdTable, JoinStop> {
    // The size is known: refuse before producing anything.
    if left.len().saturating_mul(right.len()) > limit.max_rows {
        return Err(TooManyRows {
            max_rows: limit.max_rows,
        }
        .into());
    }
    let width = left.width() + right.width();
    chunked(left.len(), width, budget, |lefts| {
        let mut out = Sink::new(width, limit, budget)?;
        let mut row = vec![0; width];
        for l in lefts {
            for (i, c) in left.columns().iter().enumerate() {
                row[i] = c[l];
            }
            for r in 0..right.len() {
                for (i, c) in right.columns().iter().enumerate() {
                    row[left.width() + i] = c[r];
                }
                out.push(&row)?;
            }
        }
        out.finish()
    })
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
    let Range {
        start: mut lo,
        end: mut hi,
    } = partition_range_from(table.len(), from, less);
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
    budget: &Arc<Budget>,
    limit: &Limit,
) -> Result<IdTable, JoinStop> {
    let width = left.width() + right_payload(right.width(), rk).len();
    // Each chunk of left rows starts at the first right row with its key.
    let out = chunked(left.len(), width, budget, |rows| {
        let start = if rows.start == 0 {
            0
        } else {
            gallop(right, rk, 0, left, rows.start, lk, false)
        };
        merge_rows(left, rows, right, start, lk, rk, budget, limit)
    })?;
    // The output follows the left input's order, so it is sorted on the left keys.
    Ok(out.assume_sorted_by(lk.to_vec()))
}

/// Merge join of the left rows `rows` with the right rows from `r`: the matching pairs of
/// row numbers first, then the output a column at a time, gathered by them (no row is
/// assembled). One key compares two id columns directly.
#[allow(clippy::too_many_arguments)]
fn merge_rows(
    left: &IdTable,
    rows: Range<usize>,
    right: &IdTable,
    mut r: usize,
    lk: &[usize],
    rk: &[usize],
    budget: &Arc<Budget>,
    limit: &Limit,
) -> Result<IdTable, JoinStop> {
    assert!(
        left.len() < END as usize && right.len() < END as usize,
        "join input exceeds u32 rows"
    );
    let mut pairs = Pairs::new(budget, limit, rows.len());
    let (mut l, end) = (rows.start, rows.end);
    if let ([lk], [rk]) = (lk, rk) {
        let (a, b) = (left.column(*lk), right.column(*rk));
        while l < end && r < b.len() {
            match a[l].cmp(&b[r]) {
                Ordering::Less => l += 1,
                Ordering::Greater => r += 1,
                Ordering::Equal => {
                    let l_end = (l + 1..end).find(|&x| a[x] != a[l]).unwrap_or(end);
                    let r_end = (r + 1..b.len()).find(|&x| b[x] != b[r]).unwrap_or(b.len());
                    pairs.runs(l..l_end, r..r_end)?;
                    l = l_end;
                    r = r_end;
                }
            }
        }
    } else {
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
                    pairs.runs(l..l_end, r..r_end)?;
                    l = l_end;
                    r = r_end;
                }
            }
        }
    }
    pairs.gather(left, right, &right_payload(right.width(), rk))
}

/// The matching pairs of row numbers of a merge, charged before they are made, and counted
/// against the join's limit: a join over its limit (or cancelled) stops within a step
/// instead of after expanding a whole group (the review of 3 October 2026, P1).
struct Pairs<'a> {
    lefts: Scratch<'a, u32>,
    /// [`END`] for a left row without a match.
    rights: Scratch<'a, u32>,
    pending: usize,
    budget: &'a Arc<Budget>,
    limit: &'a Limit,
}

impl<'a> Pairs<'a> {
    /// Pairs of a merge over `rows` left rows: room for one pair each, where the budget has
    /// it, reserved at once (a join of keys that match once makes no more), so the lists
    /// don't grow pair by pair, each growth a charge on budget counters every part shares.
    fn new(budget: &'a Arc<Budget>, limit: &'a Limit, rows: usize) -> Self {
        let mut pairs = Self {
            lefts: ChargedVec::new(&**budget),
            rights: ChargedVec::new(&**budget),
            pending: 0,
            budget,
            limit,
        };
        if rows.saturating_mul(2 * size_of::<u32>()) <= budget.remaining() {
            // Room checked first: a refusal here would only mark the budget refused.
            let _ = pairs.lefts.reserve_exact(rows);
            let _ = pairs.rights.reserve_exact(rows);
        }
        pairs
    }

    /// Each left row of `lefts` with each right row of `rights` (once with none if
    /// `rights` is empty and `unmatched`).
    #[inline(always)]
    fn add(&mut self, l: usize, rights: Range<usize>, unmatched: bool) -> Result<(), JoinStop> {
        let n = rights.len().max(usize::from(unmatched));
        self.pending += n;
        if self.pending >= LIMIT_STEP {
            self.limit.grow(std::mem::take(&mut self.pending))?;
        }
        // Room for the run first, then plain writes: most runs are a pair or two.
        self.lefts.reserve(n)?;
        self.rights.reserve(n)?;
        if rights.is_empty() && unmatched {
            self.lefts.push(l as u32)?;
            self.rights.push(END)?;
        }
        for r in rights {
            self.lefts.push(l as u32)?;
            self.rights.push(r as u32)?;
        }
        Ok(())
    }

    #[inline]
    fn runs(&mut self, mut lefts: Range<usize>, rights: Range<usize>) -> Result<(), JoinStop> {
        lefts.try_for_each(|l| self.add(l, rights.clone(), false))
    }

    /// The output, a column at a time: `left`'s columns, then `right`'s `payload` columns
    /// ([`UNDEF`] where a left row has no match).
    fn gather(
        self,
        left: &IdTable,
        right: &IdTable,
        payload: &[usize],
    ) -> Result<IdTable, JoinStop> {
        self.limit.grow(self.pending)?;
        let (lefts, rights) = (&self.lefts[..], &self.rights[..]);
        let columns = left
            .columns()
            .iter()
            .map(|column| (&column[..], lefts))
            .chain(payload.iter().map(|&c| (right.column(c), rights)))
            .map(|(column, at)| {
                at.iter()
                    .map(move |&i| if i == END { UNDEF } else { column[i as usize] })
            });
        let width = left.width() + payload.len();
        Ok(IdTable::gathered(width, lefts.len(), self.budget, columns)?)
    }
}

/// Hash table from key to the chain of `build` rows with that key: `heads[key]` is the first
/// row, `next[row]` the following one (`u32::MAX` ends the chain). No allocation per key
/// beyond a multi-column key's values. Its memory is reserved before it is allocated.
struct BuildTable<'b> {
    heads: HashMap<Vec<u64>, u32, Hasher>,
    single: HashMap<u64, u32, Hasher>,
    next: Scratch<'b, u32>,
    one_key: bool,
    /// The maps' memory and the keys': after them, so that it is released after they are
    /// freed.
    held: Reservation<&'b Budget>,
}

const END: u32 = u32::MAX;

impl<'b> BuildTable<'b> {
    /// `build`'s rows that `include` keeps, chained by their key on `keys`.
    fn new(
        build: &IdTable,
        keys: &[usize],
        budget: &'b Budget,
        include: impl Fn(usize) -> bool,
    ) -> Result<Self, BudgetExceeded> {
        assert!(build.len() < END as usize, "join input exceeds u32 rows");
        let rows = build.len();
        let one_key = keys.len() == 1;
        let mut held = Reservation::new(budget);
        let key_bytes = keys.len() * size_of::<u64>();
        held.charge(match one_key {
            true => map_bytes::<(u64, u32)>(rows),
            false => map_bytes::<(Vec<u64>, u32)>(rows).saturating_add(rows * key_bytes),
        })?;
        let mut next = ChargedVec::with_capacity(budget, rows)?;
        next.resize(rows, END)?;
        let mut table = Self {
            heads: HashMap::with_capacity_and_hasher(
                if one_key { 0 } else { rows },
                Hasher::default(),
            ),
            single: HashMap::with_capacity_and_hasher(
                if one_key { rows } else { 0 },
                Hasher::default(),
            ),
            next,
            one_key,
            held,
        };
        // Insert in reverse so chains list rows in input order.
        for row in (0..rows).rev().filter(|&row| include(row)) {
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
        // What the maps and the keys hold, within what was reserved.
        let used = table.single.allocation_size()
            + table.heads.allocation_size()
            + table.heads.len() * key_bytes;
        debug_assert!(
            used <= table.held.bytes(),
            "{used} past {}",
            table.held.bytes()
        );
        table.held.resize(used.min(table.held.bytes()))?;
        Ok(table)
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
    budget: &Arc<Budget>,
    limit: &Limit,
) -> Result<IdTable, JoinStop> {
    let table = BuildTable::new(build, build_keys, budget, |_| true)?;
    let (left, right, rk) = if build_is_left {
        (build, probe, probe_keys)
    } else {
        (probe, build, build_keys)
    };
    let payload = right_payload(right.width(), rk);
    let width = left.width() + payload.len();
    let out = chunked(probe.len(), width, budget, |rows| {
        let mut out = Sink::new(width, limit, budget)?;
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
    budget: &Arc<Budget>,
    limit: impl Into<RowLimit>,
) -> Result<IdTable, JoinStop> {
    let width = left.width() + right_payload(right.width(), right_keys).len();
    let limit = Limit::new(limit);
    // One key, both sides sorted on it, no filter: a merge (DBpedia q9 joined 100 k
    // politicians to 363 k death dates by hashing the dates).
    if accept.is_none()
        && let ([lk], [rk]) = (left_keys, right_keys)
        && left.is_sorted_on(left_keys)
        && right.is_sorted_on(right_keys)
    {
        let out = sorted_left_join(left, right, *lk, *rk, budget, &limit)?;
        return Ok(out.assume_sorted_by(left_keys.to_vec()));
    }
    let table = match left_keys.is_empty() {
        true => None,
        false => Some(BuildTable::new(right, right_keys, budget, |_| true)?),
    };
    let inputs = LeftJoin {
        left,
        right,
        left_keys,
        right_keys,
        table: table.as_ref(),
    };
    let out = match accept {
        // The filter may not be thread-safe (it decodes terms through a cache).
        Some(accept) => left_join_rows(&inputs, 0..left.len(), Some(accept), budget, &limit)?,
        None => chunked(left.len(), width, budget, |rows| {
            left_join_rows(&inputs, rows, None, budget, &limit)
        })?,
    };
    Ok(if left.is_sorted_on(left_keys) && !left_keys.is_empty() {
        out.assume_sorted_by(left_keys.to_vec())
    } else {
        out
    })
}

/// [`left_join`] of inputs sorted on one key each, without a filter: the pairs of row
/// numbers (a left row without a match pairs with none), then the output a column at a
/// time.
fn sorted_left_join(
    left: &IdTable,
    right: &IdTable,
    lk: usize,
    rk: usize,
    budget: &Arc<Budget>,
    limit: &Limit,
) -> Result<IdTable, JoinStop> {
    assert!(
        left.len() < END as usize && right.len() < END as usize,
        "join input exceeds u32 rows"
    );
    let (a, b) = (left.column(lk), right.column(rk));
    let mut pairs = Pairs::new(budget, limit, a.len());
    let (mut l, mut r) = (0, 0);
    while l < a.len() {
        let key = a[l];
        let l_end = (l + 1..a.len()).find(|&x| a[x] != key).unwrap_or(a.len());
        while r < b.len() && b[r] < key {
            r += 1;
        }
        let r_end = (r..b.len()).find(|&x| b[x] != key).unwrap_or(b.len());
        for li in l..l_end {
            pairs.add(li, r..r_end, true)?;
        }
        l = l_end;
        r = r_end;
    }
    pairs.gather(left, right, &right_payload(right.width(), &[rk]))
}

/// The inputs of a [`left_join`], shared by its parallel parts.
struct LeftJoin<'a> {
    left: &'a IdTable,
    right: &'a IdTable,
    left_keys: &'a [usize],
    right_keys: &'a [usize],
    /// `right` hashed on its keys; `None` without keys (every row matches).
    table: Option<&'a BuildTable<'a>>,
}

/// [`left_join`] of the left rows `rows`.
fn left_join_rows(
    join: &LeftJoin<'_>,
    rows: Range<usize>,
    accept: Option<RowFilter<'_>>,
    budget: &Arc<Budget>,
    limit: &Limit,
) -> Result<IdTable, JoinStop> {
    let LeftJoin {
        left,
        right,
        left_keys,
        right_keys,
        table,
    } = *join;
    let payload = right_payload(right.width(), right_keys);
    let width = left.width() + payload.len();
    let mut out = Sink::new(width, limit, budget)?;
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
            Ok::<_, JoinStop>(())
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

/// Left rows whose key doesn't occur in `right` (MINUS, FILTER NOT EXISTS), copied into a
/// table charged to `budget`. Order and sortedness of `left` are kept.
pub fn anti_join(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    budget: &Arc<Budget>,
) -> Result<IdTable, BudgetExceeded> {
    let mut out = left.try_clone(budget)?;
    anti_join_in_place(&mut out, right, left_keys, right_keys, budget)?;
    Ok(out)
}

/// [`anti_join`] of a table the caller owns, filtered where it is: no copy of it. Its
/// working memory is charged to `budget`.
pub fn anti_join_in_place(
    left: &mut IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    budget: &Budget,
) -> Result<(), BudgetExceeded> {
    if right.is_empty() {
        return Ok(());
    }
    if left_keys.is_empty() {
        left.slice(0, Some(0));
        return Ok(());
    }
    keep_members(left, right, left_keys, right_keys, false, budget)
}

/// The left rows whose key occurring in `right` is `wanted`, kept in place: a merge where
/// both are sorted on their keys ([`sorted_membership`]), else a probe per row into a hash
/// table of `right` (in parallel for large inputs, its mask charged to `left`'s budget,
/// [`IdTable::par_retain`]).
fn keep_members(
    left: &mut IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    wanted: bool,
    budget: &Budget,
) -> Result<(), BudgetExceeded> {
    if let Some(keep) = sorted_membership(left, right, left_keys, right_keys, wanted, budget)? {
        left.retain_mask(&keep);
        return Ok(());
    }
    let table = BuildTable::new(right, right_keys, budget, |_| true)?;
    left.par_retain(|t, row| (table.first(t, row, left_keys) != END) == wanted)
}

/// A mask of `len` rows, each `value`, charged to `budget` first.
fn mask(len: usize, value: bool, budget: &Budget) -> Result<Scratch<'_, bool>, BudgetExceeded> {
    let mut mask = ChargedVec::with_capacity(budget, len)?;
    mask.resize(len, value)?;
    Ok(mask)
}

/// Per left row, whether its key occurring in `right` is `wanted`, where both are sorted
/// on their keys: one merge, galloping over runs, instead of a hash table of `right`
/// (`None` otherwise). Wikidata lexemes: 432 k entries against the 246 k senses' entries.
fn sorted_membership<'b>(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    wanted: bool,
    budget: &'b Budget,
) -> Result<Option<Scratch<'b, bool>>, BudgetExceeded> {
    if !left.is_sorted_on(left_keys) || !right.is_sorted_on(right_keys) {
        return Ok(None);
    }
    let mut keep = mask(left.len(), !wanted, budget)?;
    // One key: two columns of ids, merged in one linear pass without branches to
    // mispredict. Each step advances the smaller side; a left key equal to the right one
    // is found, and the right one stays for the next left key. A left row is written
    // again until its key is passed, so the last write is its answer. DBpedia q13: 298 k
    // people against 363 k death dates' subjects.
    // Each step waits for the last one's comparison, so large inputs are cut into parts
    // merged in parallel, each from its first key's place in `right`.
    if let ([lk], [rk]) = (left_keys, right_keys) {
        const PART: usize = 1 << 15;
        let (keys, against) = (left.column(*lk), right.column(*rk));
        let merge = |keys: &[u64], keep: &mut [bool]| {
            let Some(&first) = keys.first() else {
                return;
            };
            let (mut l, mut r) = (0, against.partition_point(|&b| b < first));
            while l < keys.len() && r < against.len() {
                let (a, b) = (keys[l], against[r]);
                keep[l] = (a == b) == wanted;
                l += usize::from(a <= b);
                r += usize::from(b < a);
            }
        };
        if keys.len() < 2 * PART {
            merge(keys, &mut keep);
        } else {
            keep.par_chunks_mut(PART)
                .zip(keys.par_chunks(PART))
                .for_each(|(keep, keys)| merge(keys, keep));
        }
        return Ok(Some(keep));
    }
    let (mut l, mut r) = (0, 0);
    while l < left.len() && r < right.len() {
        match compare_keys(left, l, left_keys, right, r, right_keys) {
            Ordering::Less => l = gallop(left, left_keys, l, right, r, right_keys, true),
            Ordering::Greater => r = gallop(right, right_keys, r, left, l, left_keys, false),
            Ordering::Equal => {
                // The next left rows may have the same key: `r` stays.
                keep[l] = wanted;
                l += 1;
            }
        }
    }
    Ok(Some(keep))
}

/// Left rows whose key occurs in `right` (FILTER EXISTS), copied into a table charged to
/// `budget`. Order and sortedness of `left` are kept; each left row appears at most once.
pub fn semi_join(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    budget: &Arc<Budget>,
) -> Result<IdTable, BudgetExceeded> {
    let mut out = left.try_clone(budget)?;
    semi_join_in_place(&mut out, right, left_keys, right_keys, budget)?;
    Ok(out)
}

/// [`semi_join`] of a table the caller owns, filtered where it is: no copy of it. Its
/// working memory is charged to `budget`.
pub fn semi_join_in_place(
    left: &mut IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    budget: &Budget,
) -> Result<(), BudgetExceeded> {
    if left_keys.is_empty() {
        if right.is_empty() {
            left.slice(0, Some(0));
        }
        return Ok(());
    }
    keep_members(left, right, left_keys, right_keys, true, budget)
}

/// Inner join where key columns may hold UNDEF (SPARQL compatibility: UNDEF matches any
/// value, and the output takes the bound one). Rows with bound keys on both sides go
/// through [`join`]; rows with an UNDEF key are matched by nested loops.
pub fn join_with_undef(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    budget: &Arc<Budget>,
    limit: impl Into<RowLimit>,
) -> Result<IdTable, JoinStop> {
    // The rows with every key bound, and those with one unbound, each copied (charged).
    let split = |t: &IdTable, keys: &[usize]| -> Result<_, BudgetExceeded> {
        let unbound = |row: usize| keys.iter().any(|&k| t.get(row, k) == UNDEF);
        let open = ChargedVec::collect(&**budget, t.len(), (0..t.len()).map(unbound))?;
        Ok((
            t.selected(&open, false, budget)?,
            t.selected(&open, true, budget)?,
        ))
    };
    let (left_bound, left_open) = split(left, left_keys)?;
    let (right_bound, right_open) = split(right, right_keys)?;
    let limit = limit.into();
    let joined = join(
        &left_bound,
        &right_bound,
        left_keys,
        right_keys,
        budget,
        limit.clone(),
    )?;
    let limit = Limit::new(limit);
    limit.grow(joined.len())?;
    let payload = right_payload(right.width(), right_keys);
    let mut row = vec![0; joined.width()];
    let mut out = Sink::after(joined, &limit, budget);
    let mut nested = |l_table: &IdTable, r_table: &IdTable| -> Result<(), JoinStop> {
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

/// `right` for joins whose keys may be UNDEF: its rows with every key bound chained by key,
/// and the rows with an unbound key, which can match any left row.
struct Compatible<'a> {
    right: &'a IdTable,
    left_keys: &'a [usize],
    right_keys: &'a [usize],
    bound: BuildTable<'a>,
    open: Scratch<'a, u32>,
}

impl<'a> Compatible<'a> {
    fn new(
        right: &'a IdTable,
        left_keys: &'a [usize],
        right_keys: &'a [usize],
        budget: &'a Budget,
    ) -> Result<Self, BudgetExceeded> {
        let unbound = |r: usize| right_keys.iter().any(|&k| right.get(r, k) == UNDEF);
        let bound = BuildTable::new(right, right_keys, budget, |r| !unbound(r))?;
        let open_rows = (0..right.len()).filter(|&r| unbound(r)).count();
        let open = ChargedVec::collect(
            budget,
            open_rows,
            (0..right.len()).filter(|&r| unbound(r)).map(|r| r as u32),
        )?;
        Ok(Self {
            right,
            left_keys,
            right_keys,
            bound,
            open,
        })
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
        mut found: impl FnMut(usize) -> Result<bool, JoinStop>,
    ) -> Result<(), JoinStop> {
        if self.left_keys.iter().any(|&k| left.get(l, k) == UNDEF) {
            for r in 0..self.right.len() {
                if self.agree(left, l, r) && !found(r)? {
                    return Ok(());
                }
            }
            return Ok(());
        }
        let mut r = self.bound.first(left, l, self.left_keys);
        while r != END {
            if !found(r as usize)? {
                return Ok(());
            }
            r = self.bound.next[r as usize];
        }
        for &r in self.open.iter() {
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
#[allow(clippy::too_many_arguments)]
pub fn outer_join_with_undef(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    accept: Option<RowFilter<'_>>,
    keep_unmatched: bool,
    budget: &Arc<Budget>,
    limit: impl Into<RowLimit>,
) -> Result<IdTable, JoinStop> {
    let payload = right_payload(right.width(), right_keys);
    let width = left.width() + payload.len();
    let limit = Limit::new(limit);
    let mut out = Sink::new(width, &limit, budget)?;
    let compatible = Compatible::new(right, left_keys, right_keys, budget)?;
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

/// Per left row, into `found`, whether some right row is compatible with it on the keys,
/// which may hold UNDEF on either side. With `bound_in_both`, the two rows must also share a
/// key that both bind: SPARQL's MINUS removes a row only for a solution it has a variable in
/// common with. Without it, this is (NOT) EXISTS. Its working memory is charged to
/// `budget`; `found` is the caller's.
pub fn compatible_mask(
    left: &IdTable,
    right: &IdTable,
    left_keys: &[usize],
    right_keys: &[usize],
    bound_in_both: bool,
    budget: &Budget,
    found: &mut [bool],
) -> Result<(), BudgetExceeded> {
    assert_eq!(found.len(), left.len(), "mask length");
    let compatible = Compatible::new(right, left_keys, right_keys, budget)?;
    for (l, any) in found.iter_mut().enumerate() {
        *any = false;
        let _ = compatible.each(left, l, |r| {
            let shares = !bound_in_both
                || left_keys
                    .iter()
                    .zip(right_keys)
                    .any(|(&a, &b)| left.get(l, a) != UNDEF && right.get(r, b) != UNDEF);
            *any |= shares;
            Ok(!*any)
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> Arc<Budget> {
        Arc::new(Budget::unlimited())
    }

    /// A table of `columns`, charged to an unlimited budget.
    fn of(columns: Vec<Vec<u64>>) -> IdTable {
        let rows = columns.first().map_or(0, Vec::len);
        IdTable::gathered(columns.len(), rows, &budget(), columns).unwrap()
    }

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
            t.push_row(&row, &crate::test_budget()).unwrap();
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
                sorted_rows(&join(&left, &right, &lk, &rk, &budget(), usize::MAX).unwrap()),
                expected,
                "hash, case {case}"
            );
            left.sort_by(&lk).unwrap();
            right.sort_by(&rk).unwrap();
            let merged = join(&left, &right, &lk, &rk, &budget(), usize::MAX).unwrap();
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
            let got =
                join_with_undef(&left, &right, &[0, 1], &[1, 0], &budget(), usize::MAX).unwrap();
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
            let got =
                outer_join_with_undef(&left, &right, &lk, &rk, filter, true, &budget(), usize::MAX);
            assert_eq!(
                sorted_rows(&got.unwrap()),
                sorted(outer),
                "outer, case {case}"
            );
            let got = outer_join_with_undef(
                &left,
                &right,
                &lk,
                &rk,
                filter,
                false,
                &budget(),
                usize::MAX,
            );
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
                let mut found = vec![true; left.len()];
                compatible_mask(
                    &left,
                    &right,
                    &lk,
                    &rk,
                    bound_in_both,
                    &budget(),
                    &mut found,
                )
                .unwrap();
                assert_eq!(
                    found, expected,
                    "mask (bound in both: {bound_in_both}), case {case}"
                );
            }
        }
        // The row limit applies.
        let many =
            IdTable::from_rows(1, (0..100).map(|_| &[UNDEF][..]), &crate::test_budget()).unwrap();
        assert!(
            outer_join_with_undef(&many, &many, &[0], &[0], None, true, &budget(), 99).is_err()
        );
    }

    #[test]
    fn left_and_anti_joins() {
        let left = IdTable::from_rows(2, [&[1, 10][..], &[2, 20], &[3, 30]], &crate::test_budget())
            .unwrap();
        let right = IdTable::from_rows(
            2,
            [&[1, 100][..], &[1, 101], &[3, 300]],
            &crate::test_budget(),
        )
        .unwrap();
        let out = left_join(&left, &right, &[0], &[0], None, &budget(), usize::MAX).unwrap();
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
            &budget(),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(
            sorted_rows(&filtered),
            vec![vec![1, 10, 100], vec![2, 20, UNDEF], vec![3, 30, UNDEF]]
        );
        let anti = anti_join(&left, &right, &[0], &[0], &budget()).unwrap();
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

        let hashed = join(&left, &right, &lk, &rk, &budget(), usize::MAX).unwrap();
        left.sort_by(&lk).unwrap();
        right.sort_by(&rk).unwrap();
        let merged = join(&left, &right, &lk, &rk, &budget(), usize::MAX).unwrap();
        assert_eq!(
            merged,
            merge_rows(
                &left,
                0..rows,
                &right,
                0,
                &lk,
                &rk,
                &budget(),
                &Limit::new(usize::MAX)
            )
            .unwrap()
            .assume_sorted_by(lk.to_vec())
        );
        assert!(merged.len() > rows / 4, "the join must produce rows");
        assert_eq!(sorted_rows(&hashed), sorted_rows(&merged));

        let accept_all: &dyn Fn(&[u64]) -> bool = &|_| true;
        assert_eq!(
            left_join(&left, &right, &lk, &rk, None, &budget(), usize::MAX).unwrap(),
            left_join(
                &left,
                &right,
                &lk,
                &rk,
                Some(accept_all),
                &budget(),
                usize::MAX
            )
            .unwrap()
        );

        let held = budget();
        let table = BuildTable::new(&right, &rk, &held, |_| true).unwrap();
        let mut anti = left.clone();
        anti.retain(|t, row| table.first(t, row, &lk) == END)
            .unwrap();
        assert_eq!(anti_join(&left, &right, &lk, &rk, &budget()).unwrap(), anti);
        let mut semi = left.clone();
        semi.retain(|t, row| table.first(t, row, &lk) != END)
            .unwrap();
        assert_eq!(semi_join(&left, &right, &lk, &rk, &budget()).unwrap(), semi);
    }

    #[test]
    fn joins_stop_at_their_row_limit() {
        let mut rng = Rng(3);
        let left = random_table(&mut rng, 2, 3000, 4, false);
        let right = random_table(&mut rng, 2, 3000, 4, false);
        let full = join(&left, &right, &[0], &[0], &budget(), usize::MAX).unwrap();
        assert!(full.len() > 1_000_000);
        assert_eq!(
            join(&left, &right, &[0], &[0], &budget(), 100_000),
            Err(JoinStop::Rows(TooManyRows { max_rows: 100_000 }))
        );
        assert_eq!(
            join(&left, &right, &[], &[], &budget(), 1_000_000).map(|t| t.len()),
            Err(JoinStop::Rows(TooManyRows {
                max_rows: 1_000_000
            }))
        );
        assert_eq!(
            join(&left, &right, &[0], &[0], &budget(), full.len()).map(|t| t.len()),
            Ok(full.len())
        );
        assert!(left_join(&left, &right, &[0], &[0], None, &budget(), 1000).is_err());
        assert!(join_with_undef(&left, &right, &[0], &[0], &budget(), 1000).is_err());
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
        assert!(join(&left, &right, &[0], &[0], &budget(), stopped()).is_err());
        assert!(join(&left, &right, &[], &[], &budget(), stopped()).is_err());
        assert!(join_keeping_left_order(&left, &right, &[0], &[0], &budget(), stopped()).is_err());
        assert!(left_join(&left, &right, &[0], &[0], None, &budget(), stopped()).is_err());
        assert!(join_with_undef(&left, &right, &[0], &[0], &budget(), stopped()).is_err());
        // An unset flag changes nothing.
        let running = RowLimit {
            max_rows: usize::MAX,
            stop: Some(Arc::new(AtomicBool::new(false))),
        };
        assert_eq!(
            join(&left, &right, &[0], &[0], &budget(), running).map(|t| t.len()),
            join(&left, &right, &[0], &[0], &budget(), usize::MAX).map(|t| t.len())
        );
    }

    /// What `work` holds beyond what it charged to the budget it's given, on one thread, so
    /// that its parallel parts run one at a time: a join's row buffers and key lists (plan
    /// bookkeeping, the same at every size), never its output or its working memory. All
    /// it charged is released when it ends. The pool lives as long as the process: a pool
    /// dropped ends its thread later, and the thread's end allocates (crossbeam-epoch hands
    /// its garbage on), into whichever measurement runs then.
    fn uncharged(work: impl Fn(&Arc<Budget>) + Send + Sync) -> usize {
        static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
        let pool = POOL.get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap()
        });
        pool.install(|| {
            let budget = budget();
            let ((), excess) = crate::measure::measure_within(&budget, || work(&budget));
            assert_eq!(budget.used(), 0, "released when it ends");
            excess
        })
    }

    /// `rows` rows of distinct keys `0, step, 2·step, …` and a second column.
    fn keyed(rows: usize, step: u64) -> IdTable {
        of(vec![
            (0..rows as u64).map(|i| i * step).collect(),
            (0..rows as u64).collect(),
        ])
        .assume_sorted_by(vec![0])
    }

    /// [`keyed`]'s rows in reverse order, not known to be sorted: made without a parallel
    /// sort, whose pool threads would go on tidying up into the next measurement.
    fn unsorted(rows: usize, step: u64) -> IdTable {
        of(vec![
            (0..rows as u64).rev().map(|i| i * step).collect(),
            (0..rows as u64).rev().collect(),
        ])
    }

    /// Inner joins (merge, hash, cross product) hold nothing uncharged that grows with
    /// their rows: outputs, pairs and hash chains are charged before they are allocated.
    #[test]
    fn inner_joins_hold_no_more_than_they_charge() {
        if !crate::measure::alone() {
            return;
        }
        let held = |rows: usize| {
            let (left, right, unsorted) = (keyed(rows, 1), keyed(rows, 1), unsorted(rows, 1));
            let small = keyed(rows / 100, 1);
            [
                uncharged(|b| drop(join(&left, &right, &[0], &[0], b, usize::MAX).unwrap())),
                uncharged(|b| drop(join(&left, &unsorted, &[0], &[0], b, usize::MAX).unwrap())),
                uncharged(|b| drop(join(&small, &small, &[], &[], b, usize::MAX).unwrap())),
            ]
        };
        assert_eq!(held(10_000), held(100_000));
    }

    /// Left joins (merged on sorted keys, or hashed with a filter) hold nothing uncharged
    /// that grows with the matches of a key.
    #[test]
    fn left_joins_hold_no_more_than_they_charge() {
        if !crate::measure::alone() {
            return;
        }
        let held = |matches: usize| {
            let left = keyed(100, 2);
            // Each even key `matches` times, each odd one once: left rows with and without
            // a match.
            let keys: Vec<u64> = (0..200)
                .flat_map(|k| vec![k; if k % 2 == 0 { matches } else { 1 }])
                .collect();
            let right = of(vec![(0..keys.len() as u64).collect(), keys]).assume_sorted_by(vec![1]);
            let all: &(dyn Fn(&[u64]) -> bool + Sync) = &|_| true;
            [
                uncharged(|b| {
                    drop(left_join(&left, &right, &[0], &[1], None, b, usize::MAX).unwrap())
                }),
                uncharged(|b| {
                    drop(left_join(&left, &right, &[0], &[1], Some(all), b, usize::MAX).unwrap())
                }),
            ]
        };
        assert_eq!(held(10), held(10_000));
    }

    /// Joins whose keys may be unbound hold nothing uncharged that grows with the rows of
    /// an unbound key: the copies of the bound and the unbound rows, the chains of the
    /// bound ones, the list of the unbound ones and the output are charged.
    #[test]
    fn joins_over_unbound_keys_hold_no_more_than_they_charge() {
        if !crate::measure::alone() {
            return;
        }
        let held = |unbound: usize| {
            let keys: Vec<u64> = (0..1_000)
                .chain(std::iter::repeat_n(UNDEF, unbound))
                .collect();
            let rows = keys.len() as u64;
            let left = of(vec![keys.clone(), (0..rows).collect()]);
            let right = of(vec![keys, (0..rows).collect()]);
            let found = std::sync::Mutex::new(vec![false; left.len()]);
            [
                uncharged(|b| {
                    drop(join_with_undef(&left, &right, &[0], &[0], b, usize::MAX).unwrap())
                }),
                uncharged(|b| {
                    let out =
                        outer_join_with_undef(&left, &right, &[0], &[0], None, true, b, usize::MAX);
                    drop(out.unwrap());
                }),
                uncharged(|b| {
                    let mut found = found.lock().unwrap();
                    compatible_mask(&left, &right, &[0], &[0], true, b, &mut found).unwrap();
                }),
            ]
        };
        assert_eq!(held(10), held(300));
    }

    /// Semi- and anti-joins in place hold nothing uncharged that grows with their rows:
    /// their masks and hash chains are charged.
    #[test]
    fn semi_and_anti_joins_hold_no_more_than_they_charge() {
        if !crate::measure::alone() {
            return;
        }
        let held = |rows: usize| {
            let (left, right) = (keyed(rows, 1), keyed(rows / 2, 2));
            let unsorted = unsorted(rows / 2, 2);
            let filtered = |semi: bool, right: &IdTable| {
                uncharged(|b| {
                    // The left side as the executor has it: a table holding its charge.
                    let mut left = left.try_clone(b).unwrap();
                    match semi {
                        true => semi_join_in_place(&mut left, right, &[0], &[0], b).unwrap(),
                        false => anti_join_in_place(&mut left, right, &[0], &[0], b).unwrap(),
                    }
                })
            };
            [
                filtered(true, &right),
                filtered(false, &right),
                filtered(true, &unsorted),
                filtered(false, &unsorted),
            ]
        };
        assert_eq!(held(10_000), held(100_000));
    }
}
