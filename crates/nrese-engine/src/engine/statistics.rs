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
//!   No lock is held while they build: a sliced build of [`nrese_exec::cell`], each scan
//!   a queue of subject slices run by the builder, by lanes it spawns, and by every planner
//!   that waits, slice by slice, each slice alone (no Rayon inside it).
//!
//! The sets kept are charged to the budget the engine's owner gives it
//! ([`Statistics::set_budget`]; none: unlimited), for as long as they are kept: built into
//! charged vectors ([`Gathered::kept`]), so that the charge is what they hold. A rebuild
//! builds and charges the new sets while the old are still kept, then replaces them: while
//! a rebuild ends the budget must hold the old sets, the new ones and the build's charged
//! scratch (its sort keys), not only two copies. Where it can't take the new ones, the
//! old stay, and the next drift tries again; a refusal never leaves fewer sets than before.
//!
//! A scan stops once the slices have found more than
//! [`MAX_SETS`](super::characteristic::MAX_SETS) sets between them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use nrese_exec::budget::Budget;
use nrese_exec::cell::{self, Build, BuildCell, Builder, Claim, Kind, Scan};
use parking_lot::Mutex;

use super::characteristic::{CharacteristicSets, Gathered, PairCounts, Partial, SetCount};
use super::{ReadModel, Snapshot, Version};
use crate::quad::{GraphSelector, Permutation, QuadPattern};
use crate::term::{TermId, TermKind};

/// The characteristic sets of one read model as built at a size (`None`: there were too
/// many to keep, or the budget couldn't take them), and whether a rebuild is under way.
struct Sets {
    built: Option<cell::Kept<CharacteristicSets>>,
    at: u64,
    rebuilding: bool,
}

impl Sets {
    fn fits(&self, size: u64) -> bool {
        size.abs_diff(self.at) <= self.at / 4
    }

    fn sets(&self) -> Option<Arc<CharacteristicSets>> {
        self.built.as_ref().map(|kept| Arc::clone(kept.value()))
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
    building: BuildCell<ReadModel, CharacteristicSets>,
    /// What the sets kept are charged to (`None`: an unlimited budget of their own).
    budget: Mutex<Option<Arc<Budget>>>,
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
type Hook = Box<dyn FnOnce(&Build<CharacteristicSets>) + Send>;

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
    /// time, by one planner while the others help with it and wait, on Rayon workers
    /// too ([`Build`]); none where that build failed (the next caller builds again).
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
        let kept = || self.sets.lock().get(&model).map(Sets::sets);
        match self.building.claim(model, Kind::Sliced, kept) {
            Claim::Build(builder) => self.build_sets(&builder, snapshot, model, size, true),
            // Another planner's build: helped with and waited for (a sliced build turns no
            // waiter away).
            Claim::Wait(build) => build.wait().ok().flatten(),
        }
    }

