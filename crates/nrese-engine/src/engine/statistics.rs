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
//!   every planner that arrives meanwhile, on a Rayon worker or not, plans with them too.
//!   No lock is held while they build ([`Build`]): each scan is a queue of subject slices,
//!   run by the builder, by lanes it spawns, and by every planner that waits, slice by
//!   slice, each slice alone (no Rayon inside it). A planner sleeps only once the queue
//!   is empty, for slices others are running, which wait for nothing: none waits on
//!   another job it took on, and none takes one on while it waits.
//!
//! A scan stops once the slices have found more than
//! [`MAX_SETS`](super::characteristic::MAX_SETS) sets between them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::{Condvar, Mutex};

use super::characteristic::{CharacteristicSets, PairCounts, Partial, SetCount};
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
    /// Per read model, the build of its sets under way: one at a time, and planners that
    /// need the sets meanwhile help it and wait for it.
    building: Mutex<HashMap<ReadModel, Arc<Build>>>,
    /// Where they are saved and read back (`derived/` of a durable engine).
    dir: Mutex<Option<std::path::PathBuf>>,
    /// Scans of the store for the sets begun: what the tests count.
    scans: AtomicU64,
    /// Called once by the next build, its first scan published and not begun: what the
    /// tests hold a build with.
    #[cfg(test)]
    hook: Mutex<Option<Hook>>,
}

/// What a test runs at the next build's first scan ([`Statistics::hook`]).
#[cfg(test)]
type Hook = Box<dyn FnOnce(&Build) + Send>;

/// A build of a read model's sets under way: its current scan, which planners that wait
/// help with, and once it is over what it built (`Some(None)`: too many sets, or nothing).
#[derive(Default)]
struct Build {
    state: Mutex<BuildState>,
    changed: Condvar,
    /// Planners asleep on it (they have helped with what there was).
    waiting: AtomicUsize,
}

#[derive(Default)]
struct BuildState {
    work: Option<Arc<dyn Help>>,
    done: Option<Option<Arc<CharacteristicSets>>>,
}

impl Build {
    fn publish(&self, work: Option<Arc<dyn Help>>) {
        self.state.lock().work = work;
        self.changed.notify_all();
    }

