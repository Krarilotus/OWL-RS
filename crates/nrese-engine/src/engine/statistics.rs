//! Planner statistics (XC4): how many distinct values a pattern position takes.
//!
//! Exact counts of a pattern's matches are cheap (O(r log n)); distinct counts are not
//! (O(d log n) for d values). The first request for a pattern computes its distinct count
//! exactly from the index ([`Snapshot::distinct_in`]); the engine then keeps it across
//! revisions until the pattern's match count drifts by more than a quarter. Values may
//! therefore be slightly stale: they serve cost estimates, never results.

use std::collections::HashMap;

use parking_lot::Mutex;

use super::{ReadModel, Snapshot};
use crate::quad::{Permutation, QuadPattern};

/// Cached entries; the cache is cleared when it fills up.
const ENTRIES: usize = 1 << 14;

/// Patterns with more matches get no distinct count (`None`): computing it would hold up
/// the first query that plans with them.
const MAX_MATCHES: u64 = 1 << 22;

type Key = (ReadModel, QuadPattern, Permutation);

/// The engine's distinct-count cache, shared by all snapshots.
#[derive(Default)]
pub(crate) struct Statistics {
    /// Per key: the distinct count and the match count it was computed at.
    distinct: Mutex<HashMap<Key, (u64, u64)>>,
}

impl Statistics {
    /// The number of distinct values of `permutation`'s first unbound component among the
    /// matches of `pattern` in `model`: cached, or computed now (see the module docs).
    /// `None` where [`Snapshot::distinct_in`] can't answer.
    pub(crate) fn distinct(
        &self,
        snapshot: &Snapshot,
        model: ReadModel,
        pattern: &QuadPattern,
        permutation: Permutation,
    ) -> Option<u64> {
        let key = (model, *pattern, permutation);
        let count = snapshot.count_in(model, pattern);
        if let Some(&(distinct, at)) = self.distinct.lock().get(&key)
            && count.abs_diff(at) <= at / 4
        {
            return Some(distinct.min(count));
        }
        if count > MAX_MATCHES {
            return None;
        }
        // Computed without the lock: concurrent planners may both compute it once.
        let distinct = snapshot.distinct_in(model, pattern, permutation)?;
        let mut cache = self.distinct.lock();
        if cache.len() >= ENTRIES {
            cache.clear();
        }
        cache.insert(key, (distinct, count));
        Some(distinct)
    }
}
