//! The contexts and their scheduling (docs/design/owl2-dl.md §5, "Parallelism"; ELK's
//! scheme, *The Incredible ELK*, JAR 2014).
//!
//! - **Activation.** Each context has an inbox and an activation flag. Whoever delivers a
//!   message to an inactive context sets the flag (compare-and-swap) and schedules it: on
//!   a rayon scope, whose work stealing shares the active contexts among the workers, or
//!   on a plain queue with one thread. A context's state is only touched by the one worker
//!   holding its activation, so its lock is never contended.
//! - **Contexts are created as the rules need them** (Succ, through the expansion
//!   strategy), in an arena of segments that never move, so a context is found by its id
//!   without a lock; the registry of cores (one context per core) takes a lock only to
//!   look a core up or create its context.
//! - **Termination:** the scope ends when no context is active or has mail.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use hashbrown::HashMap;

use super::atoms::Atom;
use super::memory::{Interval, Observer};
use super::program::Program;
use super::rules::{Message, Out, State, Worker};
use super::state::ContextId;

#[path = "scheduling.rs"]
mod scheduling;

/// How the Succ rule picks a successor's context (Bate et al., §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// One context per filler `B` of `∃R.B` (core `B(x)`) when `B(f(x))` holds
    /// unconditionally, else the context with the empty core: at most linearly many
    /// contexts, as the EL calculi have.
    #[default]
    Cautious,
    /// One context per set `K₁` of what certainly holds for the successor: more, smaller
    /// contexts.
    Eager,
    /// As `Cautious`, but a successor whose filler isn't certain gets an empty-core
    /// context of its own Skolem function instead of the one shared by all: the possible
    /// atoms of unrelated successors then never combine in one context (on the ORE
    /// development set the shared one held 25-80 % of all clauses, 99 % of what it
    /// derived redundant).
    Split,
}

/// A context: its inbox, activation flag, core and state.
#[derive(Default)]
pub struct Context {
    inbox: Mutex<Inbox>,
    active: AtomicBool,
    core: OnceLock<Box<[Atom]>>,
    pub(super) state: Mutex<State>,
    /// Capacity publications: the activation writes `working`, the inbox's writers `queued`.
    working: AtomicUsize,
    queued: AtomicUsize,
}

#[derive(Default)]
struct Inbox {
    messages: Vec<Message>,
    /// The messages' payloads, where observed.
    payload_bytes: usize,
}

impl Inbox {
    fn bytes(&self) -> usize {
        super::memory::vec(&self.messages) + self.payload_bytes
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// An inbox publishes what it holds at its first message and every `QUEUED`th after it.
const QUEUED: usize = 16;

impl Context {
    fn push(&self, message: Message, observe: bool) {
        let mut inbox = lock(&self.inbox);
        if !observe {
            inbox.messages.push(message);
            return;
        }
        inbox.payload_bytes += message.heap_bytes();
        inbox.messages.push(message);
        if inbox.messages.len() % QUEUED == 1 {
            self.queued.store(inbox.bytes(), Relaxed);
        }
    }
}

/// A poisoned lock means a worker panicked; the scope re-raises that panic anyway.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

const BASE: usize = 256;
const SEGMENTS: usize = 26;

/// Contexts by id, in segments of `BASE`, `2·BASE`, `4·BASE`, …: appending never moves
/// a context, so readers need no lock.
struct Arena {
    segments: [OnceLock<Box<[Context]>>; SEGMENTS],
    len: AtomicUsize,
}

impl Arena {
    fn bytes(&self) -> usize {
        self.segments
            .iter()
            .filter_map(OnceLock::get)
            .map(|segment| std::mem::size_of_val(&**segment))
            .sum()
    }
    fn new() -> Self {
        Self {
            segments: std::array::from_fn(|_| OnceLock::new()),
            len: AtomicUsize::new(0),
        }
    }

    fn locate(id: usize) -> (usize, usize) {
        let m = id / BASE + 1;
        let segment = (usize::BITS - 1 - m.leading_zeros()) as usize;
        (segment, id - BASE * ((1 << segment) - 1))
    }

    fn get(&self, id: ContextId) -> &Context {
        let (segment, offset) = Self::locate(id as usize);
        &self.segments[segment]
            .get()
            .expect("a context id is handed out after its segment exists")[offset]
    }

    /// A new context's id (the caller holds the registry's lock).
    fn push(&self) -> Option<ContextId> {
        let id = self.len.load(SeqCst);
        let (segment, _) = Self::locate(id);
        if segment >= SEGMENTS || id >= u32::MAX as usize {
            return None;
        }
        self.segments[segment].get_or_init(|| {
            (0..BASE << segment)
                .map(|_| Context::default())
                .collect::<Vec<_>>()
                .into_boxed_slice()
        });
        self.len.store(id + 1, SeqCst);
        Some(id as ContextId)
    }
}

/// A context's key in the registry: its core and its tag ([`Engine::context_tagged`]).
type CoreKey = (Box<[Atom]>, u32);

/// Hashes like the owned tuple, so registry hits need no temporary allocation.
#[derive(Hash)]
struct CoreLookup<'a>(&'a [Atom], u32);

impl hashbrown::Equivalent<CoreKey> for CoreLookup<'_> {
    fn equivalent(&self, key: &CoreKey) -> bool {
        self.0 == &*key.0 && self.1 == key.1
    }
}

