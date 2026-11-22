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

    pub(super) fn union_sorted(&mut self, other: &[TermId]) -> Result<(), BudgetExceeded> {
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
