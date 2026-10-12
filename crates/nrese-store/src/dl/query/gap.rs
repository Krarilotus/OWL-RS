//! ID-level set difference, with bounded candidate admission. Lower bags stay untouched.

#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasher, Hasher};

use nrese_exec::{Budget, IdTable, Reservation};
use nrese_sparql::{AlignedSolutions, CancellationToken};

use crate::StoreResult;

pub(super) struct Gap<'a> {
    pub lower: u64,
    pub total: u64,
    /// Only the admitted prefix plus the bounded diagnostic preview, in term-id order.
    pub rows: Vec<usize>,
    /// Conservative decoded-row bytes, for reserving each decision batch before decoding.
    pub bytes: Vec<usize>,
    _memory: Reservation<&'a Budget>,
}

fn row_hash(table: &IdTable, row: usize, state: &impl BuildHasher) -> u64 {
    let mut hash = state.build_hasher();
    for column in table.columns() {
        hash.write_u64(column[row]);
    }
    hash.finish()
}

fn same(a: &IdTable, i: usize, b: &IdTable, j: usize) -> bool {
    a.columns()
        .iter()
        .zip(b.columns())
        .all(|(a, b)| a[i] == b[j])
}

/// Capped candidates are admitted in term-id order: arbitrary, but deterministic.
struct Candidate<'a> {
    row: usize,
    table: &'a IdTable,
}

impl Candidate<'_> {
    fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.table.columns().iter().map(|c| c[self.row])
    }
}

impl Ord for Candidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ids().cmp(other.ids())
    }
}
impl PartialOrd for Candidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Candidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate<'_> {}

/// O(U log U + L + U log K), O(L + distinct terms + K) scratch. U's existing table
/// supplies deduplication; no decoded gap is collected. Each distinct term is decoded
/// once, for the reserved-name exclusion and its decoded-row bytes.
pub(super) fn select<'a>(
    pair: &mut AlignedSolutions,
    limit: usize,
    budget: &'a Budget,
    cancel: &CancellationToken,
) -> StoreResult<Gap<'a>> {
    cancel.check()?;
    pair.deduplicate_upper()?;
    #[cfg(test)]
    tests::checkpoint(false);
    cancel.check()?;
    let mut memory = Reservation::new(budget);
    let (lower, upper) = (pair.lower(), pair.upper());
    memory.charge(lower.len().saturating_mul(40))?;
    let state = std::collections::hash_map::RandomState::new();
    let mut known = hashbrown::HashTable::with_capacity(lower.len());
    for row in 0..lower.len() {
        if row % 1024 == 0 {
            cancel.check()?;
        }
        let hash = row_hash(lower, row, &state);
        if known.find(hash, |&i| same(lower, i, lower, row)).is_none() {
            known.insert_unique(hash, row, |&i| row_hash(lower, i, &state));
        }
    }
    let in_lower = |row: usize| {
        known
            .find(row_hash(upper, row, &state), |&i| {
                same(lower, i, upper, row)
            })
            .is_some()
    };
    // Per distinct term of the gap: whether it is a reserved Skolem name (excluded even
    // where a query expression supplies it) and its decoded bytes.
    let mut terms: HashMap<u64, (bool, usize)> = HashMap::new();
    for row in 0..upper.len() {
        if row % 1024 == 0 {
            cancel.check()?;
        }
        if in_lower(row) {
            continue;
        }
        for column in upper.columns() {
            let id = column[row];
            if terms.contains_key(&id) {
                continue;
            }
            let term = pair.term(id);
            let internal = matches!(&term, Some(nrese_rdf::Term::NamedNode(n)) if n.as_str().starts_with(super::U1));
            let bytes = term.as_ref().map_or(0, nrese_sparql::cache::term_heap);
            memory.charge(2 * std::mem::size_of::<(u64, (bool, usize))>())?;
            terms.insert(id, (internal, bytes.saturating_mul(2).saturating_add(128)));
        }
    }
    let capacity = limit.min(upper.len());
    memory.charge(capacity.saturating_mul(std::mem::size_of::<Candidate<'_>>() + 24))?;
    let mut best = BinaryHeap::with_capacity(capacity);
    let mut total = 0;
    for row in 0..upper.len() {
        if row % 1024 == 0 {
            cancel.check()?;
        }
        if in_lower(row) || upper.columns().iter().any(|c| terms[&c[row]].0) {
            continue;
        }
        total += 1;
        if capacity == 0 {
            continue;
        }
        let candidate = Candidate { row, table: upper };
        if best.len() < capacity {
            best.push(candidate);
        } else if best.peek().is_some_and(|last| candidate < *last) {
            best.pop();
            best.push(candidate);
        }
    }
    let rows: Vec<_> = best.into_sorted_vec().into_iter().map(|c| c.row).collect();
    let bytes: Vec<usize> = rows
        .iter()
        .map(|&r| upper.columns().iter().map(|c| terms[&c[r]].1).sum())
        .collect();
    let lower = known.len() as u64;
    drop(known);
    drop(terms);
    let retained =
        (rows.capacity() + bytes.capacity()).saturating_mul(std::mem::size_of::<usize>());
    memory.release(memory.bytes() - retained);
    #[cfg(test)]
    tests::checkpoint(true);
    cancel.check()?;
    Ok(Gap {
        lower,
        total,
        rows,
        bytes,
        _memory: memory,
    })
}