/// The program and the contexts saturating it.
pub struct Engine {
    pub program: Program,
    pub strategy: Strategy,
    /// Record each clause's derivation (for proofs).
    pub proofs: bool,
    contexts: Arena,
    registry: Mutex<HashMap<CoreKey, ContextId>>,
    budget: Budget,
    cancel: Option<crate::Cancel>,
    /// Pred joins stop where the body so far already makes the conclusion redundant.
    pub prune_pred: bool,
    /// The budget ran out: the workers drop what is left (the saturation is then
    /// incomplete and says so).
    exhausted: AtomicBool,
    /// Whether a budget check reads the clock (a deadline, a memory limit or an envelope).
    timed: bool,
    interval: Interval,
    /// The capacity envelope's observations (`None`: off).
    observer: Option<Observer>,
}

/// What a saturation may use (the dynamic fallback's budgets, design §4, in their first
/// form): past it, the run stops and is reported, never answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Budget {
    pub deadline: Option<Instant>,
    /// The most conclusions one join of the Pred rule may produce (its premises'
    /// combinations can grow as their product).
    pub max_join: Option<usize>,
    /// The most steps one Pred join may take: past them it is left and its context marked
    /// incomplete (the run goes on).
    pub max_join_steps: Option<usize>,
    /// The most memory the process may hold (bytes, `nrese_exec::memory`): a saturation
    /// that grows past it stops instead of taking the machine.
    pub max_memory: Option<u64>,
    /// The saturation's estimated capacity, sampled (docs/design/owl2-dl.md §13); `None`:
    /// unlimited.
    pub task_memory: Option<usize>,
    /// The request's memory envelope: the sampled capacity is reserved from it as it
    /// grows, and the saturation stops where it has no room, as past `task_memory`.
    /// Without either, nothing is observed.
    pub memory: Option<nrese_exec::Envelope>,
}

impl Engine {
    pub fn new(program: Program, strategy: Strategy, proofs: bool, budget: Budget) -> Self {
        let envelope = budget
            .memory
            .as_ref()
            .and_then(nrese_exec::Envelope::bounded);
        let observer = (budget.task_memory.is_some() || envelope.is_some())
            .then(|| Observer::new(budget.task_memory.unwrap_or(usize::MAX), envelope.cloned()));
        let engine = Self {
            program,
            strategy,
            proofs,
            contexts: Arena::new(),
            registry: Mutex::new(HashMap::new()),
            cancel: None,
            prune_pred: true,
            exhausted: AtomicBool::new(false),
            timed: budget.deadline.is_some() || budget.max_memory.is_some() || observer.is_some(),
            budget,
            interval: Interval::new(),
            observer,
        };
        if engine.observing() {
            engine.own(engine.program.bytes(), 0);
        }
        engine
    }

    /// The engine with Pred's pruning on or off (on by default; off only for A/B runs).
    pub fn with_prune_pred(mut self, on: bool) -> Self {
        self.prune_pred = on;
        self
    }

