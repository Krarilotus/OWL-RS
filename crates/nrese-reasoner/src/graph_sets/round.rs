//! A round of an evaluation of support graph sets ([`super`]) in parallel tasks: what its
//! derivations give is merged per head as it is found, and every task polls the
//! evaluation's [`Watch`] through a [`Ticker`] between derivations.
//!
//! A round's derivations can outnumber what it derives by far: a transitive relation
//! complete on n terms has n³ instances of its transitivity for n² facts. Collecting one
//! entry per derivation before merging them held all of them at once, with no stop in
//! between (review r1 of N10, F1). Here a task merges what it finds into a part of its
//! own, at most the cap of sets per head, moves the part into the round's shared state at
//! its end, and at every poll reports what all of them hold, with room for what each task
//! running can add before its next poll. So a round holds its heads' annotations and one
//! part per running task, and a refusal, a cancellation (the watch's) or the process's
//! memory limit ends every task at its next poll; nothing gathered is used then.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use hashbrown::{HashMap, HashSet};
use nrese_exec::{POLL_UNITS, Stop, StopCheck, Ticker};
use rayon::prelude::*;

use super::{Annotation, GraphSet, Stopped, Watch, check, table_bytes};
use crate::ir::Triple;

/// Shards of a round's shared state: tasks moving their parts in contend on one each.
const SHARDS: usize = 64;

fn shard_of(fact: &Triple) -> usize {
    let mixed = fact[0].wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17)
        ^ fact[1].wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
        ^ fact[2].wrapping_mul(0x1656_67b1_9e37_79f9);
    (mixed >> 58) as usize % SHARDS
}

/// Why a round ended before its work did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum End {
    /// The watch refused what it holds, or the process passed its memory limit.
    Stopped(Stopped),
    /// The shared state passed the caller's limit ([`Gather::over`]).
    Over,
}

/// How a round's tasks stop. Each polls every [`POLL_UNITS`] units of work with what the
/// evaluation holds: `base` beside the round, the round's shared state, every task's part,
/// and `unit` bytes per unit of work for each task running (what it can add before its
/// next poll). The first stop is kept, and every task ends at its next poll.
pub(super) struct Polling<'w> {
    watch: Watch<'w>,
    base: usize,
    unit: usize,
    /// The bytes of the running tasks' parts, as they last reported them.
    parts: AtomicUsize,
    running: AtomicUsize,
    ended: OnceLock<End>,
    any: AtomicBool,
}

impl<'w> Polling<'w> {
    pub(super) fn new(watch: Watch<'w>, base: usize, unit: usize) -> Self {
        Self {
            watch,
            base,
            unit,
            parts: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            ended: OnceLock::new(),
            any: AtomicBool::new(false),
        }
    }

    fn end(&self, end: End) {
        let _ = self.ended.set(end);
        self.any.store(true, Ordering::Release);
    }

    fn ended(&self) -> bool {
        self.any.load(Ordering::Acquire)
    }

    /// Polls the same latched round while the rule walker tries partial bindings,
    /// including searches that emit no derivation. Local growth is reported by Tick.
    pub(super) fn stopped<G: Gather>(&self, gather: &G) -> bool {
        self.check(gather).is_err()
    }

    fn check<G: Gather>(&self, gather: &G) -> Result<(), Stop> {
        if self.ended() {
            return Err(Stop::Cancelled);
        }
        if gather.over() {
            self.end(End::Over);
            return Err(Stop::Cancelled);
        }
        let room = self.running.load(Ordering::Relaxed) * POLL_UNITS as usize * self.unit;
        let held = self.base + gather.bytes() + self.parts.load(Ordering::Relaxed) + room;
        check(self.watch, held).map_err(|stopped| {
            self.end(End::Stopped(stopped));
            Stop::Cancelled
        })
    }
}

/// What a round's tasks gather: each into a part of its own, moved into the shared state
/// at the task's end.
pub(super) trait Gather: Sync {
    type Part: Default + Send;
    /// Moves `part` into the shared state.
    fn absorb(&self, part: Self::Part);
    /// The bytes the shared state holds.
    fn bytes(&self) -> usize;
    /// Whether the shared state passed the caller's limit: the round ends ([`End::Over`]).
    fn over(&self) -> bool {
        false
    }
}

/// A task's poll: the round's state, its own part's bytes (`local`), and what it reported
/// of them last (`reported`, counted in [`Polling::parts`]).
struct TaskCheck<'a, 'w, G> {
    polling: &'a Polling<'w>,
    gather: &'a G,
    local: &'a Cell<usize>,
    reported: &'a Cell<usize>,
}

