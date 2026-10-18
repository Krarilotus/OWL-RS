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
//! ([`Statistics::prepare_sets`]). No query plans on a half-built state, and no build
//! takes every core beside the queries (DBpedia core q02 ran 3.3-7.5 ms beside one, 2.0
//! ms after it):
//!
//! - Sets kept or saved but drifted: every planner goes on with them, while one rebuild
//!   on a thread of its own, scanning on that thread alone, replaces them when done.
//! - None at all (a first use without a load's build): the first planner builds them and
//!   planners that arrive meanwhile wait for that build. A Rayon worker doesn't wait (the
//!   stack it holds may be what the build waits for, as for the result cache's flights):
//!   it plans without them. Only such a query (SHACL-SPARQL checks run on workers) can
//!   plan by when it came, and only during that first build.
//!
//! A scan stops once the slices have found more than
//! [`MAX_SETS`](super::characteristic::MAX_SETS) sets between them.
//!
//! The corpora of full-text searches ([`super::corpora`]) are kept beside them, in a store
//! the views made from a snapshot share with it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use super::characteristic::{CharacteristicSets, Partial, SetCount};
use super::corpora::CorpusStore;
use super::{ReadModel, Snapshot, Version};
use crate::quad::{GraphSelector, Permutation, QuadPattern};
use crate::term::{TermId, TermKind};

/// The characteristic sets of one read model as built at a size (`None`: there were too
/// many to keep), and whether a rebuild is under way.
struct Sets {
    built: Option<Arc<CharacteristicSets>>,
    at: u64,
    rebuilding: bool,
}

impl Sets {
    fn fits(&self, size: u64) -> bool {
        size.abs_diff(self.at) <= self.at / 4
    }
}

/// How a planner came by the sets ([`Statistics::kept_sets`]).
enum Kept {
    /// Kept for its size, or kept and drifted (a rebuild started if none was).
    Sets(Option<Arc<CharacteristicSets>>),
    /// None kept or saved.
    Cold,
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
    /// Held while the sets are built: planners that need them meanwhile wait for them.
    building: Mutex<()>,
    /// The full-text corpora ([`super::corpora`]): this engine's, shared with the views
    /// made from its snapshots.
    pub(crate) corpora: Arc<CorpusStore>,
    /// Where they are saved and read back (`derived/` of a durable engine).
    dir: Mutex<Option<std::path::PathBuf>>,
    /// Scans of the store for the sets begun: what the tests count.
    scans: AtomicU64,
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

    /// Statistics of an engine whose full-text corpora may hold `corpus_bytes`.
    pub(crate) fn new(corpus_bytes: usize) -> Self {
        Self {
            corpora: Arc::new(CorpusStore::new(corpus_bytes)),
            ..Self::default()
        }
    }

    /// Statistics of their own for a view made from a snapshot with `self`, sharing its
    /// full-text corpora (they are kept by the statements' identity, which the view has
    /// its own of).
    pub(crate) fn for_view(&self) -> Self {
        Self {
            corpora: Arc::clone(&self.corpora),
            ..Self::default()
        }
    }

    /// The characteristic sets of the default graph in `model` (see the module docs): kept
    /// (drifted ones while one rebuild replaces them), read from their file (written
    /// after a build, read at the first use after a start), or built now, the first
    /// time, by one planner while the others wait; a Rayon worker that would wait gets
    /// none.
    pub(crate) fn characteristic_sets(
        self: &Arc<Self>,
        snapshot: &Snapshot,
        model: ReadModel,
    ) -> Option<Arc<CharacteristicSets>> {
        let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
        let size = snapshot.count_in(model, &pattern);
        if let Kept::Sets(sets) = self.kept_sets(snapshot, model, size) {
            return sets;
        }
        let building = match rayon::current_thread_index() {
            Some(_) => self.building.try_lock(),
            None => Some(self.building.lock()),
        };
        let _building = building?;
        match self.kept_sets(snapshot, model, size) {
            Kept::Sets(sets) => sets,
            Kept::Cold => self.build_sets(snapshot, model, size, true),
        }
    }

