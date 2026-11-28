//! Sorted node sets held by the affected walk. The charge follows the storage.

use nrese_engine::TermId;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec};

pub(super) struct Reached<'a>(ChargedVec<TermId, &'a Budget>);

impl<'a> Reached<'a> {
    pub(super) fn new(budget: &'a Budget) -> Self {
        Self(ChargedVec::new(budget))
    }

    pub(super) fn from_sorted(
        budget: &'a Budget,
        nodes: &[TermId],
    ) -> Result<Self, BudgetExceeded> {
        Ok(Self(ChargedVec::collect(
            budget,
            nodes.len(),
            nodes.iter().copied(),
        )?))
    }

    pub(super) fn from_unsorted(
        budget: &'a Budget,
        nodes: impl IntoIterator<Item = TermId>,
    ) -> Result<Self, BudgetExceeded> {
        let mut items = ChargedVec::new(budget);
        for node in nodes {
            items.push(node)?;
        }
        items.sort_unstable();
        let mut previous = None;
        items.retain(|&node| previous.replace(node) != Some(node));
        Ok(Self(items))
    }

    /// Take over the sorted output of the path producer and charge its capacity now.
    pub(super) fn adopt(budget: &'a Budget, nodes: Vec<TermId>) -> Result<Self, BudgetExceeded> {
        Ok(Self(ChargedVec::new(budget).adopt_vec(nodes)?))
    }

    pub(super) fn as_slice(&self) -> &[TermId] {
        &self.0
    }

    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn contains(&self, node: &TermId) -> bool {
        self.0.binary_search(node).is_ok()
    }

    pub(super) fn union(&mut self, other: Self) -> Result<(), BudgetExceeded> {
        if self.is_empty() {
            *self = other;
            return Ok(());
        }
        self.union_sorted(other.as_slice())
    }

    /// Adds `other`, which must be sorted and free of duplicates (as every set here is),
    /// growing the storage by exactly the nodes it doesn't hold yet.
    pub(super) fn union_sorted(&mut self, other: &[TermId]) -> Result<(), BudgetExceeded> {
        debug_assert!(
            other.windows(2).all(|pair| pair[0] < pair[1]),
            "union_sorted: the added nodes must be sorted and unique"
        );
        if self.is_empty() {
            return self.0.extend_from_slice(other);
        }
        // Count new nodes first so an overlapping set is only charged for actual growth.
        let mut left = self.0.iter().peekable();
        let added = other
            .iter()
            .filter(|&node| {
                while left.next_if(|&earlier| earlier < node).is_some() {}
                left.peek() != Some(&node)
            })
            .count();
        if added == 0 {
            return Ok(());
        }

        // Merge from the tail into the reserved space. The prefix that remains when
        // the right side is exhausted is already in place.
        let mut left = self.len();
        let mut end = left + added;
        // Exactly the new nodes: a doubling would hold, and charge, up to twice the set.
        self.0.reserve_exact(added)?;
        self.0.resize(end, other[0])?;
        for &node in other.iter().rev() {
            while left > 0 && self.0[left - 1] > node {
                left -= 1;
                end -= 1;
                self.0[end] = self.0[left];
            }
            if left == 0 || self.0[left - 1] != node {
                end -= 1;
                self.0[end] = node;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn ids(raw: &[u64]) -> Vec<TermId> {
        raw.iter().copied().map(TermId::from_raw).collect()
    }

    /// The merge of two sorted, duplicate-free sets is their union, sorted and free of
    /// duplicates, and the storage after a merge into a nonempty set holds no more than it.
    #[test]
    fn a_merge_is_the_sorted_union_in_storage_of_its_size() {
        let cases: [(&[u64], &[u64]); 8] = [
            (&[], &[]),
            (&[], &[1, 4]),
            (&[2, 5], &[]),
            (&[1, 2, 3], &[1, 2, 3]),
            (&[1, 3, 5], &[2, 4, 6]),
            (&[1, 2], &[7, 8, 9]),
            (&[7, 8, 9], &[1, 2]),
            (&[1, 4, 9], &[0, 4, 5, 9, 10]),
        ];
        let budget = Budget::unlimited();
        for (left, right) in cases {
            let expected: Vec<TermId> = ids(left)
                .into_iter()
                .chain(ids(right))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let mut set = Reached::from_sorted(&budget, &ids(left)).expect("unlimited");
            set.union_sorted(&ids(right)).expect("unlimited");
            assert_eq!(set.as_slice(), expected, "{left:?} with {right:?}");
            if !left.is_empty() {
                assert_eq!(set.0.capacity(), expected.len(), "{left:?} with {right:?}");
            }
        }
    }
}