impl<G: Gather> StopCheck for TaskCheck<'_, '_, G> {
    fn check(&self) -> Result<(), Stop> {
        let polling = self.polling;
        if polling.ended() {
            return Err(Stop::Cancelled);
        }
        let (now, before) = (self.local.get(), self.reported.replace(self.local.get()));
        if now >= before {
            polling.parts.fetch_add(now - before, Ordering::Relaxed);
        } else {
            polling.parts.fetch_sub(before - now, Ordering::Relaxed);
        }
        polling.check(self.gather)
    }
}

/// A task's countdown to its next poll ([`Ticker`]).
pub(super) struct Tick<'a, 'w, G: Gather> {
    ticker: Ticker<TaskCheck<'a, 'w, G>>,
    local: &'a Cell<usize>,
    done: bool,
}

impl<G: Gather> Tick<'_, '_, G> {
    /// Counts `units` of work done, the part holding `bytes` now; false once the round
    /// ends: the task adds nothing more.
    pub(super) fn tick(&mut self, units: u32, bytes: usize) -> bool {
        if self.done {
            return false;
        }
        self.local.set(bytes);
        if self.ticker.tick(units).is_err() {
            self.done = true;
        }
        !self.done
    }

    /// Whether the round has ended for this task.
    pub(super) fn done(&self) -> bool {
        self.done
    }
}

/// Runs `work` for each of `tasks` in parallel. `work` puts what it finds into its part,
/// ticking each unit of work with the part's bytes, and adds nothing once a tick says no.
/// A part goes into `gather` when its task ends unstopped. `Err` with the first stop:
/// nothing gathered is to be used then.
#[allow(clippy::type_complexity)]
pub(super) fn run<T: Sync, G: Gather>(
    tasks: &[T],
    gather: &G,
    polling: &Polling<'_>,
    work: &(dyn Fn(&T, &mut G::Part, &mut Tick<'_, '_, G>) + Sync),
) -> Result<(), End> {
    tasks.par_iter().for_each(|task| {
        if polling.ended() {
            return;
        }
        let (local, reported) = (Cell::new(0), Cell::new(0));
        polling.running.fetch_add(1, Ordering::Relaxed);
        let check = TaskCheck {
            polling,
            gather,
            local: &local,
            reported: &reported,
        };
        if let Ok(ticker) = Ticker::new(check) {
            let mut tick = Tick {
                ticker,
                local: &local,
                done: false,
            };
            let mut part = G::Part::default();
            work(task, &mut part, &mut tick);
            if !tick.done && !polling.ended() {
                gather.absorb(part);
            }
        }
        polling.running.fetch_sub(1, Ordering::Relaxed);
        polling.parts.fetch_sub(reported.get(), Ordering::Relaxed);
    });
    match polling.ended.get() {
        Some(&end) => Err(end),
        None => match gather.over() {
            true => Err(End::Over),
            false => Ok(()),
        },
    }
}

/// Adds `set` to the minimal sets `into` keeps: nothing if a kept one is contained in it,
/// else in place of the kept ones that contain it; past `cap` the largest go, and `into`
/// is marked, as [`super::minimal`] decides. The change of its heap bytes.
pub(super) fn add_set(into: &mut Annotation, set: GraphSet, cap: usize) -> isize {
    if into.sets.iter().any(|kept| kept.is_subset(&set)) {
        return 0;
    }
    let before = into.heap_bytes() as isize;
    into.sets.retain(|kept| !set.is_subset(kept));
    into.sets.push(set);
    if into.sets.len() > cap {
        into.sets
            .sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        into.sets.truncate(cap);
        into.truncated = true;
    }
    into.heap_bytes() as isize - before
}

/// The bytes one set added to a part can add at most: an entry for its head (with room
/// for the table's growth), the set's place in a list, and its words past two
/// (`set_heap`, the most any set of the evaluation holds).
pub(super) fn unit_bytes(set_heap: usize) -> usize {
    2 * (size_of::<(Triple, Annotation)>() + 1) + size_of::<GraphSet>() + set_heap
}

/// Per head, the sets a round's derivations give it, minimal and at most `cap`, and
/// whether one of them was marked or cut ([`Annotation`]): merged as they are found, so
/// the round holds its heads' annotations, never one entry per derivation.
pub(super) struct Heads {
    cap: usize,
    shards: Vec<Mutex<HashMap<Triple, Annotation>>>,
    bytes: AtomicUsize,
}

