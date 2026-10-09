//! Context activation on reusable workers. Narrow allowances use nonblocking drains:
//! idle drains return their slot instead of occupying a Rayon worker while awaiting mail.

use nrese_exec::workers::Workers;

use super::{ContextId, Engine, Message, Mutex, SeqCst, lock};
use crate::context::memory;

impl Engine {
    /// Saturates with a standalone pool, falling back to the calling thread on failure.
    /// Repeated rounds should use [`Self::run_with_workers`] with one reusable owner.
    pub fn run(&self, seeds: &[ContextId], threads: usize) {
        let workers = Workers::new(threads.max(1)).unwrap_or_else(|_| Workers::serial());
        self.run_with_workers(seeds, &workers);
    }

    /// Starts `seeds` and saturates on an existing pool within its worker allowance.
    pub fn run_with_workers(&self, seeds: &[ContextId], workers: &Workers) {
        workers.install(|| self.run_on_workers(seeds, workers));
    }

    fn run_on_workers(&self, seeds: &[ContextId], workers: &Workers) {
        if workers.width() == 1 {
            let mut queue = Vec::new();
            let mut charge = self.memory_charge();
            for &seed in seeds {
                self.deliver_seq(&mut queue, seed, Message::Init);
            }
            charge.set(memory::vec(&queue));
            while let Some(context) = queue.pop() {
                self.work(context, &mut |to, message| {
                    self.deliver_seq(&mut queue, to, message);
                    charge.set(memory::vec(&queue));
                });
            }
            return;
        }
        if workers.width() == workers.pool_width() {
            rayon::scope(|scope| {
                for &seed in seeds {
                    self.deliver_par(scope, seed, Message::Init);
                }
            });
        } else {
            let agenda = Agenda {
                engine: self,
                width: workers.width(),
                ready: Mutex::new(Ready {
                    queue: Vec::new(),
                    drains: 0,
                    charge: self.memory_charge(),
                }),
            };
            rayon::scope(|scope| {
                for &seed in seeds {
                    agenda.deliver(scope, seed, Message::Init);
                }
            });
        }
    }

    pub(super) fn deliver_seq(&self, queue: &mut Vec<ContextId>, to: ContextId, message: Message) {
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
}

struct Ready {
    queue: Vec<ContextId>,
    drains: usize,
    charge: memory::Charge,
}

struct Agenda<'a> {
    engine: &'a Engine,
    width: usize,
    ready: Mutex<Ready>,
}

impl Agenda<'_> {
    fn deliver<'s>(&'s self, scope: &rayon::Scope<'s>, to: ContextId, message: Message) {
        let context = self.engine.context(to);
        lock(&context.inbox).push(message);
        if context.active.swap(true, SeqCst) {
            return;
        }
        let mut ready = lock(&self.ready);
        ready.queue.push(to);
        let capacity = memory::vec(&ready.queue);
        ready.charge.set(capacity);
        if ready.drains < self.width {
            ready.drains += 1;
            drop(ready);
            scope.spawn(move |scope| self.drain(scope));
        }
    }

    fn drain<'s>(&'s self, scope: &rayon::Scope<'s>) {
        loop {
            let next = {
                let mut ready = lock(&self.ready);
                match ready.queue.pop() {
                    Some(next) => next,
                    None => {
                        // Enqueue and release share the lock: a racing delivery either
                        // leaves work for a live drain or starts a new one, never loses it.
                        ready.drains -= 1;
                        return;
                    }
                }
            };
            self.engine
                .work(next, &mut |to, message| self.deliver(scope, to, message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        atoms::{Atom, CTerm},
        compile,
        engine::{Budget, Strategy},
    };
    use nrese_owl::{Axiom, ClassExpr, ExprId, Ontology};

    #[test]
    fn concurrent_delivery_restarts_retired_drains_and_releases_capacity() {
        let mut ontology = Ontology::default();
        let a = ExprId(ontology.classes.intern(ClassExpr::Class(1)));
        let b = ExprId(ontology.classes.intern(ClassExpr::Class(2)));
        ontology.axioms.push(Axiom::SubClassOf(a, b));
        let normalised = nrese_owl::normalise_with(&ontology, Default::default());
        let program = compile::compile(&normalised, &[1, 2], false)
            .unwrap()
            .program;
        let engine = Engine::new(program, Strategy::Cautious, false).with_budget(Budget {
            task_memory: Some(64 << 20),
            ..Budget::default()
        });
        let ledger = engine.task_memory.clone().unwrap();
        let concept = engine.program.names.iter().position(|&t| t == 1).unwrap() as u32;
        let roots: Vec<_> = (0..32)
            .map(|tag| {
                engine
                    .context_tagged(&[Atom::concept(concept, CTerm::X)], tag)
                    .0
            })
            .collect();
        let workers = Workers::pooled(4).unwrap();
        {
            let agenda = Agenda {
                engine: &engine,
                width: 2,
                ready: Mutex::new(Ready {
                    queue: Vec::new(),
                    drains: 0,
                    charge: engine.memory_charge(),
                }),
            };
            for round in 0..3 {
                workers.install(|| {
                    rayon::scope(|scope| {
                        for &root in &roots {
                            let agenda = &agenda;
                            scope.spawn(move |scope| {
                                agenda.deliver(scope, root, Message::Init);
                                assert!(lock(&agenda.ready).drains <= agenda.width);
                            });
                        }
                    })
                });
                assert_eq!(lock(&agenda.ready).drains, 0, "round {round}");
                assert!(lock(&agenda.ready).queue.is_empty());
                for &root in &roots {
                    let context = engine.context(root);
                    assert!(!context.active.load(SeqCst));
                    assert!(lock(&context.inbox).is_empty());
                    assert!(lock(&context.state).started);
                }
            }
        }
        assert!(!engine.exhausted());
        drop(engine);
        assert!(ledger.peak() > 0);
        assert_eq!(ledger.used(), 0);
    }
}
