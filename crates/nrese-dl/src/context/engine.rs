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
}

#[derive(Default)]
struct Inbox {
    messages: Vec<Message>,
    payload_bytes: usize,
    memory: super::memory::Charge,
}

impl Inbox {
    fn push(&mut self, message: Message) {
        self.payload_bytes += message.heap_bytes();
        self.messages.push(message);
        self.memory
            .set(super::memory::vec(&self.messages) + self.payload_bytes);
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty()
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
    exhausted: AtomicBool,
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
    /// Task-owned saturation capacity; `None` is unlimited and disables accounting.
    /// Checked at growth/work boundaries, not a strict allocation ceiling. Compilation
    /// temporaries, allocator metadata and worker stacks are outside this capacity limit.
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
            exhausted: AtomicBool::new(false),
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

    /// The engine with a budget.
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self.memory = budget.max_memory.map(nrese_exec::memory::MemoryWatch::new);
        self.task_memory = budget.task_memory.map(super::memory::Task::new);
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

    pub(super) fn task_memory_exhausted(&self) -> bool {
        self.task_memory.as_ref().is_some_and(|m| m.exhausted())
    }

    /// Accounted task capacity and peak; zero when task accounting is disabled.
    pub fn task_memory_bytes(&self) -> (usize, usize) {
        self.task_memory
            .as_ref()
            .map_or((0, 0), |m| (m.used(), m.peak()))
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
        self.exhausted.load(Relaxed) || self.task_memory_exhausted()
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
        let key = (core.into(), tag);
        if let Some(&id) = registry.get(&key) {
            return (id, false);
        }
        let before = self
            .task_memory
            .as_ref()
            .map(|_| self.contexts.bytes() + super::memory::map(&*registry));
        let id = self.contexts.push().expect("the context arena has room");
        let _ = self.context(id).core.set(core.into());
        registry.insert(key, id);
        if let Some(before) = before {
            let more = self.contexts.bytes() + super::memory::map(&*registry) - before
                + 2 * std::mem::size_of_val(core);
            let bytes = self.owned_bytes.fetch_add(more, Relaxed) + more;
            lock(&self.owned_memory).set(bytes);
            let mut state = lock(&self.context(id).state);
            state.memory = self.memory_charge();
            state.clauses.memory = self.memory_charge();
            lock(&self.context(id).inbox).memory = self.memory_charge();
        }
        (id, true)
    }

    /// Processes a context's messages until its inbox stays empty; messages for other
    /// contexts go out through `deliver` after each batch.
    fn work(&self, c: ContextId, deliver: &mut dyn FnMut(ContextId, Message)) {
        let context = self.context(c);
        loop {
            let mut batch = std::mem::replace(
                &mut *lock(&context.inbox),
                Inbox {
                    memory: self.memory_charge(),
                    ..Inbox::default()
                },
            );
            if !batch.is_empty() && self.out_of_budget() {
                // Out of budget: the messages are dropped, the run reports it.
                batch.messages.clear();
                batch.payload_bytes = 0;
                batch.memory.set(super::memory::vec(&batch.messages));
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
            out.memory = self.memory_charge();
            {
                let mut state = lock(&context.state);
                let mut worker = Worker {
                    engine: self,
                    state: &mut state,
                    out: &mut out,
                    scratch: super::rules::Scratch::new(self),
                };
                let capacity = super::memory::vec(&batch.messages);
                for message in batch.messages {
                    batch.payload_bytes -= message.heap_bytes();
                    batch.memory.set(capacity + batch.payload_bytes);
                    worker.handle(message);
                }
            }
            let capacity = super::memory::vec(&out.sends) + super::memory::vec(&out.local);
            for (to, message) in out.sends {
                out.payload_bytes -= message.heap_bytes();
                out.memory.set(capacity + out.payload_bytes);
                deliver(to, message);
            }
        }
    }

    /// The contexts' states, after the run.
    pub fn states(&self) -> impl Iterator<Item = (ContextId, MutexGuard<'_, State>)> {
        (0..self.count() as ContextId).map(|id| (id, lock(&self.context(id).state)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_stops_nested_context_work_after_messages_have_run() {
        for task_memory in [None, Some(64 * 1024 * 1024)] {
            cancellation_releases_task(task_memory);
        }
    }

    fn cancellation_releases_task(task_memory: Option<usize>) {
        use nrese_owl::{Axiom, ClassExpr, ExprId, ObjProp, Ontology};
        use std::sync::Arc;

        // A -> exists r.B starts a successor while processing A's context.
        let mut ontology = Ontology::default();
        let a = ExprId(ontology.classes.intern(ClassExpr::Class(1)));
        let b = ExprId(ontology.classes.intern(ClassExpr::Class(2)));
        let some = ExprId(
            ontology
                .classes
                .intern(ClassExpr::Some(ObjProp::Named(3), b)),
        );
        ontology.axioms.push(Axiom::SubClassOf(a, some));
        let normalised = nrese_owl::normalise_with(&ontology, Default::default());
        let compiled = super::super::compile::compile(&normalised, &[1, 2], false).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = crate::tableau::Cancel::from_flag(Arc::clone(&flag));
        let engine = Engine::new(compiled.program, Strategy::Cautious, false)
            .with_budget(Budget {
                task_memory,
                ..Budget::default()
            })
            .with_cancel(Some(cancel));
        let ledger = engine.task_memory.clone();
        let concept = engine.program.names.iter().position(|&t| t == 1).unwrap() as u32;
        let (root, _) =
            engine.context_for(&[Atom::concept(concept, super::super::atoms::CTerm::X)]);
        let mut queue = Vec::new();
        engine.deliver_seq(&mut queue, root, Message::Init);
        let mut delivered = 0;
        while let Some(context) = queue.pop() {
            // The scheduler's existing delivery callback is a deterministic checkpoint:
            // cancellation fires only after actual rule work has emitted a message.
            engine.work(context, &mut |to, message| {
                delivered += 1;
                assert!(
                    engine
                        .states()
                        .any(|(_, s)| s.clauses.counters.messages > 0)
                );
                flag.store(true, std::sync::atomic::Ordering::Release);
                engine.deliver_seq(&mut queue, to, message);
            });
        }
        assert!(delivered > 0, "the test must cancel running nested work");
        assert!(
            engine.exhausted(),
            "cancellation must reach a budget checkpoint"
        );
        assert!(engine.states().any(|(_, s)| !s.started));
        drop(engine);
        if let Some(ledger) = ledger {
            assert!(ledger.peak() > 0);
            assert_eq!(ledger.used(), 0, "cancelled tasks release every owner");
        }
    }

    #[test]
    fn queued_payload_moves_with_its_capacity_charge() {
        use super::super::{
            memory::{Charge, Task},
            state::ClauseRef,
        };
        let task = Task::new(1024 * 1024);
        let mut inbox = Inbox {
            memory: Charge::new(Some(&task)),
            ..Inbox::default()
        };
        let body = vec![Atom::BOTTOM; 100].into_boxed_slice();
        let payload = std::mem::size_of_val(&*body);
        inbox.push(Message::Pred {
            from: ClauseRef {
                context: 0,
                clause: 0,
            },
            func: 0,
            body,
            head: Atom::BOTTOM,
        });
        let capacity = super::super::memory::vec(&inbox.messages);
        assert_eq!(task.used(), capacity + payload);
        let batch = std::mem::take(&mut inbox);
        assert_eq!(task.used(), capacity + payload);
        drop(inbox);
        assert_eq!(task.used(), capacity + payload);
        drop(batch);
        assert_eq!(task.used(), 0);
    }

    #[test]
    fn the_arena_locates_ids_in_growing_segments() {
        assert_eq!(Arena::locate(0), (0, 0));
        assert_eq!(Arena::locate(BASE - 1), (0, BASE - 1));
        assert_eq!(Arena::locate(BASE), (1, 0));
        assert_eq!(Arena::locate(3 * BASE - 1), (1, 2 * BASE - 1));
        assert_eq!(Arena::locate(3 * BASE), (2, 0));
        let arena = Arena::new();
        for i in 0..(BASE * 3 + 5) as u32 {
            assert_eq!(arena.push(), Some(i));
        }
        let _ = arena.get(BASE as u32 * 3 + 4);
    }
}
