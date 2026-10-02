//! Planner statistics (XC4): how many distinct values a pattern position takes.
//!
//! Exact counts of a pattern's matches are cheap (O(r log n)); distinct counts are not
//! (O(d log n) for d values). The first request for a pattern computes its distinct count
//! exactly from the index ([`Snapshot::distinct_in`]); the engine then keeps it across
//! revisions until the pattern's match count drifts by more than a quarter. Values may
//! therefore be slightly stale: they serve cost estimates, never results.
//!
//! The default graph's characteristic sets ([`super::characteristic`]) are kept likewise,
//! per read model, until its size drifts by a quarter. Built at once for up to
//! [`SYNC_SETS`] statements; beyond, on a thread of their own, while queries plan without
//! them (no query waits for a scan of the store).

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use super::characteristic::CharacteristicSets;
use super::{ReadModel, Snapshot, Version};
use crate::quad::{GraphSelector, Permutation, QuadPattern};
use crate::term::TermId;

/// Default graphs up to this size get their characteristic sets built at once.
const SYNC_SETS: u64 = 1 << 20;

/// The characteristic sets of one read model: being built, or built at a size (`None`:
/// there were too many to keep).
enum Sets {
    Building,
    Built(Option<Arc<CharacteristicSets>>, u64),
}

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
    /// Node counts (distinct subjects and objects) of versions, exact. An entry holds its
    /// version weakly: while it does, the allocation can't be reused, so a pointer match
    /// is the same version (a transaction's pending snapshot shares its base's revision
    /// number, not its content).
    nodes: Mutex<Vec<NodeCount>>,
    /// Per read model, the default graph's characteristic sets.
    sets: Mutex<HashMap<ReadModel, Sets>>,
}

/// A version (held weakly), what was read of it, and its node count.
type NodeCount = (Weak<Version>, ReadModel, GraphSelector, u64);

/// Node counts kept; the cache is cleared when it fills up.
const NODE_ENTRIES: usize = 64;

impl Statistics {
    /// The number of distinct subjects and objects of `graphs` in `model` at `version`:
    /// cached, or computed by `count` (`None`: it can't be, and nothing is kept).
    pub(crate) fn node_count(
        &self,
        version: &Arc<Version>,
        model: ReadModel,
        graphs: GraphSelector,
        count: impl FnOnce() -> Option<u64>,
    ) -> Option<u64> {
        let found = self.nodes.lock().iter().find_map(|(weak, m, g, n)| {
            (*m == model && *g == graphs && std::ptr::eq(weak.as_ptr(), Arc::as_ptr(version)))
                .then_some(*n)
        });
        if found.is_some() {
            return found;
        }
        let n = count()?;
        let mut nodes = self.nodes.lock();
        nodes.retain(|(weak, ..)| weak.strong_count() > 0);
        if nodes.len() >= NODE_ENTRIES {
            nodes.clear();
        }
        nodes.push((Arc::downgrade(version), model, graphs, n));
        Some(n)
    }

    /// The characteristic sets of the default graph in `model`, as of a size within a
    /// quarter of `snapshot`'s: kept, built now (small graphs) or started on a thread of
    /// their own (`None` until they are there).
    pub(crate) fn characteristic_sets(
        self: &Arc<Self>,
        snapshot: &Snapshot,
        model: ReadModel,
    ) -> Option<Arc<CharacteristicSets>> {
        let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
        let size = snapshot.count_in(model, &pattern);
        {
            let mut sets = self.sets.lock();
            match sets.get(&model) {
                Some(Sets::Built(built, at)) if size.abs_diff(*at) <= at / 4 => {
                    return built.clone();
                }
                Some(Sets::Building) => return None,
                _ => {}
            }
            if size > SYNC_SETS {
                sets.insert(model, Sets::Building);
            }
        }
        let build = move |snapshot: &Snapshot| {
            snapshot
                .scan_sorted_in(model, &pattern, Permutation::Gspo)
                .and_then(CharacteristicSets::build)
                .map(Arc::new)
        };
        if size <= SYNC_SETS {
            let built = build(snapshot);
            self.sets
                .lock()
                .insert(model, Sets::Built(built.clone(), size));
            return built;
        }
        let (statistics, snapshot) = (Arc::clone(self), snapshot.clone());
        let started = std::thread::Builder::new()
            .name("nrese-statistics".to_owned())
            .spawn(move || {
                let built = build(&snapshot);
                statistics
                    .sets
                    .lock()
                    .insert(model, Sets::Built(built, size));
            });
        if started.is_err() {
            self.sets.lock().remove(&model);
        }
        None
    }

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
