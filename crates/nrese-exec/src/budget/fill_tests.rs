//! One existing guard's admission extension; no new test identity.

use super::{Budget, BudgetExceeded, ChargedVec, FillRange, SharedBudget, Written};
use crate::measure;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FillError {
    Item(usize),
    Memory(BudgetExceeded),
}

impl From<BudgetExceeded> for FillError {
    fn from(error: BudgetExceeded) -> Self {
        Self::Memory(error)
    }
}

pub(super) fn header_admission_and_lifetimes() {
    let _ = measure::measure(|| ());
    // Spec: the exact payload Budget has no header headroom; no callback may run.
    let budget = Budget::new(2 * size_of::<usize>());
    let mut values = ChargedVec::with_capacity(&budget, 2).unwrap();
    values.push(usize::MAX).unwrap();
    let called = AtomicUsize::new(0);
    let refused = values
        .fill(1, |room| {
            room.try_par_fill(1, |i| {
                called.fetch_add(1, Ordering::Relaxed);
                Ok::<_, FillError>(i)
            })
        })
        .unwrap();
    eprintln!(
        "header_refusal result={refused:?} callbacks={} len={} used={} payload={}",
        called.load(Ordering::Relaxed),
        values.len(),
        budget.used(),
        values.charged()
    );
    assert!(
        matches!(refused, Err(FillError::Memory(error)) if error.limit == budget.limit()
            && error.used == values.charged() && !error.shared),
        "header admission must refuse through the exact payload Budget: {refused:?}"
    );
    assert_eq!(called.load(Ordering::Relaxed), 0);
    assert_eq!(&*values, &[usize::MAX]);
    assert_eq!(budget.used(), values.charged());
    drop(values);
    assert_eq!(budget.used(), 0);

    // Spec: actual headers are charged before allocator events and freed before release,
    // on success and on a reached callback error. Pre-existing payload has no new credit.
    let shared = SharedBudget::new(64 * 1024);
    let budget = Budget::new(64 * 1024).within(Some(shared.clone()));
    let mut values = ChargedVec::with_capacity(&budget, 101).unwrap();
    values.push(usize::MAX).unwrap();
    for (n, fail) in [(1, None), (100, None), (100, Some(60))] {
        let ((filled, excess), peak) = measure::measure(|| {
            measure::measure_within(&budget, || {
                values.fill(n, |room| {
                    room.try_par_fill(n, |i| {
                        if Some(i) == fail {
                            Err(FillError::Item(i))
                        } else {
                            Ok(i)
                        }
                    })
                })
            })
        });
        assert_eq!(
            excess, 0,
            "all actual scratch remains admitted through free"
        );
        match fail {
            None => {
                assert_eq!(filled, Ok(Ok(())));
                assert_eq!(values.len(), n + 1);
                assert_eq!(values[0], usize::MAX);
                for (i, value) in values.iter().skip(1).enumerate() {
                    assert_eq!(*value, i);
                }
            }
            Some(i) => {
                assert_eq!(filled, Ok(Err(FillError::Item(i))));
                assert_eq!(&*values, &[usize::MAX]);
            }
        }
        assert_eq!(budget.used(), values.charged());
        assert_eq!(shared.used(), values.charged());
        eprintln!(
            "header_alloc n={n} fail={fail:?} requested_peak={peak} excess={excess} used={} charged_peak={} range_layout={} written_layout={}",
            budget.used(),
            budget.peak(),
            std::alloc::Layout::new::<FillRange<usize, FillError>>().size(),
            std::alloc::Layout::new::<Written<usize>>().size()
        );
        values.truncate(1);
    }
    drop(values);
    assert_eq!(budget.used(), 0);
    assert_eq!(shared.used(), 0);
}
