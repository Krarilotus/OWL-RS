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
    /// What the `Arc` block of one allocates: its two counts, padded to the held value's
    /// alignment, then the held value, the whole padded to the block's alignment (the block
    /// is `repr(C)`: the counts first, then the value).
    pub(crate) const COST: usize = {
        let value = align_of::<Held<T>>();
        let block = if value > align_of::<usize>() {
            value
        } else {
            align_of::<usize>()
        };
        ((2 * size_of::<usize>()).next_multiple_of(value) + size_of::<Held<T>>())
            .next_multiple_of(block)
    };

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

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nrese_exec::Budget;

    use super::ChargedArc;

    /// A value aligned above `usize`, which counts its drops: in its `Arc` block the two counts
    /// are padded out to its alignment before it.
    #[repr(align(64))]
    struct Wide {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for Wide {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Review P2A-F1 (8 Nov): a shared value's block is charged in full, whatever its value's
    /// alignment, and held only while charged.
    ///
    /// - Input: a `ChargedArc` of a 64-byte-aligned value, cloned twice, its holders dropped
    ///   one by one, the last one last.
    /// - Expected: at no point more allocated than charged (the allocator sees the padded
    ///   block); the charge stays while any holder lives and the value is dropped once, with
    ///   the last; then nothing is charged.
    ///
    /// Measured, alone.
    #[test]
    fn an_over_aligned_value_is_held_only_while_charged_to_its_last_holder() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let budget = Arc::new(Budget::unlimited());
        let drops = Arc::new(AtomicUsize::new(0));
        let value = Wide {
            drops: Arc::clone(&drops),
        };
        let ((), excess) = nrese_exec::measure::measure_within(&budget, || {
            let first = ChargedArc::new(value, &budget).unwrap();
            let charged = budget.used();
            assert!(charged > 0, "the block is charged");
            let second = first.clone();
            let third = second.clone();
            drop(first);
            drop(third);
            assert_eq!(budget.used(), charged, "charged while a holder lives");
            assert_eq!(
                drops.load(Ordering::Relaxed),
                0,
                "the value lives with a holder"
            );
            drop(second);
            assert_eq!(
                drops.load(Ordering::Relaxed),
                1,
                "dropped once, with the last"
            );
            assert_eq!(budget.used(), 0, "released with the last holder");
        });
        assert_eq!(excess, 0, "bytes held beyond the block's charge");
    }

    /// Holders dropped by an unwind release the charge once and drop the value once.
    ///
    /// - Input: a `ChargedArc` and a clone, both live when their thread unwinds.
    /// - Expected: after the unwind is caught, the value dropped once and nothing charged.
    #[test]
    fn holders_dropped_by_an_unwind_release_the_charge_once() {
        let budget = Arc::new(Budget::unlimited());
        let drops = Arc::new(AtomicUsize::new(0));
        let value = Wide {
            drops: Arc::clone(&drops),
        };
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let first = ChargedArc::new(value, &budget).unwrap();
            let _second = first.clone();
            assert!(budget.used() > 0, "the block is charged");
            resume_unwind(Box::new("unwinding with two holders"));
        }));
        assert!(unwound.is_err(), "it unwound");
        assert_eq!(drops.load(Ordering::Relaxed), 1, "the value dropped once");
        assert_eq!(budget.used(), 0, "the charge released");
    }
}
