//! Searching sorted slices forward from where the last search landed (finger search): the
//! partition point found by galloping from a position, O(log d) for a distance d instead
//! of O(log n) from the root. Sorted probes (a morsel's candidates, a join's next keys)
//! read a run forward this way and stay in the cache, where each search from the root
//! misses it at every level (performance.md §4; the index's cursor does the same over
//! packed keys, `nrese_engine::ProbeCursor`).

/// The first index `>= from` in `slice` whose element `pred` rejects, where `pred` holds
/// for a prefix of `slice` (as for [`slice::partition_point`]): exponential steps from
/// `from`, then a binary search in the last step.
pub fn partition_point_from<T>(slice: &[T], from: usize, pred: impl Fn(&T) -> bool) -> usize {
    let len = slice.len();
    if from >= len || !pred(&slice[from]) {
        return from.min(len);
    }
    // `pred` holds at `low`; the answer is in `(low, high]`.
    let (mut low, mut step) = (from, 1);
    let mut high = from + 1;
    while high < len && pred(&slice[high]) {
        low = high;
        step *= 2;
        high = low + step;
    }
    let high = high.min(len);
    low + 1 + slice[low + 1..high].partition_point(pred)
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
    use super::{Finger, partition_point_from};

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