    /// The build's outcome, its scans helped with until then.
    fn wait(&self) -> Option<Arc<CharacteristicSets>> {
        loop {
            let work = {
                let state = self.state.lock();
                if let Some(done) = &state.done {
                    return done.clone();
                }
                state.work.clone()
            };
            if let Some(work) = &work {
                work.help();
            }
            let mut state = self.state.lock();
            if let Some(done) = &state.done {
                return done.clone();
            }
            let same = match (&state.work, &work) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if same {
                self.waiting.fetch_add(1, Ordering::SeqCst);
                self.changed.wait(&mut state);
                self.waiting.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

/// A scan's slices, as a queue: whoever helps claims the next and runs it alone.
trait Help: Send + Sync {
    fn help(&self);
}

/// One scan of a build: `run(i)` for every slice `i`, its results kept in slice order; a
/// slice that finds too much (`None`) stops the rest.
struct Scan<R> {
    run: Box<dyn Fn(usize) -> Option<R> + Send + Sync>,
    len: usize,
    next: AtomicUsize,
    failed: AtomicBool,
    results: Mutex<Vec<Option<R>>>,
    /// Slices not yet over (claimed or not); `over` is notified when none are left.
    left: Mutex<usize>,
    over: Condvar,
}

impl<R: Send + 'static> Scan<R> {
    fn new(len: usize, run: impl Fn(usize) -> Option<R> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            run: Box::new(run),
            len,
            next: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            results: Mutex::new(std::iter::repeat_with(|| None).take(len).collect()),
            left: Mutex::new(len),
            over: Condvar::new(),
        })
    }

    /// Every slice's result in order, once all are over: helped with by `lanes` jobs on
    /// the current Rayon pool, by this thread, and by `build`'s waiting planners where it
    /// is published there. `None` if a slice found too much.
    fn run(self: Arc<Self>, build: &Build, lanes: usize, publish: bool) -> Option<Vec<R>> {
        if publish {
            build.publish(Some(Arc::clone(&self) as Arc<dyn Help>));
        }
        for _ in 0..lanes.min(self.len) {
            let scan = Arc::clone(&self);
            rayon::spawn(move || scan.help());
        }
        // A builder outside the pool waits for the lanes, as for a parallel loop, and
        // allocates nothing for the scan: its heap is the queries' after a load. With no
        // lanes, or on a pool thread, it scans too.
        if lanes == 0 || rayon::current_thread_index().is_some() {
            self.help();
        }
        let mut left = self.left.lock();
        while *left > 0 {
            self.over.wait(&mut left);
        }
        drop(left);
        if publish {
            build.publish(None);
        }
        if self.failed.load(Ordering::SeqCst) {
            return None;
        }
        std::mem::take(&mut *self.results.lock())
            .into_iter()
            .collect()
    }
}

impl<R: Send> Help for Scan<R> {
    fn help(&self) {
        /// A slice over, whether it ran, failed, or panicked.
        struct Over<'s, R>(&'s Scan<R>);
        impl<R> Drop for Over<'_, R> {
            fn drop(&mut self) {
                let mut left = self.0.left.lock();
                *left -= 1;
                if *left == 0 {
                    self.0.over.notify_all();
                }
            }
        }
        loop {
            let i = self.next.fetch_add(1, Ordering::SeqCst);
            if i >= self.len {
                return;
            }
            let _over = Over(self);
            if self.failed.load(Ordering::SeqCst) {
                continue;
            }
            match (self.run)(i) {
                Some(result) => self.results.lock()[i] = Some(result),
                None => self.failed.store(true, Ordering::SeqCst),
            }
        }
    }
}

/// The builder's claim on a read model's build: over when dropped, its outcome given to
/// the planners that wait.
struct Builder<'s> {
    statistics: &'s Statistics,
    model: ReadModel,
    build: Arc<Build>,
}

