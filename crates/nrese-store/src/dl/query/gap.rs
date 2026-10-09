//! ID-level set difference, with bounded candidate admission. Lower bags stay untouched.

#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasher, Hasher};

use nrese_exec::{Budget, IdTable};
use nrese_sparql::{AlignedSolutions, CancellationToken, QueryEvaluationError};

use crate::StoreResult;

pub(super) struct Gap<'a> {
    pub lower: u64,
    pub total: u64,
    /// Only the admitted prefix plus the bounded diagnostic preview, in legacy priority.
    pub rows: Vec<usize>,
    /// Conservative decoded-row bytes, for reserving each decision batch before decoding.
    pub bytes: Vec<usize>,
    _memory: Scratch<'a>,
}

/// Scratch reservations are released on every exit, including errors/cancellation.
pub(super) struct Scratch<'a> {
    budget: &'a Budget,
    bytes: usize,
}

impl<'a> Scratch<'a> {
    pub fn new(budget: &'a Budget) -> Self {
        Self { budget, bytes: 0 }
    }
    pub fn charge(&mut self, bytes: usize) -> StoreResult<()> {
        self.budget
            .charge(bytes)
            .map_err(QueryEvaluationError::MemoryLimit)?;
        self.bytes += bytes;
        Ok(())
    }
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

fn alive(cancel: &CancellationToken) -> StoreResult<()> {
    if cancel.is_cancelled() {
        return Err(QueryEvaluationError::Cancelled.into());
    }
    Ok(())
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

struct Candidate<'a> {
    row: usize,
    table: &'a IdTable,
    ranks: &'a HashMap<u64, (usize, bool, usize)>,
}

impl Ord for Candidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.table
            .columns()
            .iter()
            .map(|c| self.ranks[&c[self.row]].0)
            .cmp(
                other
                    .table
                    .columns()
                    .iter()
                    .map(|c| other.ranks[&c[other.row]].0),
            )
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
/// supplies deduplication; no decoded gap or per-row formatted keys are collected.
/// The priority of capped candidates remains the old debug-row lexical order. Its
/// compatibility ranks are built once per distinct term, then rows compare integers.
pub(super) fn select<'a>(
    pair: &mut AlignedSolutions,
    limit: usize,
    budget: &'a Budget,
    cancel: &CancellationToken,
) -> StoreResult<Gap<'a>> {
    alive(cancel)?;
    pair.deduplicate_upper()?;
    let mut memory = Scratch::new(budget);
    let (lower, upper) = (pair.lower(), pair.upper());
    memory.charge(lower.len().saturating_mul(40))?;
    let state = std::collections::hash_map::RandomState::new();
    let mut known = hashbrown::HashTable::with_capacity(lower.len());
    for row in 0..lower.len() {
        if row % 1024 == 0 {
            alive(cancel)?;
        }
        let hash = row_hash(lower, row, &state);
        if known.find(hash, |&i| same(lower, i, lower, row)).is_none() {
            known.insert_unique(hash, row, |&i| row_hash(lower, i, &state));
        }
    }
    // Distinct-value rank construction also identifies reserved Skolem names. Keep
    // the legacy namespace exclusion even for terms supplied by a query expression.
    let mut keys = HashMap::new();
    for row in 0..upper.len() {
        if row % 1024 == 0 {
            alive(cancel)?;
        }
        if known
            .find(row_hash(upper, row, &state), |&i| {
                same(lower, i, upper, row)
            })
            .is_some()
        {
            continue;
        }
        for column in upper.columns() {
            let id = column[row];
            if keys.contains_key(&id) {
                continue;
            }
            let term = pair.term(id);
            let internal = matches!(&term, Some(nrese_rdf::Term::NamedNode(n)) if n.as_str().starts_with(super::U1));
            let key = format!("{term:?}");
            memory.charge(key.len().saturating_mul(3).saturating_add(256))?;
            keys.insert(id, (key, internal));
        }
    }
    memory.charge(keys.len().saturating_mul(128))?;
    let mut sorted: Vec<_> = keys.into_iter().collect();
    sorted.sort_unstable_by(|a, b| a.1.0.cmp(&b.1.0));
    let ranks: HashMap<_, _> = sorted
        .into_iter()
        .enumerate()
        .map(|(rank, (id, (key, internal)))| {
            (
                id,
                (
                    rank,
                    internal,
                    key.len().saturating_mul(2).saturating_add(128),
                ),
            )
        })
        .collect();
    let capacity = limit.min(upper.len());
    memory.charge(capacity.saturating_mul(std::mem::size_of::<Candidate<'_>>() + 24))?;
    let mut best = BinaryHeap::with_capacity(capacity);
    let mut total = 0;
    for row in 0..upper.len() {
        if row % 1024 == 0 {
            alive(cancel)?;
        }
        if known
            .find(row_hash(upper, row, &state), |&i| {
                same(lower, i, upper, row)
            })
            .is_some()
            || upper.columns().iter().any(|c| ranks[&c[row]].1)
        {
            continue;
        }
        total += 1;
        if capacity == 0 {
            continue;
        }
        let candidate = Candidate {
            row,
            table: upper,
            ranks: &ranks,
        };
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
        .map(|&r| upper.columns().iter().map(|c| ranks[&c[r]].2).sum())
        .collect();
    let lower = known.len() as u64;
    drop(known);
    drop(ranks);
    let retained =
        (rows.capacity() + bytes.capacity()).saturating_mul(std::mem::size_of::<usize>());
    budget.release(memory.bytes - retained);
    memory.bytes = retained;
    Ok(Gap {
        lower,
        total,
        rows,
        bytes,
        _memory: memory,
    })
}
