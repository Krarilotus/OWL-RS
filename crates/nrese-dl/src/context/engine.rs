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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Mutex, MutexGuard, OnceLock};

use hashbrown::HashMap;

use super::atoms::Atom;
use super::program::Program;
use super::rules::{Message, Out, State, Worker};
use super::state::ContextId;

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
}

/// A context: its inbox, activation flag, core and state.
#[derive(Default)]
pub struct Context {
    inbox: Mutex<Vec<Message>>,
    active: AtomicBool,
    core: OnceLock<Box<[Atom]>>,
    pub(super) state: Mutex<State>,
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
pub struct Engine {
    pub program: Program,
    pub strategy: Strategy,
    /// Record each clause's derivation (for proofs).
    pub proofs: bool,
    contexts: Arena,
    registry: Mutex<HashMap<Box<[Atom]>, ContextId>>,
}

impl Engine {
    pub fn new(program: Program, strategy: Strategy, proofs: bool) -> Self {
        Self {
            program,
            strategy,
            proofs,
            contexts: Arena::new(),
            registry: Mutex::new(HashMap::new()),
        }
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
        let mut registry = lock(&self.registry);
        if let Some(&id) = registry.get(core) {
            return (id, false);
        }
        let id = self.contexts.push().expect("the context arena has room");
        let _ = self.context(id).core.set(core.into());
        registry.insert(core.into(), id);
        (id, true)
    }

    /// Starts `seeds` and saturates until no context has work, on `threads` workers.
    pub fn run(&self, seeds: &[ContextId], threads: usize) {
        if threads <= 1 {
            let mut queue = Vec::new();
            for &s in seeds {
                self.deliver_seq(&mut queue, s, Message::Init);
            }
            while let Some(c) = queue.pop() {
                let mut next = Vec::new();
                self.work(c, &mut |to, m| self.deliver_seq(&mut next, to, m));
                queue.extend(next);
            }
            return;
        }
        let run = || {
            rayon::scope(|scope| {
                for &s in seeds {
                    self.deliver_par(scope, s, Message::Init);
                }
            });
        };
        match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
            Ok(pool) => pool.install(run),
            Err(_) => run(),
        }
    }

    fn deliver_seq(&self, queue: &mut Vec<ContextId>, to: ContextId, message: Message) {
        let context = self.context(to);
        lock(&context.inbox).push(message);
        if !context.active.swap(true, SeqCst) {
            queue.push(to);
        }
    }

    fn deliver_par<'s>(&'s self, scope: &rayon::Scope<'s>, to: ContextId, message: Message) {
        let context = self.context(to);
        lock(&context.inbox).push(message);
        if !context.active.swap(true, SeqCst) {
            scope.spawn(move |scope| self.work(to, &mut |t, m| self.deliver_par(scope, t, m)));
        }
    }

    /// Processes a context's messages until its inbox stays empty; messages for other
    /// contexts go out through `deliver` after each batch.
    fn work(&self, c: ContextId, deliver: &mut dyn FnMut(ContextId, Message)) {
        let context = self.context(c);
        loop {
            let batch = std::mem::take(&mut *lock(&context.inbox));
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
            {
                let mut state = lock(&context.state);
                let mut worker = Worker {
                    engine: self,
                    state: &mut state,
                    out: &mut out,
                };
                for message in batch {
                    worker.handle(message);
                }
            }
            for (to, message) in out.sends {
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