impl Drop for Builder<'_> {
    fn drop(&mut self) {
        {
            let mut state = self.build.state.lock();
            state.work = None;
            // A build that panicked leaves its planners without the sets.
            state.done.get_or_insert(None);
        }
        self.build.changed.notify_all();
        self.statistics.building.lock().remove(&self.model);
    }
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
        match self.claim(model) {
            Ok(builder) => self.build_sets(&builder, snapshot, model, size, true),
            // Another planner's build: helped with and waited for.
            Err(build) => build.wait(),
        }
    }

    /// The build of `model`'s sets for this caller to run, unless one is under way (then
    /// that one), or none is needed any more. Under `building` only briefly.
    fn claim(&self, model: ReadModel) -> Result<Builder<'_>, Arc<Build>> {
        let mut building = self.building.lock();
        if let Some(build) = building.get(&model) {
            return Err(Arc::clone(build));
        }
        // Built meanwhile: kept since (a build keeps its sets before it lets go).
        if let Some(kept) = self.sets.lock().get(&model) {
            let build = Build::default();
            build.state.lock().done = Some(kept.built.clone());
            return Err(Arc::new(build));
        }
        let build = Arc::new(Build::default());
        building.insert(model, Arc::clone(&build));
        Ok(Builder {
            statistics: self,
            model,
            build,
        })
    }

    /// [`Self::claim`] for a build that must run whatever is kept: a build under way is
    /// waited for first.
    fn claim_anyway(&self, model: ReadModel) -> Builder<'_> {
        loop {
            let waited = {
                let mut building = self.building.lock();
                match building.get(&model) {
                    Some(build) => Arc::clone(build),
                    None => {
                        let build = Arc::new(Build::default());
                        building.insert(model, Arc::clone(&build));
                        return Builder {
                            statistics: self,
                            model,
                            build,
                        };
                    }
                }
            };
            waited.wait();
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
                let builder = statistics.claim_anyway(model);
                let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
                let size = snapshot.count_in(model, &pattern);
                statistics.build_sets(&builder, &snapshot, model, size, false);
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
        let builder = self.claim_anyway(model);
        self.build_sets(&builder, snapshot, model, size, true);
    }

    /// The sets of `model` scanned (on every core or on this thread alone), kept as built
    /// at `size` and saved, and given to the planners that wait for `builder`'s build.
    fn build_sets(
        &self,
        builder: &Builder<'_>,
        snapshot: &Snapshot,
        model: ReadModel,
        size: u64,
        every_core: bool,
    ) -> Option<Arc<CharacteristicSets>> {
        let started = std::time::Instant::now();
        self.scans.fetch_add(1, Ordering::Relaxed);
        let built = self.scan_sets(&builder.build, snapshot, model, every_core);
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
        builder.build.state.lock().done = Some(built.clone());
        built
    }

    /// Scans of the store for the characteristic sets begun so far.
    #[cfg(test)]
    pub(crate) fn scans(&self) -> u64 {
        self.scans.load(Ordering::Relaxed)
    }

    /// The sets of `model` from two scans in subject order over slices of the subjects, on
    /// every core (lanes on the current Rayon pool, and the planners that wait for
    /// `build`) or on this thread alone; the first stops once the slices have found more
    /// than [`MAX_SETS`](super::characteristic::MAX_SETS) between them.
    fn scan_sets(
        &self,
        build: &Build,
        snapshot: &Snapshot,
        model: ReadModel,
        every_core: bool,
    ) -> Option<Arc<CharacteristicSets>> {
        let ranges: Arc<[(TermId, TermId)]> = subject_ranges(
            snapshot.dictionary_len(),
            rayon::current_num_threads() as u64 * 4,
        )
        .into();
        // As many lanes as the pool has threads beside this one.
        let lanes = match every_core {
            true => {
                rayon::current_num_threads() - usize::from(rayon::current_thread_index().is_some())
            }
            false => 0,
        };
        // Slice `i` of the default graph, in subject order.
        fn slice<'s>(
            snapshot: &'s Snapshot,
            model: ReadModel,
            ranges: &[(TermId, TermId)],
            i: usize,
        ) -> Option<impl Iterator<Item = crate::quad::EncodedQuad> + 's> {
            let (low, high) = ranges[i];
            let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
            snapshot.scan_range_in(model, &pattern, Permutation::Gspo, low, high)
        }
        let count = Arc::new(SetCount::default());
        let parts = Scan::new(ranges.len(), {
            let (snapshot, ranges) = (snapshot.clone(), Arc::clone(&ranges));
            move |i| {
                slice(&snapshot, model, &ranges, i).and_then(|quads| Partial::of(quads, &count))
            }
        });
        #[cfg(test)]
        let hook = self.hook.lock().take();
        #[cfg(test)]
        if let Some(hook) = hook {
            build.publish(Some(Arc::clone(&parts) as Arc<dyn Help>));
            hook(build);
        }
        let mut sets = CharacteristicSets::merge(parts.run(build, lanes, every_core)?)?;
        // The pairs need each subject's set first: a second scan.
        let subjects: Arc<[(u64, u32)]> = sets.take_subjects().into();
        if subjects.is_empty() {
            return Some(Arc::new(sets));
        }
        let snapshot = snapshot.clone();
        let pairs: Arc<Scan<PairCounts>> = Scan::new(ranges.len(), move |i| {
            slice(&snapshot, model, &ranges, i)
                .and_then(|quads| CharacteristicSets::pair_counts(&subjects, quads))
        });
        Some(Arc::new(
            sets.with_pair_counts(pairs.run(build, lanes, every_core)),
        ))
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

    /// Holds the next build once its first scan is published: `planners` start, and the
    /// build goes on once `asleep` of them sleep on it.
    fn holding(statistics: &Statistics, asleep: usize, planners: impl FnOnce() + Send + 'static) {
        *statistics.hook.lock() = Some(Box::new(move |build: &Build| {
            planners();
            let started = std::time::Instant::now();
            while build.waiting.load(Ordering::SeqCst) < asleep {
                assert!(
                    started.elapsed().as_secs() < 30,
                    "the planners never waited"
                );
                std::thread::yield_now();
            }
        }));
    }

    /// Planners on Rayon workers (SHACL-SPARQL checks) that arrive during the first build
    /// plan with the complete sets, as every other planner does (q02: no plan by when a
    /// query came), and the build stays one scan: 64 planners on a pool of two threads,
    /// nested in its joins, each asleep on the build or helping it. Workers planned
    /// without the sets before. After a drift by more than a quarter every planner goes on
    /// with the old sets at once, and one rebuild on a thread of its own scans once more.
    #[test]
    fn planners_during_a_build_plan_with_the_complete_sets() {
        let model = ReadModel::default();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        add(&engine, 0..3_000);
        let statistics = Arc::clone(&engine.inner.shared.statistics);
        let workers = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
        );
        let planners = move |workers: &rayon::ThreadPool, snapshot: &Snapshot| {
            workers.install(|| {
                (0..64)
                    .into_par_iter()
                    .map(|_| snapshot.characteristic_sets_in(model))
                    .collect::<Vec<_>>()
            })
        };
        let same = |found: &[Option<Arc<CharacteristicSets>>], sets: &Arc<CharacteristicSets>| {
            found
                .iter()
                .all(|s| s.as_ref().is_some_and(|s| Arc::ptr_eq(s, sets)))
        };
        let snapshot = engine.snapshot();
        let (found, during) = std::sync::mpsc::channel();
        holding(&statistics, 2, {
            let (workers, snapshot) = (Arc::clone(&workers), snapshot.clone());
            move || {
                std::thread::spawn(move || {
                    let _ = found.send(planners(&workers, &snapshot));
                });
            }
        });
        let first = snapshot.characteristic_sets_in(model).expect("built");
        let during = during
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the planners finished");
        assert!(same(&during, &first));
        assert_eq!(statistics.scans(), 1);
        assert!(same(&planners(&workers, &snapshot), &first));
        // Drifted, the rebuild held back: every planner has the old sets, none waits.
        add(&engine, 3_000..6_000);
        let snapshot = engine.snapshot();
        let held = statistics.claim_anyway(model);
        assert!(same(&planners(&workers, &snapshot), &first));
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
        assert!(same(&planners(&workers, &snapshot), &rebuilt));
        assert_eq!(statistics.scans(), 2);
    }

    /// A pool's only thread that plans during a build started outside it, with no lanes
    /// beside the builder, scans the whole first scan itself and then has the complete
    /// sets: a planner that waits never needs a thread of its own pool to get on.
    #[test]
    fn a_pools_only_thread_runs_the_build_it_waits_for() {
        let model = ReadModel::default();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        add(&engine, 0..3_000);
        let statistics = Arc::clone(&engine.inner.shared.statistics);
        let one = || {
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap()
        };
        let (planners, builders) = (Arc::new(one()), one());
        let snapshot = engine.snapshot();
        let (found, planned) = std::sync::mpsc::channel();
        // The builder holds back until the planner sleeps: it has scanned every slice.
        holding(&statistics, 1, {
            let snapshot = snapshot.clone();
            move || {
                planners.spawn(move || {
                    let _ = found.send(snapshot.characteristic_sets_in(model));
                });
            }
        });
        // On a pool of one thread the builder spawns no lanes.
        let built = builders
            .install(|| snapshot.characteristic_sets_in(model))
            .expect("built");
        let planned = planned
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the planner finished")
            .expect("the planner has the sets");
        assert!(Arc::ptr_eq(&planned, &built));
        assert_eq!(statistics.scans(), 1);
    }
}