    /// The build of `model`'s sets for a caller that must build whatever is kept: a build
    /// under way is waited for first.
    fn claim_anyway(&self, model: ReadModel) -> Builder<'_, ReadModel, CharacteristicSets> {
        match self.building.claim_anyway(model, Kind::Sliced) {
            Ok(builder) => builder,
            Err(_) => unreachable!("a sliced build turns no waiter away"),
        }
    }

    /// What the sets kept are charged to from now on (a store's own envelope within its
    /// catalog's budget).
    pub(crate) fn set_budget(&self, budget: Arc<Budget>) {
        *self.budget.lock() = Some(budget);
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
                // Over the budget: none until the next build (there were none before).
                vacant.insert(Sets {
                    built: built.and_then(|sets| self.keep(sets).ok()),
                    at,
                    rebuilding: false,
                })
            }
        };
        if !kept.fits(size) && !kept.rebuilding {
            kept.rebuilding = true;
            self.rebuild(snapshot, model);
        }
        Kept::Sets(kept.sets())
    }

    /// `sets` as kept, built into vectors charged to the budget ([`Gathered::kept`]); an
    /// error, nothing charged, where it can't take them.
    fn keep(
        &self,
        sets: Gathered,
    ) -> Result<cell::Kept<CharacteristicSets>, nrese_exec::BudgetExceeded> {
        let budget = self.budget.lock().clone();
        let budget = budget.unwrap_or_else(|| Arc::new(Budget::unlimited()));
        cell::Kept::new(sets.kept(&budget)?, budget)
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
        builder: &Builder<'_, ReadModel, CharacteristicSets>,
        snapshot: &Snapshot,
        model: ReadModel,
        size: u64,
        every_core: bool,
    ) -> Option<Arc<CharacteristicSets>> {
        let started = std::time::Instant::now();
        self.scans.fetch_add(1, Ordering::Relaxed);
        let built = self.scan_sets(builder.build(), snapshot, model, every_core);
        tracing::debug!(
            ?model,
            statements = size,
            sets = built.as_ref().map_or(0, |s| s.len()),
            pairs = built.as_ref().is_some_and(|s| s.has_pairs()),
            every_core,
            ms = started.elapsed().as_millis() as u64,
            "characteristic sets built"
        );
        self.save_sets(snapshot, model, size, built.as_ref());
        // Built and charged while the old sets are still kept, then put in their place.
        let kept = match built.map(|sets| self.keep(sets)) {
            Some(Ok(kept)) => Some(kept),
            // Too many sets: none kept at this size.
            None => None,
            // Over the budget: the old sets stay (none if there were none); the next
            // drift tries again.
            Some(Err(error)) => {
                tracing::warn!(%error, ?model, "characteristic sets not replaced: over the budget");
                let mut sets = self.sets.lock();
                let old = match sets.get_mut(&model) {
                    Some(old) => {
                        old.rebuilding = false;
                        old.sets()
                    }
                    None => {
                        sets.insert(
                            model,
                            Sets {
                                built: None,
                                at: size,
                                rebuilding: false,
                            },
                        );
                        None
                    }
                };
                builder.finish(old.clone());
                return old;
            }
        };
        let built = kept.as_ref().map(|kept| Arc::clone(kept.value()));
        let replaced = self.sets.lock().insert(
            model,
            Sets {
                built: kept,
                at: size,
                rebuilding: false,
            },
        );
        // The old sets let go after the swap, outside the lock.
        drop(replaced);
        builder.finish(built.clone());
        built
    }

    /// What the sets of `model` kept are charged (their vectors' and their `Arc`'s).
    #[cfg(test)]
    fn charged(&self, model: ReadModel) -> usize {
        self.sets
            .lock()
            .get(&model)
            .and_then(|s| s.built.as_ref())
            .map_or(0, |kept| kept.bytes() + kept.value().charged())
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
        build: &Build<CharacteristicSets>,
        snapshot: &Snapshot,
        model: ReadModel,
        every_core: bool,
    ) -> Option<Gathered> {
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
            parts.publish_on(build);
            hook(build);
        }
        let mut sets = Gathered::merge(parts.run(build, lanes, every_core)?)?;
        // The pairs need each subject's set first: a second scan.
        let subjects: Arc<[(u64, u32)]> = sets.take_subjects().into();
        if subjects.is_empty() {
            return Some(sets);
        }
        let snapshot = snapshot.clone();
        let pairs: Arc<Scan<PairCounts>> = Scan::new(ranges.len(), move |i| {
            slice(&snapshot, model, &ranges, i)
                .and_then(|quads| Gathered::pair_counts(&subjects, quads))
        });
        Some(sets.with_pair_counts(pairs.run(build, lanes, every_core)))
    }

    /// Where the derived files are (a durable engine's `derived/`).
    pub(crate) fn set_dir(&self, dir: std::path::PathBuf) {
        *self.dir.lock() = Some(dir);
    }

    fn sets_name(model: ReadModel) -> String {
        format!("statistics-{model:?}").to_ascii_lowercase()
    }

    fn save_sets(&self, snapshot: &Snapshot, model: ReadModel, size: u64, sets: Option<&Gathered>) {
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
    fn load_sets(&self, snapshot: &Snapshot, model: ReadModel) -> Option<(Option<Gathered>, u64)> {
        let dir = self.dir.lock().clone()?;
        let dictionary = snapshot.dictionary();
        let (_, loaded) = crate::term::derived::load(&dir, &Self::sets_name(model), |covered| {
            dictionary.fingerprint(covered)
        })?;
        let mut reader = loaded.reader();
        let size = reader.u64()?;
        let sets = match reader.u32()? {
            0 => None,
            _ => Some(Gathered::read(&mut reader)?),
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
        *statistics.hook.lock() = Some(Box::new(move |build: &Build<CharacteristicSets>| {
            planners();
            let started = std::time::Instant::now();
            while build.waiting() < asleep {
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
    /// The sets kept are charged to the owner's budget, the rebuilt ones in place of the old.
    #[test]
    fn planners_during_a_build_plan_with_the_complete_sets() {
        let model = ReadModel::default();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let budget = Arc::new(Budget::unlimited());
        engine.charge_statistics_to(Arc::clone(&budget));
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
        assert_eq!(budget.used(), statistics.charged(model));
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
        // The old sets' vectors stay charged while anyone holds them: here, until now.
        drop((first, during));
        assert_eq!(budget.used(), statistics.charged(model));
        assert!(same(&planners(&workers, &snapshot), &rebuilt));
        assert_eq!(statistics.scans(), 2);
    }

    /// A rebuild of the same sets under a budget that holds one copy keeps the old ones,
    /// charged as before; one that holds two replaces them, and the old ones' charge goes
    /// with them. Sol's review of 6c683347: the rebuild that didn't fit dropped the kept sets
    /// and left none (and nothing charged).
    #[test]
    fn a_rebuild_the_budget_cannot_take_keeps_the_old_sets() {
        let model = ReadModel::default();
        let engine = |budget: &Arc<Budget>| {
            let engine = Engine::new(EngineConfig::default()).unwrap();
            add(&engine, 0..300);
            engine.charge_statistics_to(Arc::clone(budget));
            engine.prepare_statistics();
            engine
        };
        let unlimited = Arc::new(Budget::unlimited());
        let _measured = engine(&unlimited);
        let one = unlimited.used();
        assert!(one > 0);
        for (copies, replaced) in [(1, false), (2, true)] {
            let budget = Arc::new(Budget::new(copies * one));
            let engine = engine(&budget);
            let first = engine
                .snapshot()
                .characteristic_sets_in(model)
                .expect("kept");
            engine.prepare_statistics();
            let after = engine
                .snapshot()
                .characteristic_sets_in(model)
                .expect("still kept after the rebuild");
            assert_eq!(!Arc::ptr_eq(&first, &after), replaced, "{copies} copies");
            // Replaced, the old sets' vectors stay charged while anyone holds them.
            drop(first);
            assert_eq!(budget.used(), one, "{copies} copies");
        }
    }

    /// What the kept sets hold is what they are charged, at any number of predicates: they
    /// are built into charged vectors, and their `Arc`'s block is charged before it is
    /// allocated. Sol's review of 6c683347: a charge estimated from the maps' capacities,
    /// and the `Arc` charged after it was allocated, left 81,968 bytes uncharged at 4,096
    /// predicates.
    #[test]
    fn kept_sets_hold_no_more_than_they_charge() {
        if !nrese_exec::measure::alone() {
            return;
        }
        for predicates in [16u64, 256, 4096] {
            let statistics = Statistics::default();
            let budget = Arc::new(Budget::unlimited());
            statistics.set_budget(Arc::clone(&budget));
            // A chain over `predicates` subjects, each with a predicate of its own: as many
            // sets as predicates, and as many pairs.
            let id = |raw| TermId::from_raw(raw);
            let quads: Vec<crate::quad::EncodedQuad> = (0..predicates)
                .map(|i| {
                    let (s, o) = (id(1 + i), id(1 + (i + 1) % predicates));
                    crate::quad::EncodedQuad::new(s, id(1_000_000 + i), o, TermId::DEFAULT_GRAPH)
                })
                .collect();
            let gathered = Gathered::build(quads.clone().into_iter())
                .unwrap()
                .with_pairs(quads.into_iter());
            let (kept, excess) =
                nrese_exec::measure::measure_within(&budget, || statistics.keep(gathered).unwrap());
            assert_eq!(kept.value().len(), predicates as usize);
            assert!(kept.value().has_pairs());
            assert_eq!(
                excess, 0,
                "{predicates} predicates: {excess} bytes beyond the charge"
            );
            drop(kept);
            assert_eq!(budget.used(), 0);
        }
    }

    /// A slice that panics on a lane leaves no build stuck: the lane goes on with the queue,
    /// the builder outside the pool (which only waits for its lanes) gets the panic, the
    /// planners asleep on the build wake without sets, and the next caller builds again.
    /// Before, the panic left the lane (Rayon aborts the process on it, or with a handler
    /// that returns the queue stays unclaimed) and the builder waited for good.
    #[test]
    fn a_panicking_slice_fails_the_build_and_wakes_its_planners() {
        let model = ReadModel::default();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        add(&engine, 0..300);
        // The builder's thread borrows the statistics for good (a test's leak).
        let statistics: &'static Statistics =
            Box::leak(Box::new(Arc::clone(&engine.inner.shared.statistics)));
        let Claim::Build(builder) = statistics.building.claim(model, Kind::Sliced, || None) else {
            panic!("no build yet");
        };
        let Claim::Wait(build) = statistics.building.claim(model, Kind::Sliced, || None) else {
            panic!("one build at a time");
        };
        let (woke, planner) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = woke.send(build.wait().ok().flatten());
        });
        let (over, failed) = std::sync::mpsc::channel();
        let scanned = {
            std::thread::spawn(move || {
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let scan = Scan::new(2, |i| -> Option<u8> {
                        assert!(i != 0, "the injected slice panic");
                        Some(1)
                    });
                    // Not published: the planner sleeps, the lane alone runs the slices.
                    let ran = scan.run(builder.build(), 1, false);
                    drop(builder);
                    ran
                }));
                let _ = over.send(ran.is_err());
            })
        };
        let patience = std::time::Duration::from_secs(10);
        assert_eq!(
            failed.recv_timeout(patience),
            Ok(true),
            "the builder got the panic"
        );
        scanned.join().unwrap();
        assert!(matches!(planner.recv_timeout(patience), Ok(None)));
        assert!(statistics.building.is_idle());
        assert!(statistics.sets.lock().get(&model).is_none());
        assert!(engine.snapshot().characteristic_sets_in(model).is_some());
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