    /// The sets of `model` kept, or read from their file if none are; drifted ones by more
    /// than a quarter from `size` too, with one rebuild started for them.
    fn kept_sets(self: &Arc<Self>, snapshot: &Snapshot, model: ReadModel, size: u64) -> Kept {
        let mut sets = self.sets.lock();
        let kept = match sets.entry(model) {
            std::collections::hash_map::Entry::Occupied(kept) => kept.into_mut(),
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let Some((built, at)) = self.load_sets(snapshot, model) else {
                    return Kept::Cold;
                };
                vacant.insert(Sets {
                    built,
                    at,
                    rebuilding: false,
                })
            }
        };
        if !kept.fits(size) && !kept.rebuilding {
            kept.rebuilding = true;
            self.rebuild(snapshot, model);
        }
        Kept::Sets(kept.built.clone())
    }

    /// Rebuilds the drifted sets of `model` on a thread of its own, scanning on that
    /// thread alone, and replaces them when done; planners go on with the old ones.
    fn rebuild(self: &Arc<Self>, snapshot: &Snapshot, model: ReadModel) {
        let (statistics, snapshot) = (Arc::clone(self), snapshot.clone());
        let started = std::thread::Builder::new()
            .name("nrese-statistics".to_owned())
            .spawn(move || {
                let _building = statistics.building.lock();
                let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
                let size = snapshot.count_in(model, &pattern);
                statistics.build_sets(&snapshot, model, size, false);
            });
        if started.is_err()
            && let Some(kept) = self.sets.lock().get_mut(&model)
        {
            // Asked again at the next drifted use.
            kept.rebuilding = false;
        }
    }

    /// Builds the characteristic sets of `model` now, whatever the size, keeps and saves
    /// them: for a store that has just been loaded, so that no query after a start waits
    /// for them or competes with their build.
    pub(crate) fn prepare_sets(self: &Arc<Self>, snapshot: &Snapshot, model: ReadModel) {
        let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
        let size = snapshot.count_in(model, &pattern);
        let _building = self.building.lock();
        self.build_sets(snapshot, model, size, true);
    }

    /// The sets of `model` scanned (on every core or on this thread alone), kept as built
    /// at `size` and saved; under `building`.
    fn build_sets(
        &self,
        snapshot: &Snapshot,
        model: ReadModel,
        size: u64,
        every_core: bool,
    ) -> Option<Arc<CharacteristicSets>> {
        let started = std::time::Instant::now();
        self.scans.fetch_add(1, Ordering::Relaxed);
        let built = Self::scan_sets(snapshot, model, every_core);
        tracing::debug!(
            ?model,
            statements = size,
            sets = built.as_ref().map_or(0, |s| s.len()),
            pairs = built.as_ref().is_some_and(|s| s.has_pairs()),
            every_core,
            ms = started.elapsed().as_millis() as u64,
            "characteristic sets built"
        );
        self.save_sets(snapshot, model, size, built.as_deref());
        self.sets.lock().insert(
            model,
            Sets {
                built: built.clone(),
                at: size,
                rebuilding: false,
            },
        );
        built
    }

    /// Scans of the store for the characteristic sets begun so far.
    #[cfg(test)]
    pub(crate) fn scans(&self) -> u64 {
        self.scans.load(Ordering::Relaxed)
    }

    /// The sets of `model` from two scans in subject order over slices of the subjects,
    /// on every core or on this thread alone; the first stops once the slices have found
    /// more than [`MAX_SETS`](super::characteristic::MAX_SETS) between them.
    fn scan_sets(
        snapshot: &Snapshot,
        model: ReadModel,
        every_core: bool,
    ) -> Option<Arc<CharacteristicSets>> {
        use rayon::prelude::*;
        let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
        let ranges = subject_ranges(
            snapshot.dictionary_len(),
            rayon::current_num_threads() as u64 * 4,
        );
        let scan = |&(low, high): &(TermId, TermId)| {
            snapshot.scan_range_in(model, &pattern, Permutation::Gspo, low, high)
        };
        let count = SetCount::default();
        let part = |range: &(TermId, TermId)| scan(range).and_then(|q| Partial::of(q, &count));
        let parts: Option<Vec<Partial>> = match every_core {
            true => ranges.par_iter().map(part).collect(),
            false => ranges.iter().map(part).collect(),
        };
        let sets = CharacteristicSets::merge(parts?)?;
        // The pairs need each subject's set first: a second scan.
        let scans: Option<Vec<_>> = ranges.iter().map(scan).collect();
        Some(Arc::new(match scans {
            Some(scans) => sets.with_pairs_of(scans),
            None => sets,
        }))
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

#[cfg(test)]
mod tests {
    use rayon::prelude::*;

    use super::*;
    use crate::{Engine, EngineConfig};
    use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

    /// Subjects `s{i}` with one to three of three predicates.
    fn add(engine: &Engine, subjects: std::ops::Range<u32>) {
        let mut tx = engine.transaction();
        for i in subjects {
            let subject = NamedNode::new_unchecked(format!("http://example.com/s{i}"));
            for p in 0..1 + i % 3 {
                let predicate = NamedNode::new_unchecked(format!("http://example.com/p{p}"));
                let object = Literal::new_simple_literal(format!("{i}"));
                let quad = Quad::new(subject.clone(), predicate, object, GraphName::DefaultGraph);
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
    }

    /// Planners on Rayon workers (SHACL-SPARQL checks) during the first build don't each
    /// scan the store: they plan without the sets and the build is one scan. After a drift
    /// by more than a quarter every planner, on a worker or not, goes on with the old sets
    /// at once, and one rebuild on a thread of its own scans once more.
    #[test]
    fn planners_during_a_build_scan_the_store_once() {
        let model = ReadModel::default();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        add(&engine, 0..3_000);
        let statistics = Arc::clone(&engine.inner.shared.statistics);
        let workers = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let planners = |snapshot: &Snapshot| -> Vec<Option<Arc<CharacteristicSets>>> {
            workers.install(|| {
                (0..64)
                    .into_par_iter()
                    .map(|_| snapshot.characteristic_sets_in(model))
                    .collect()
            })
        };
        let same = |found: &[Option<Arc<CharacteristicSets>>], sets: &Arc<CharacteristicSets>| {
            found
                .iter()
                .all(|s| s.as_ref().is_some_and(|s| Arc::ptr_eq(s, sets)))
        };
        // The first build under way (held here): the workers plan without the sets.
        let snapshot = engine.snapshot();
        let held = statistics.building.lock();
        assert!(planners(&snapshot).iter().all(Option::is_none));
        assert_eq!(statistics.scans(), 0);
        drop(held);
        // A planner off the workers builds them, once; then the workers have them too.
        let first = snapshot.characteristic_sets_in(model).expect("built");
        assert_eq!(statistics.scans(), 1);
        assert!(same(&planners(&snapshot), &first));
        // Drifted, the rebuild held back: every planner has the old sets, none waits.
        add(&engine, 3_000..6_000);
        let snapshot = engine.snapshot();
        let held = statistics.building.lock();
        assert!(same(&planners(&snapshot), &first));
        assert!(same(&[snapshot.characteristic_sets_in(model)], &first));
        drop(held);
        // One rebuild replaces them.
        let started = std::time::Instant::now();
        let size = snapshot.count_in(model, &QuadPattern::in_graph(TermId::DEFAULT_GRAPH));
        while statistics
            .sets
            .lock()
            .get(&model)
            .is_none_or(|s| s.at != size)
        {
            assert!(started.elapsed().as_secs() < 30, "no rebuild");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(statistics.scans(), 2);
        let rebuilt = snapshot.characteristic_sets_in(model).expect("rebuilt");
        assert!(!Arc::ptr_eq(&rebuilt, &first));
        assert_eq!(rebuilt.subjects(), 6_000);
        assert!(same(&planners(&snapshot), &rebuilt));
        assert_eq!(statistics.scans(), 2);
    }
}
