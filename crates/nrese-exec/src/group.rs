//! Grouping: which rows belong together, for GROUP BY and DISTINCT-like operators.
//!
//! [`group_rows`] returns one key row per group and the group of every input row. Input
//! sorted on the keys is grouped by run length, with no hashing and groups in key order;
//! other input goes through a hash table, with groups in order of first appearance.
//! Aggregates are computed by the caller from the row-to-group map, because most of them
//! (SUM, MIN, AVG) need term values, which this crate doesn't resolve.

use hashbrown::HashMap;

use std::sync::Arc;

use crate::table::IdTable;
use crate::{Budget, BudgetExceeded};

type Hasher = foldhash::fast::FixedState;

/// The rows of each group, in row order within a group: one list of every row ordered by
/// group (a counting sort of the row-to-group map) and where each group's part starts. Two
/// allocations however many groups there are: a vector per group cost a heap allocation
/// each (Sparqloscope 060 grouped 30.5 M rows into as many groups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    rows: Vec<usize>,
    starts: Vec<usize>,
}

impl Members {
    /// The members of `groups` groups from each row's group, in row order. O(rows + groups).
    pub fn of(group_of: impl Iterator<Item = usize> + Clone, groups: usize) -> Self {
        // Counted one place to the right of where the starts end up, so that the prefix
        // sums leave group g's start at `starts[g + 1]`, the fill moves it on to group
        // g + 1's start, and no second vector of cursors is needed: after the fill
        // `starts[g]` is group g's start, and the one extra entry is dropped.
        let mut starts = vec![0usize; groups + 2];
        for group in group_of.clone() {
            starts[group + 2] += 1;
        }
        for g in 1..starts.len() {
            starts[g] += starts[g - 1];
        }
        let mut rows = vec![0usize; starts[groups + 1]];
        for (row, group) in group_of.enumerate() {
            rows[starts[group + 1]] = row;
            starts[group + 1] += 1;
        }
        starts.pop();
        Self { rows, starts }
    }

    /// The number of groups.
    pub fn len(&self) -> usize {
        self.starts.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The rows of group `group`, ascending.
    pub fn get(&self, group: usize) -> &[usize] {
        &self.rows[self.starts[group]..self.starts[group + 1]]
    }

    /// The groups' rows in group order.
    pub fn iter(&self) -> impl Iterator<Item = &[usize]> {
        (0..self.len()).map(|group| self.get(group))
    }

    /// The bytes it holds.
    pub fn memory_bytes(&self) -> usize {
        (self.rows.capacity() + self.starts.capacity()) * std::mem::size_of::<usize>()
    }
}

/// The groups of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Groups {
    /// One row per group: the key columns' values.
    pub keys: IdTable,
    /// `group_of[row]` is the group index (a row of `keys`) of input row `row`.
    pub group_of: Vec<u32>,
}

impl Groups {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Number of input rows per group.
    pub fn counts(&self) -> Vec<u64> {
        let mut counts = vec![0u64; self.len()];
        for &group in &self.group_of {
            counts[group as usize] += 1;
        }
        counts
    }
}

/// Groups `table` by `keys`. With no keys, all rows form one group, and an empty input gives
/// one empty group (SPARQL: an aggregate without GROUP BY yields one row).
/// The groups of `table`'s rows by their values in `keys`; the table of the groups' keys
/// charged to `budget` as it grows.
pub fn group_rows(
    table: &IdTable,
    keys: &[usize],
    budget: &Arc<Budget>,
) -> Result<Groups, BudgetExceeded> {
    if keys.is_empty() {
        return Ok(Groups {
            keys: IdTable::from_rows(0, [&[][..]], budget)?,
            group_of: vec![0; table.len()],
        });
    }
    let mut group_keys = IdTable::charged(keys.len(), 0, budget)?;
    let mut group_of = Vec::with_capacity(table.len());
    let mut key = vec![0u64; keys.len()];
    let fill = |key: &mut Vec<u64>, row: usize| {
        for (slot, &k) in key.iter_mut().zip(keys) {
            *slot = table.get(row, k);
        }
    };
    if table.is_sorted_on(keys) {
        for row in 0..table.len() {
            let new_group = row == 0
                || keys
                    .iter()
                    .any(|&k| table.get(row, k) != table.get(row - 1, k));
            if new_group {
                fill(&mut key, row);
                group_keys.push_row(&key, budget)?;
            }
            group_of.push(group_keys.len() as u32 - 1);
        }
        let sorted: Vec<usize> = (0..keys.len()).collect();
        return Ok(Groups {
            keys: group_keys.assume_sorted_by(sorted),
            group_of,
        });
    }
    if let [k] = keys {
        let mut index: HashMap<u64, u32, Hasher> = HashMap::with_hasher(Hasher::default());
        for &value in table.column(*k) {
            let next = index.len() as u32;
            let group = match index.entry(value) {
                hashbrown::hash_map::Entry::Occupied(found) => *found.get(),
                hashbrown::hash_map::Entry::Vacant(slot) => {
                    group_keys.push_row(&[value], budget)?;
                    *slot.insert(next)
                }
            };
            group_of.push(group);
        }
    } else {
        let mut index: HashMap<Vec<u64>, u32, Hasher> = HashMap::with_hasher(Hasher::default());
        for row in 0..table.len() {
            fill(&mut key, row);
            let next = index.len() as u32;
            let group = match index.get(&key) {
                Some(&group) => group,
                None => {
                    index.insert(key.clone(), next);
                    group_keys.push_row(&key, budget)?;
                    next
                }
            };
            group_of.push(group);
        }
    }
    Ok(Groups {
        keys: group_keys,
        group_of,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counted(groups: &Groups) -> Vec<(Vec<u64>, u64)> {
        let mut out: Vec<_> = groups.keys.rows().zip(groups.counts()).collect();
        out.sort();
        out
    }

    #[test]
    fn sorted_and_hashed_grouping_agree() {
        let rows: Vec<[u64; 3]> = (0..500u64).map(|i| [i % 7, i % 3, i]).collect();
        let mut table =
            IdTable::from_rows(3, rows.iter().map(|r| &r[..]), &crate::test_budget()).unwrap();
        let hashed = group_rows(&table, &[0, 1], &crate::test_budget()).unwrap();
        table.sort_by(&[0, 1]).unwrap();
        let sorted = group_rows(&table, &[0, 1], &crate::test_budget()).unwrap();
        assert_eq!(counted(&hashed), counted(&sorted));
        assert_eq!(sorted.len(), 21);
        assert!(sorted.keys.is_sorted_on(&[0, 1]));
        let single = group_rows(&table, &[1], &crate::test_budget()).unwrap();
        assert_eq!(counted(&single).iter().map(|(_, n)| n).sum::<u64>(), 500);
    }

    #[test]
    fn no_keys_is_one_group_even_when_empty() {
        let empty = IdTable::new(2);
        let groups = group_rows(&empty, &[], &crate::test_budget()).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups.counts(), vec![0]);
    }

    #[test]
    fn members_list_each_groups_rows_in_order() {
        let group_of = [2usize, 0, 2, 1, 0, 2];
        let members = Members::of(group_of.iter().copied(), 4);
        assert_eq!(members.len(), 4);
        assert_eq!(members.get(0), &[1, 4]);
        assert_eq!(members.get(1), &[3]);
        assert_eq!(members.get(2), &[0, 2, 5]);
        assert!(members.get(3).is_empty());
        assert_eq!(members.iter().map(<[usize]>::len).sum::<usize>(), 6);
        let none = Members::of(std::iter::empty(), 0);
        assert!(none.is_empty() && none.iter().next().is_none());
    }
}
