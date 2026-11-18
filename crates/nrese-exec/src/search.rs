//! Searching sorted slices forward from where the last search landed (finger search): the
//! partition point found by galloping from a position, O(log d) for a distance d instead
//! of O(log n) from the root. Sorted probes (a morsel's candidates, a join's next keys)
//! read a run forward this way and stay in the cache, where each search from the root
//! misses it at every level (performance.md §4; the index's cursor does the same over
//! packed keys, `nrese_engine::ProbeCursor`).

use std::ops::Range;

/// The first index `>= from` in `slice` whose element `pred` rejects, where `pred` holds
/// for a prefix of `slice` (as for [`slice::partition_point`]): exponential steps from
/// `from`, then a binary search in the last step.
#[inline]
pub fn partition_point_from<T>(slice: &[T], from: usize, pred: impl Fn(&T) -> bool) -> usize {
    let range = partition_range_from(slice.len(), from.min(slice.len()), |i| pred(&slice[i]));
    range.start + slice[range].partition_point(pred)
}

/// Bracket a forward partition point in O(log distance) index probes. `pred` must hold
/// for a prefix of `0..len`. The answer lies in `start..=end`; only `start..end` still
/// needs searching. An exhausted start (`from >= len`) returns `from..from` unprobed.
/// Slices and columnar joins keep their own binary-search specialisations.
// Inline early so slice callers can eliminate the predicate's redundant bounds checks.
#[inline(always)]
pub(crate) fn partition_range_from(
    len: usize,
    from: usize,
    pred: impl Fn(usize) -> bool,
) -> Range<usize> {
    if from >= len || !pred(from) {
        return from..from;
    }
    // `pred` holds at `low`; the answer is in `(low, high]`.
    let (mut low, mut step) = (from, 1);
    let mut high = from + 1;
    while high < len && pred(high) {
        low = high;
        step *= 2;
        high = low + step;
    }
    low + 1..high.min(len)
}

/// A finger into a sorted slice: probes in ascending order find each value by
/// [`partition_point_from`] from where the previous one landed. A probe below the last
/// starts from the beginning again, so any order gives the right answers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Finger {
    at: usize,
}

impl Finger {
    /// The first index whose element is not below `value`, from the finger on (or from
    /// the start if `value` is below the element before the finger).
    pub fn seek<T: Ord>(&mut self, slice: &[T], value: &T) -> usize {
        if self.at > slice.len() || (self.at > 0 && slice[self.at - 1] >= *value) {
            self.at = 0;
        }
        self.at = partition_point_from(slice, self.at, |x| x < value);
        self.at
    }

    /// Whether `slice` holds `value` ([`Self::seek`]).
    pub fn contains<T: Ord>(&mut self, slice: &[T], value: &T) -> bool {
        let at = self.seek(slice, value);
        slice.get(at) == Some(value)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{Finger, partition_point_from, partition_range_from};

    #[test]
    fn forward_ranges_bracket_every_boundary_with_logarithmic_probes() {
        for len in 0..=65 {
            for from in 0..=len + 1 {
                for boundary in 0..=len {
                    let probes = Cell::new(0);
                    let range = partition_range_from(len, from, |i| {
                        assert!((from..len).contains(&i));
                        probes.set(probes.get() + 1);
                        i < boundary
                    });
                    let expected = boundary.max(from);
                    assert!((range.start..=range.end).contains(&expected));
                    assert!(range.start >= from);
                    assert!(range.end <= len.max(from));
                    // Everything skipped before the bracket is known to pass.
                    assert!(range.start == from || range.start <= boundary);
                    // The end is either exhausted or already known to reject.
                    assert!(range.end >= boundary);
                    if from >= len {
                        assert_eq!(range, from..from);
                        assert_eq!(probes.get(), 0);
                    } else {
                        let distance = expected - from;
                        assert!(probes.get() <= 2 + distance.max(1).ilog2());
                    }
                }
            }
        }
    }

    #[test]
    fn fingers_keep_the_first_duplicate_and_rewind_after_exhaustion() {
        let values = [1, 1, 3, 3, 3, 7];
        let mut finger = Finger::default();
        for probe in [0, 1, 1, 2, 3, 3, 6, 7, 8, 9, 3, 1] {
            assert_eq!(
                finger.seek(&values, &probe),
                values.partition_point(|v| *v < probe)
            );
        }
        assert_eq!(finger.seek(&[], &3), 0);
        assert_eq!(finger.seek(&values[..2], &1), 0);
        assert_eq!(
            partition_point_from(&values, usize::MAX, |_| panic!()),
            values.len()
        );
    }

    #[test]
    fn galloping_equals_a_partition_point() {
        let mut state = 7u64;
        let mut next = |n: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 33) % n
        };
        for _ in 0..300 {
            let mut values: Vec<u64> = (0..next(200)).map(|_| next(500)).collect();
            values.sort_unstable();
            for _ in 0..20 {
                let target = next(520);
                let expected = values.partition_point(|&v| v < target);
                let from = next(values.len() as u64 + 2) as usize;
                let got = partition_point_from(&values, from, |&v| v < target);
                assert_eq!(got, expected.max(from.min(values.len())));
            }
            // A finger over probes in any order answers as a search from the root.
            let mut finger = Finger::default();
            for _ in 0..50 {
                let probe = next(520);
                assert_eq!(
                    finger.contains(&values, &probe),
                    values.binary_search(&probe).is_ok()
                );
            }
        }
    }
}
