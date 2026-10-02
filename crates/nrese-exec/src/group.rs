//! Grouping: which rows belong together, for GROUP BY and DISTINCT-like operators.
//!
//! [`group_rows`] returns one key row per group and the group of every input row. Input
//! sorted on the keys is grouped by run length, with no hashing and groups in key order;
//! other input goes through a hash table, with groups in order of first appearance.
//! Aggregates are computed by the caller from the row-to-group map, because most of them
//! (SUM, MIN, AVG) need term values, which this crate doesn't resolve.

use hashbrown::HashMap;

use crate::table::IdTable;

type Hasher = foldhash::fast::FixedState;

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
pub fn group_rows(table: &IdTable, keys: &[usize]) -> Groups {
    if keys.is_empty() {
        return Groups {
            keys: IdTable::from_rows(0, [&[][..]]),
            group_of: vec![0; table.len()],
        };
    }
    let mut group_keys = IdTable::new(keys.len());
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
                group_keys.push_row(&key);
            }
            group_of.push(group_keys.len() as u32 - 1);
        }
        let sorted: Vec<usize> = (0..keys.len()).collect();
        return Groups {
            keys: group_keys.assume_sorted_by(sorted),
            group_of,
        };
    }
    if let [k] = keys {
        let mut index: HashMap<u64, u32, Hasher> = HashMap::with_hasher(Hasher::default());
        for &value in table.column(*k) {
            let next = index.len() as u32;
            let group = *index.entry(value).or_insert_with(|| {
                group_keys.push_row(&[value]);
                next
            });
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
                    group_keys.push_row(&key);
                    next
                }
            };
            group_of.push(group);
        }
    }
    Groups {
        keys: group_keys,
        group_of,
    }
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
        let mut table = IdTable::from_rows(3, rows.iter().map(|r| &r[..]));
        let hashed = group_rows(&table, &[0, 1]);
        table.sort_by(&[0, 1]);
        let sorted = group_rows(&table, &[0, 1]);
        assert_eq!(counted(&hashed), counted(&sorted));
        assert_eq!(sorted.len(), 21);
        assert!(sorted.keys.is_sorted_on(&[0, 1]));
        let single = group_rows(&table, &[1]);
        assert_eq!(counted(&single).iter().map(|(_, n)| n).sum::<u64>(), 500);
    }

    #[test]
    fn no_keys_is_one_group_even_when_empty() {
        let empty = IdTable::new(2);
        let groups = group_rows(&empty, &[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups.counts(), vec![0]);
    }
}
