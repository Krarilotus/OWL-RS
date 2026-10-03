//! Planner statistics (XC4): how many distinct values a pattern position takes.
//!
//! Exact counts of a pattern's matches are cheap (O(r log n)); distinct counts are not
//! (O(d log n) for d values). The first request for a pattern computes its distinct count
//! exactly from the index ([`Snapshot::distinct_in`]); the engine then keeps it across
//! revisions until the pattern's match count drifts by more than a quarter. Values may
//! therefore be slightly stale: they serve cost estimates, never results.
//!
//! The default graph's characteristic sets ([`super::characteristic`]) are kept likewise,
//! per read model, until its size drifts by a quarter. Built on every core over slices
//! of the subjects, and saved beside the checkpoint (`derived/`), so that a restart reads
//! them instead of scanning the store again; a load builds them before it returns
//! ([`Statistics::prepare_sets`]). Otherwise built at once for up to [`SYNC_SETS`]
//! statements; beyond, on a thread of their own, while queries plan without them.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use super::characteristic::{CharacteristicSets, Partial};
use super::{ReadModel, Snapshot, Version};
use crate::quad::{GraphSelector, Permutation, QuadPattern};
use crate::term::{TermId, TermKind};

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
    /// Where they are saved and read back (`derived/` of a durable engine).
    dir: Mutex<Option<std::path::PathBuf>>,
}

/// Slices of the subject ids for scanning a graph by subject on every core: the
/// dictionary's IRIs and blank nodes each cut into `slices` (their payloads are dictionary
/// indexes), then every other kind at once. In id order, together all ids.
fn subject_ranges(entries: u64, slices: u64) -> Vec<(TermId, TermId)> {
    let mut starts = vec![0u64];
    for kind in [TermKind::Iri, TermKind::BlankNode] {
        let (first, last) = TermId::kind_range(kind);
        for i in 0..slices {
            starts.push(
                TermId::new(kind, entries * i / slices)
                    .raw()
                    .max(first.raw()),
            );
        }
        starts.push(last.raw() + 1);
    }
    starts.sort_unstable();
    starts.dedup();
    let mut ranges: Vec<(TermId, TermId)> = starts
        .windows(2)
        .map(|w| (TermId::from_raw(w[0]), TermId::from_raw(w[1] - 1)))
        .collect();
    ranges.push((
        TermId::from_raw(*starts.last().unwrap_or(&0)),
        TermId::from_raw(u64::MAX),
    ));
    ranges
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
    /// quarter of `snapshot`'s: kept, read from their file (written after a build, read
    /// at the first use after a start), built now (small graphs) or started on a thread
    /// of their own (`None` until they are there).
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
                None => {
                    if let Some((built, at)) = self.load_sets(snapshot, model)
                        && size.abs_diff(at) <= at / 4
                    {
                        sets.insert(model, Sets::Built(built.clone(), at));
                        return built;
                    }
                }
                _ => {}
            }
            if size > SYNC_SETS {
                sets.insert(model, Sets::Building);
            }
        }
        if size <= SYNC_SETS {
            return self.build_sets(snapshot, model, size);
        }
        let (statistics, snapshot) = (Arc::clone(self), snapshot.clone());
        let started = std::thread::Builder::new()
            .name("nrese-statistics".to_owned())
            .spawn(move || {
                statistics.build_sets(&snapshot, model, size);
            });
        if started.is_err() {
            self.sets.lock().remove(&model);
        }
        None
    }

    /// Builds the characteristic sets of `model` now, whatever the size, keeps and saves
    /// them: for a store that has just been loaded, so that no query after a start waits
    /// for them or competes with their build.
    pub(crate) fn prepare_sets(self: &Arc<Self>, snapshot: &Snapshot, model: ReadModel) {
        let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
        let size = snapshot.count_in(model, &pattern);
        self.build_sets(snapshot, model, size);
    }

    /// The sets of `model` from two scans in subject order, each on every core over slices
    /// of the subjects; kept as built at `size` and saved.
    fn build_sets(
        &self,
        snapshot: &Snapshot,
        model: ReadModel,
        size: u64,
    ) -> Option<Arc<CharacteristicSets>> {
        use rayon::prelude::*;
        let started = std::time::Instant::now();
        let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
        let ranges = subject_ranges(
            snapshot.dictionary_len(),
            rayon::current_num_threads() as u64 * 4,
        );
        let scan = |&(low, high): &(TermId, TermId)| {
            snapshot.scan_range_in(model, &pattern, Permutation::Gspo, low, high)
        };
        let built = (|| {
            let parts: Option<Vec<Partial>> = ranges
                .par_iter()
                .map(|range| scan(range).and_then(Partial::of))
                .collect();
            let sets = CharacteristicSets::merge(parts?)?;
            // The pairs need each subject's set first: a second scan.
            let scans: Option<Vec<_>> = ranges.iter().map(scan).collect();
            Some(Arc::new(match scans {
                Some(scans) => sets.with_pairs_of(scans),
                None => sets,
            }))
        })();
        tracing::debug!(
            ?model,
            statements = size,
            sets = built.as_ref().map_or(0, |s| s.len()),
            pairs = built.as_ref().is_some_and(|s| s.has_pairs()),
            ms = started.elapsed().as_millis() as u64,
            "characteristic sets built"
        );
        self.save_sets(snapshot, model, size, built.as_deref());
        self.sets
            .lock()
            .insert(model, Sets::Built(built.clone(), size));
        built
    }

    /// Where the derived files are (a durable engine's `derived/`).
    pub(crate) fn set_dir(&self, dir: std::path::PathBuf) {
        *self.dir.lock() = Some(dir);
    }

    fn sets_name(model: ReadModel) -> String {
        format!("statistics-{model:?}").to_ascii_lowercase()
    }

    fn save_sets(
        &self,
        snapshot: &Snapshot,
        model: ReadModel,
        size: u64,
        sets: Option<&CharacteristicSets>,
    ) {
        let Some(dir) = self.dir.lock().clone() else {
            return;
        };
        let covered = snapshot.dictionary_len();
        let Some(fingerprint) = snapshot.dictionary().fingerprint(covered) else {
            return;
        };
        let written =
            crate::term::derived::save(&dir, &Self::sets_name(model), covered, fingerprint, |w| {
                w.u64(size)?;
                match sets {
                    Some(sets) => {
                        w.u32(1)?;
                        sets.write(w)
                    }
                    None => w.u32(0),
                }
            });
        if let Err(error) = written {
            tracing::warn!(%error, "characteristic sets not saved; they are built again after a restart");
        }
    }

    /// The sets of `model` saved for this dictionary: (`None` if there were too many to
    /// keep, the size they were built at).
    fn load_sets(
        &self,
        snapshot: &Snapshot,
        model: ReadModel,
    ) -> Option<(Option<Arc<CharacteristicSets>>, u64)> {
        let dir = self.dir.lock().clone()?;
        let dictionary = snapshot.dictionary();
        let (_, loaded) = crate::term::derived::load(&dir, &Self::sets_name(model), |covered| {
            dictionary.fingerprint(covered)
        })?;
        let mut reader = loaded.reader();
        let size = reader.u64()?;
        let sets = match reader.u32()? {
            0 => None,
            _ => Some(Arc::new(CharacteristicSets::read(&mut reader)?)),
        };
        Some((sets, size))
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