    pub(super) fn with_cancel(mut self, cancel: Option<crate::Cancel>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub(super) fn observing(&self) -> bool {
        self.observer.is_some()
    }

    /// Latest capacity estimate and sampled peak; zero when observations are off.
    pub fn task_memory_bytes(&self) -> (usize, usize) {
        self.observer
            .as_ref()
            .map_or((0, 0), |o| (o.used(), o.peak()))
    }

    /// Replaces `release` bytes the engine holds with `add` (exhausted past the envelope).
    fn own(&self, add: usize, release: usize) {
        if let Some(observer) = &self.observer
            && !observer.own(add, release)
        {
            self.exhaust();
        }
    }

    /// Updates an engine-held estimate `held` to `bytes`; false once the budget ran out.
    pub(super) fn hold(&self, held: &mut usize, bytes: usize) -> bool {
        self.own(bytes, std::mem::replace(held, bytes));
        !self.exhausted()
    }

    /// The latest sampling election ([`Interval::last`]).
    pub(super) fn election(&self) -> u64 {
        self.interval.last()
    }

    /// Publishes context `c`'s capacity (by the worker holding it).
    pub(super) fn publish(&self, c: ContextId, bytes: usize) {
        self.context(c).working.store(bytes, Relaxed);
    }

    /// Sums the contexts' publications, never taking their locks (exhausted past the
    /// envelope).
    pub(super) fn sample(&self) {
        let Some(observer) = &self.observer else {
            return;
        };
        let mut bytes = 0usize;
        for id in 0..self.count() as ContextId {
            let context = self.context(id);
            bytes = bytes
                .saturating_add(context.working.load(Relaxed))
                .saturating_add(context.queued.load(Relaxed));
        }
        if !observer.sample(bytes) {
            self.exhaust();
        }
    }

    /// Whether the budget ran out (then the saturation is incomplete).
    pub fn exhausted(&self) -> bool {
        self.exhausted.load(Relaxed)
    }

    /// Marks the budget spent.
    pub fn exhaust(&self) {
        self.exhausted.store(true, Relaxed);
    }

    /// Whether cancelled, the deadline passed or the memory limit or envelope is exceeded
    /// (marks the budget spent if so). One clock read; the process memory and the
    /// contexts' capacity are read once per interval, by whichever check it elects.
    pub(super) fn out_of_budget(&self) -> bool {
        if self.exhausted() {
            return true;
        }
        if self
            .cancel
            .as_ref()
            .is_some_and(crate::Cancel::is_cancelled)
        {
            self.exhaust();
            return true;
        }
        if !self.timed {
            return false;
        }
        let now = Instant::now();
        if self.budget.deadline.is_some_and(|d| now >= d) {
            self.exhaust();
            return true;
        }
        if self.interval.due(now) {
            if let Some(limit) = self.budget.max_memory
                && nrese_exec::memory::process_bytes().is_some_and(|held| held > limit)
            {
                self.exhaust();
            }
            self.sample();
        }
        self.exhausted()
    }

    pub fn count(&self) -> usize {
        self.contexts.len.load(SeqCst)
    }

    pub fn context(&self, id: ContextId) -> &Context {
        self.contexts.get(id)
    }

    pub fn core(&self, id: ContextId) -> &[Atom] {
        self.context(id).core.get().map_or(&[], |c| c)
    }

    /// The context with core `core` (sorted), and whether it was created now. The creator
    /// starts it ([`Message::Init`]).
    ///
    /// # Panics
    /// Past 2³² contexts or the arena's segments (some 17 billion contexts).
    pub fn context_for(&self, core: &[Atom]) -> (ContextId, bool) {
        self.context_tagged(core, 0)
    }

    /// The context with core `core` and tag `tag`: contexts with the same core and
    /// different tags are separate (Succ may pick any context whose core holds, so
    /// several with one core keep the calculus sound and complete; `Strategy::Split`).
    pub fn context_tagged(&self, core: &[Atom], tag: u32) -> (ContextId, bool) {
        let mut registry = lock(&self.registry);
        if let Some(&id) = registry.get(&CoreLookup(core, tag)) {
            return (id, false);
        }
        let before = self
            .observing()
            .then(|| self.contexts.bytes() + super::memory::map(&*registry));
        let id = self.contexts.push().expect("the context arena has room");
        let _ = self.context(id).core.set(core.into());
        registry.insert((core.into(), tag), id);
        if let Some(before) = before {
            // The arena's and the registry's growth, and the core's two copies.
            let after = self.contexts.bytes() + super::memory::map(&*registry);
            self.own(after - before + 2 * std::mem::size_of_val(core), 0);
        }
        (id, true)
    }

    /// Processes a context's messages until its inbox stays empty; messages for other
    /// contexts go out through `deliver` after each batch.
    fn work(&self, c: ContextId, deliver: &mut dyn FnMut(ContextId, Message)) {
        let context = self.context(c);
        let observe = self.observing();
        loop {
            let mut batch = {
                let mut inbox = lock(&context.inbox);
                if observe {
                    context.queued.store(0, Relaxed);
                }
                std::mem::take(&mut *inbox)
            };
            if !batch.is_empty() && self.out_of_budget() {
                // Out of budget: the messages are dropped, the run reports it.
                batch = Inbox::default();
            }
            if batch.is_empty() {
                context.active.store(false, SeqCst);
                // A message delivered between the take and the store found the context
                // still active and scheduled nothing: take it up here.
                if lock(&context.inbox).is_empty() || context.active.swap(true, SeqCst) {
                    return;
                }
                continue;
            }
            let mut out = Out::new(c);
            let state_bytes = {
                let mut state = lock(&context.state);
                let mut worker = Worker::new(self, &mut state, &mut out, batch.bytes());
                for (i, message) in batch.messages.into_iter().enumerate() {
                    worker.handle(message);
                    if (i + 1).is_multiple_of(256) && !worker.checkpoint(0) {
                        break;
                    }
                }
                worker.input_bytes = 0;
                worker.finish()
            };
            for (to, message) in out.sends {
                deliver(to, message);
            }
            if let Some(bytes) = state_bytes {
                context.working.store(bytes, Relaxed);
            }
        }
    }

    /// The contexts' states, after the run.
    pub fn states(&self) -> impl Iterator<Item = (ContextId, MutexGuard<'_, State>)> {
        (0..self.count() as ContextId).map(|id| (id, lock(&self.context(id).state)))
    }
}

#[cfg(test)]
mod tests;
