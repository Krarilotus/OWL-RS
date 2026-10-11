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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use hashbrown::HashMap;

use super::atoms::Atom;
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
    // Distinct writers: the activation owns working, inbox writers own queued.
    working: AtomicUsize,
    queued: AtomicUsize,
}

#[derive(Default)]
struct Inbox {
    messages: Vec<Message>,
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

impl Context {
    fn push(&self, message: Message, observe: bool) {
        let mut inbox = lock(&self.inbox);
        inbox.payload_bytes += message.heap_bytes();
        inbox.messages.push(message);
        if observe {
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

/// The program and the contexts saturating it.
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

pub struct Engine {
    pub program: Program,
    pub strategy: Strategy,
    /// Record each clause's derivation (for proofs).
    pub proofs: bool,
    contexts: Arena,
    registry: Mutex<HashMap<CoreKey, ContextId>>,
    budget: Budget,
    cancel: Option<crate::tableau::Cancel>,
    /// Pred joins stop where the body so far already makes the conclusion redundant.
    pub prune_pred: bool,
    /// The budget ran out: the workers drop what is left (the saturation is then
    /// incomplete and says so).
    exhausted: Arc<AtomicBool>,
    memory: Option<nrese_exec::memory::MemoryWatch>,
    task_memory: Option<std::sync::Arc<super::memory::Task>>,
    owned_memory: Mutex<super::memory::Charge>,
    /// Program, arena segments, registry table and the two copies of context cores.
    owned_bytes: AtomicUsize,
}

/// What a saturation may use (the dynamic fallback's budgets, design §4, in their first
/// form): past it, the run stops and is reported, never answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// Estimated saturation capacity; `None` is unlimited and disables observations.
    /// Contexts publish at work checkpoints; their sampled sum may lag or overlap at
    /// handoffs. This is not an allocation ceiling. Compilation, short-lived recursive
    /// temporaries, allocator metadata and worker stacks are outside the observation.
    pub task_memory: Option<usize>,
}

impl Engine {
    pub fn new(program: Program, strategy: Strategy, proofs: bool) -> Self {
        Self {
            program,
            strategy,
            proofs,
            contexts: Arena::new(),
            registry: Mutex::new(HashMap::new()),
            budget: Budget::default(),
            cancel: None,
            prune_pred: true,
            exhausted: Arc::new(AtomicBool::new(false)),
            memory: None,
            task_memory: None,
            owned_memory: Mutex::new(Default::default()),
            owned_bytes: AtomicUsize::new(0),
        }
    }

    /// The engine with Pred's pruning on or off (on by default; off only for A/B runs).
    pub fn with_prune_pred(mut self, on: bool) -> Self {
        self.prune_pred = on;
        self
    }

    /// The engine with a budget. Changing it does not revive an exhausted run:
    /// interrupted saturation may already have discarded unfinished work.
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self.memory = budget.max_memory.map(nrese_exec::memory::MemoryWatch::new);
        self.task_memory = budget
            .task_memory
            .map(|limit| super::memory::Task::new(limit, Arc::clone(&self.exhausted)));
        if self.task_memory.is_some() {
            let bytes = self.program.bytes();
            let mut charge = self.memory_charge();
            charge.set(bytes);
            self.owned_bytes.store(bytes, Relaxed);
            self.owned_memory = Mutex::new(charge);
        }
        self
    }

    pub(super) fn memory_charge(&self) -> super::memory::Charge {
        super::memory::Charge::new(self.task_memory.as_ref())
    }

    /// Latest capacity estimate and sampled peak; zero when observations are disabled.
    pub fn task_memory_bytes(&self) -> (usize, usize) {
        self.task_memory
            .as_ref()
            .map_or((0, 0), |m| (m.used(), m.peak()))
    }

    pub(super) fn observe_context(&self, id: ContextId, bytes: usize) {
        self.context(id).working.store(bytes, Relaxed);
        self.sample_memory(false);
    }

    /// Reads only publications, never another context's locks. The final sample
    /// observes remaining capacity even when the timer gate skipped a short run;
    /// transient peaks may already have disappeared.
    pub(super) fn sample_memory(&self, force: bool) {
        let Some(task) = &self.task_memory else {
            return;
        };
        if !task.sample_due(force) {
            return;
        }
        let mut bytes = 0usize;
        for id in 0..self.count() as ContextId {
            let context = self.context(id);
            bytes = bytes
                .saturating_add(context.working.load(Relaxed))
                .saturating_add(context.queued.load(Relaxed));
        }
        task.observe(bytes);
    }

    pub fn budget(&self) -> Budget {
        self.budget
    }

    pub(super) fn with_cancel(mut self, cancel: Option<crate::tableau::Cancel>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Whether the budget ran out (then the saturation is incomplete).
    pub fn exhausted(&self) -> bool {
        self.exhausted.load(Relaxed)
    }

    /// Marks the budget spent.
    pub fn exhaust(&self) {
        self.exhausted.store(true, Relaxed);
    }

    /// Whether cancelled, the deadline passed or the memory limit is exceeded (marks
    /// the budget spent if so).
    pub(super) fn out_of_budget(&self) -> bool {
        if self.exhausted() {
            return true;
        }
        if self.budget.deadline.is_some_and(|d| Instant::now() >= d)
            || self
                .cancel
                .as_ref()
                .is_some_and(crate::tableau::Cancel::is_cancelled)
            || self.memory.as_ref().is_some_and(|m| m.exceeded())
        {
            self.exhaust();
            return true;
        }
        false
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
            .task_memory
            .as_ref()
            .map(|_| self.contexts.bytes() + super::memory::map(&*registry));
        let id = self.contexts.push().expect("the context arena has room");
        let _ = self.context(id).core.set(core.into());
        registry.insert((core.into(), tag), id);
        if let Some(before) = before {
            let more = self.contexts.bytes() + super::memory::map(&*registry) - before
                + 2 * std::mem::size_of_val(core);
            let bytes = self.owned_bytes.fetch_add(more, Relaxed) + more;
            lock(&self.owned_memory).set(bytes);
            let mut state = lock(&self.context(id).state);
            state.clauses.observe_memory = true;
        }
        (id, true)
    }

    /// Processes a context's messages until its inbox stays empty; messages for other
    /// contexts go out through `deliver` after each batch.
    fn work(&self, c: ContextId, deliver: &mut dyn FnMut(ContextId, Message)) {
        let context = self.context(c);
        let observe = self.task_memory.is_some();
        loop {
            let mut batch = {
                let mut inbox = lock(&context.inbox);
                let batch = std::mem::take(&mut *inbox);
                if observe {
                    context.queued.store(0, Relaxed);
                }
                batch
            };
            if !batch.is_empty() && self.out_of_budget() {
                // Incomplete runs discard remaining messages, never report an answer.
                batch.messages.clear();
                batch.payload_bytes = 0;
            }
            if batch.is_empty() {
                // No old activation may publish after a new worker acquires it.
                context.active.store(false, SeqCst);
                if lock(&context.inbox).is_empty() || context.active.swap(true, SeqCst) {
                    return;
                }
                continue;
            }
            let mut out = Out::new(c);
            let state_bytes;
            {
                let mut state = lock(&context.state);
                let mut worker = Worker {
                    engine: self,
                    state: &mut state,
                    out: &mut out,
                    scratch: super::rules::Scratch::default(),
                    input_bytes: batch.bytes(),
                };
                for (i, message) in batch.messages.into_iter().enumerate() {
                    worker.input_bytes -= message.heap_bytes();
                    worker.handle(message);
                    if self.exhausted() || (i.is_multiple_of(256) && !worker.checkpoint()) {
                        break;
                    }
                }
                // IntoIter has now released its backing allocation and any leftovers.
                worker.input_bytes = 0;
                worker.checkpoint();
                state_bytes = if observe { worker.state.bytes() } else { 0 };
            }
            for (to, message) in out.sends {
                if self.exhausted() {
                    break;
                }
                deliver(to, message);
            }
            if observe {
                // Scratch, detached input and outgoing payloads have left this owner.
                context.working.store(state_bytes, Relaxed);
            }
        }
    }

    /// The contexts' states, after the run.
    pub fn states(&self) -> impl Iterator<Item = (ContextId, MutexGuard<'_, State>)> {
        (0..self.count() as ContextId).map(|id| (id, lock(&self.context(id).state)))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(task) = &self.task_memory {
            task.observe(0);
        }
    }
}

#[cfg(test)]
mod tests;
