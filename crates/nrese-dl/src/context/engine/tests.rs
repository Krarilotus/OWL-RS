use super::super::{atoms::CTerm, state::ClauseRef};
use super::*;

fn payload() -> Message {
    Message::Pred {
        from: ClauseRef {
            context: 0,
            clause: 0,
        },
        func: 0,
        body: vec![Atom::concept(0, CTerm::X)].into_boxed_slice(),
        head: Atom::BOTTOM,
    }
}

fn payload_engine() -> Engine {
    let normalised = nrese_owl::normalise_with(&nrese_owl::Ontology::default(), Default::default());
    let program = super::super::compile::compile(&normalised, &[1], false)
        .unwrap()
        .program;
    let budget = Budget {
        task_memory: Some(64 << 20),
        ..Budget::default()
    };
    Engine::new(program, Strategy::Cautious, false, budget)
}

#[test]
fn the_envelope_stops_its_engine_alone_and_for_good() {
    let engine = payload_engine();
    let other = payload_engine();
    let mut held = 0;
    assert!(engine.hold(&mut held, 20));
    assert!(!engine.hold(&mut held, 64 << 20));
    assert!(engine.exhausted() && engine.out_of_budget());
    assert!(
        !engine.hold(&mut held, 0),
        "released capacity restores no work"
    );
    assert_eq!(engine.task_memory_bytes().0, other.task_memory_bytes().0);
    assert!(engine.task_memory_bytes().1 > 64 << 20);
    assert!(!other.exhausted());
}

#[test]
fn borrowed_context_keys_reuse_only_equal_cores_and_tags() {
    let engine = payload_engine();
    let a = Atom::concept(0, CTerm::X);
    let b = Atom::concept(1, CTerm::X);
    let cores: &[&[Atom]] = &[&[], &[a], &[a, b]];
    for (index, &core) in cores.iter().enumerate() {
        for tag in 0..2 {
            assert_eq!(
                engine.context_tagged(core, tag),
                (index as u32 * 2 + tag, true)
            );
        }
    }
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let engine = &engine;
            scope.spawn(move || {
                for (index, &core) in cores.iter().enumerate() {
                    for tag in 0..2 {
                        assert_eq!(
                            engine.context_tagged(core, tag),
                            (index as u32 * 2 + tag, false)
                        );
                    }
                }
            });
        }
    });
    assert_eq!(engine.count(), 6);
}

#[test]
fn queued_payloads_are_visible_before_the_receiver_runs() {
    let engine = payload_engine();
    let (root, _) = engine.context_for(&[]);
    engine.sample();
    let base = engine.task_memory_bytes().0;
    let mut queue = Vec::new();
    for _ in 0..=QUEUED {
        engine.deliver_seq(&mut queue, root, payload());
    }
    engine.sample();
    let inbox = lock(&engine.context(root).inbox);
    assert_eq!(engine.task_memory_bytes().0, base + inbox.bytes());
    assert_eq!(inbox.payload_bytes, (QUEUED + 1) * payload().heap_bytes());
    assert_eq!(queue, [root], "one activation for many messages");
}

#[test]
fn cancellation_stops_nested_context_work_after_messages_have_run() {
    for task_memory in [None, Some(64 * 1024 * 1024)] {
        cancellation_drops_queued_work(task_memory);
    }
}

fn cancellation_drops_queued_work(task_memory: Option<usize>) {
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
    let cancel = crate::Cancel::from_flag(Arc::clone(&flag));
    let budget = Budget {
        task_memory,
        ..Budget::default()
    };
    let engine =
        Engine::new(compiled.program, Strategy::Cautious, false, budget).with_cancel(Some(cancel));
    let concept = engine.program.names.iter().position(|&t| t == 1).unwrap() as u32;
    let (root, _) = engine.context_for(&[Atom::concept(concept, super::super::atoms::CTerm::X)]);
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
    for id in 0..engine.count() as ContextId {
        assert!(lock(&engine.context(id).inbox).is_empty());
        assert_eq!(engine.context(id).queued.load(Relaxed), 0);
    }
    assert_eq!(engine.task_memory_bytes().1 > 0, task_memory.is_some());
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
