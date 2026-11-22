//! Suspended callers of a depth-first shape walk, indexed by their active keys.

use std::hash::Hash;

use nrese_exec::{Budget, BudgetExceeded, ChargedMap, Reservation};

pub(crate) struct Suspended<'a, K, F> {
    // A root and its direct child need only one suspended frame, inline.
    root: Option<(K, F)>,
    deeper: ChargedMap<K, F, Reservation<&'a Budget>>,
}

impl<'a, K: Eq + Hash, F> Suspended<'a, K, F> {
    pub(crate) fn new(budget: &'a Budget) -> Self {
        Self {
            root: None,
            deeper: ChargedMap::new(budget),
        }
    }

    pub(crate) fn contains(&self, key: &K) -> bool {
        self.root.as_ref().is_some_and(|(root, _)| root == key)
            || (!self.deeper.is_empty() && self.deeper.get(key).is_some())
    }

    pub(crate) fn park(&mut self, key: K, frame: F) -> Result<(), BudgetExceeded> {
        if self.root.is_none() {
            self.root = Some((key, frame));
            return Ok(());
        }
        let previous = self.deeper.insert(key, frame)?;
        debug_assert!(previous.is_none());
        Ok(())
    }

    pub(crate) fn resume(&mut self, key: &K) -> F {
        if self.root.as_ref().is_some_and(|(root, _)| root == key) {
            self.root.take().expect("the suspended root").1
        } else {
            self.deeper.remove(key).expect("a suspended caller")
        }
    }
}