/// A task's part of [`Heads`].
#[derive(Default)]
pub(super) struct HeadsPart {
    map: HashMap<Triple, Annotation>,
    heap: usize,
}

impl HeadsPart {
    /// Adds `set` to what `head` is given, marked if `mark`.
    pub(super) fn add(&mut self, head: Triple, set: GraphSet, mark: bool, cap: usize) {
        let entry = self.map.entry(head).or_default();
        entry.truncated |= mark;
        let delta = add_set(entry, set, cap);
        self.heap = self.heap.saturating_add_signed(delta);
    }

    /// Marks what `head` is given, without a set: its sets may be missing one.
    pub(super) fn mark(&mut self, head: Triple) {
        self.map.entry(head).or_default().truncated = true;
    }

    /// Gives `head` all of `found`.
    pub(super) fn put(&mut self, head: Triple, found: Annotation, cap: usize) {
        let truncated = found.truncated;
        for set in found.sets {
            self.add(head, set, false, cap);
        }
        self.map.entry(head).or_default().truncated |= truncated;
    }

    pub(super) fn bytes(&self) -> usize {
        table_bytes::<Triple, Annotation>(self.map.capacity()) + self.heap
    }
}

impl Heads {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            cap,
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
            bytes: AtomicUsize::new(0),
        }
    }

    /// Every head with what it was given.
    pub(super) fn into_heads(self) -> impl Iterator<Item = (Triple, Annotation)> {
        self.shards
            .into_iter()
            .flat_map(|shard| shard.into_inner().unwrap_or_else(|p| p.into_inner()))
    }
}

impl Gather for Heads {
    type Part = HeadsPart;

    fn absorb(&self, part: HeadsPart) {
        let mut by_shard: Vec<Vec<(Triple, Annotation)>> = vec![Vec::new(); SHARDS];
        for (head, found) in part.map {
            by_shard[shard_of(&head)].push((head, found));
        }
        for (k, found) in by_shard.into_iter().enumerate() {
            if found.is_empty() {
                continue;
            }
            let mut shard = self.shards[k].lock().unwrap_or_else(|p| p.into_inner());
            let table = shard.capacity();
            let mut heap: isize = 0;
            for (head, found) in found {
                let entry = shard.entry(head).or_default();
                entry.truncated |= found.truncated;
                for set in found.sets {
                    heap += add_set(entry, set, self.cap);
                }
            }
            let grown = table_bytes::<Triple, Annotation>(shard.capacity().saturating_sub(table));
            let added = heap + grown as isize;
            if added >= 0 {
                self.bytes.fetch_add(added as usize, Ordering::Relaxed);
            } else {
                self.bytes
                    .fetch_sub(added.unsigned_abs(), Ordering::Relaxed);
            }
        }
    }

    fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }
}

/// The facts a round reaches that weren't reached before, each once, at most `limit` of
/// them: past it the round ends ([`End::Over`]).
pub(super) struct Facts {
    limit: usize,
    shards: Vec<Mutex<HashSet<Triple>>>,
    count: AtomicUsize,
    bytes: AtomicUsize,
}

impl Facts {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
            count: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }

    /// The facts reached, unordered.
    pub(super) fn into_facts(self) -> Vec<Triple> {
        self.shards
            .into_iter()
            .flat_map(|shard| shard.into_inner().unwrap_or_else(|p| p.into_inner()))
            .collect()
    }
}

impl Gather for Facts {
    type Part = HashSet<Triple>;

    fn absorb(&self, part: HashSet<Triple>) {
        let mut by_shard: Vec<Vec<Triple>> = vec![Vec::new(); SHARDS];
        for fact in part {
            by_shard[shard_of(&fact)].push(fact);
        }
        for (k, facts) in by_shard.into_iter().enumerate() {
            if facts.is_empty() {
                continue;
            }
            let mut shard = self.shards[k].lock().unwrap_or_else(|p| p.into_inner());
            let (table, len) = (shard.capacity(), shard.len());
            shard.extend(facts);
            self.count.fetch_add(shard.len() - len, Ordering::Relaxed);
            let grown = table_bytes::<Triple, ()>(shard.capacity().saturating_sub(table));
            self.bytes.fetch_add(grown, Ordering::Relaxed);
        }
    }

    fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    fn over(&self) -> bool {
        self.count.load(Ordering::Relaxed) > self.limit
    }
}
