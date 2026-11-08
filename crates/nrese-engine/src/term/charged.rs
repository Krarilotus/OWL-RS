//! A shared value whose `Arc` block is charged, and released only after the block is freed
//! (package 1 review r1 F2's order, generalised from the suffix's chunk reference).
//!
//! An `Arc<T>` dropped as such drops `T` first, releasing whatever charges `T` holds, and frees
//! its block after: memory held beyond its charge. A [`ChargedArc`]'s last holder instead takes
//! the value out of its `Arc` ([`Arc::into_inner`], which frees the block), then lets the value
//! and the block's own reservation drop.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "suffixes and overlays are made from package 2b")
)]

use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, Reservation};

/// A value shared by its holders, its `Arc` block charged ([`Self::COST`]).
pub(crate) struct ChargedArc<T>(Option<Arc<Held<T>>>);

struct Held<T> {
    // Declared first: dropped (its own charges released) before the block's reservation.
    value: T,
    _block: Reservation<Arc<Budget>>,
}

impl<T> ChargedArc<T> {
    /// What the `Arc` block of one allocates: its two counts and the held value.
    pub(crate) const COST: usize = 2 * size_of::<usize>() + size_of::<Held<T>>();

    /// `value`, shared, its block charged to `budget` first: refused, `value` is dropped.
    pub(crate) fn new(value: T, budget: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let mut block = Reservation::new(Arc::clone(budget));
        block.charge(Self::COST)?;
        Ok(Self::with_reserved(value, block))
    }

    /// `value`, shared, its block's [`Self::COST`] already charged to `block` (for a caller
    /// that must not lose `value` on a refusal: it charges first, then moves the value in).
    pub(crate) fn with_reserved(value: T, block: Reservation<Arc<Budget>>) -> Self {
        debug_assert!(block.bytes() >= Self::COST, "the block charged");
        Self(Some(Arc::new(Held {
            value,
            _block: block,
        })))
    }
}

impl<T> Clone for ChargedArc<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> std::ops::Deref for ChargedArc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self
            .0
            .as_deref()
            .expect("a value until its holder drops")
            .value
    }
}

impl<T> Drop for ChargedArc<T> {
    fn drop(&mut self) {
        if let Some(held) = self.0.take() {
            drop(Arc::into_inner(held));
        }
    }
}
