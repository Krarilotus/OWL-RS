//! Guard of a key's scratch (`tableau::keys`; PR #48's review, F1): the pairs a key tries
//! are enumerated, not stored, so evaluating it holds memory linear in its members, not in
//! their pairs. Measured with a counting allocator, so this file is a test binary of its own.

use nrese_dl::tableau::{Answer, Config, consistency};
use nrese_exec::heap;
use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// `HasKey(A (R) ())` with R transitive, and `members` A-individuals with R to one `z`, the
/// first two of them different: every two members share `z`'s reach name, and the key makes
/// the first two equal.
fn shared_target(members: u64) -> Ontology {
    let mut o = Ontology::default();
    let add = |o: &mut Ontology, axiom: Axiom| {
        o.axioms.push(axiom);
        o.sources.push(Vec::new());
    };
    let a = ExprId(o.classes.intern(ClassExpr::Class(1)));
    let r = ObjProp::Named(2);
    add(
        &mut o,
        Axiom::ObjectCharacteristic(Characteristic::Transitive, r),
    );
    add(&mut o, Axiom::HasKey(a, vec![r], Vec::new()));
    for x in 100..100 + members {
        add(&mut o, Axiom::ClassAssertion(a, x));
        add(&mut o, Axiom::ObjectPropertyAssertion(2, x, 10));
    }
    add(&mut o, Axiom::DifferentIndividuals(vec![100, 101]));
    o
}

/// The most one evaluation of the key held at once (its step of the heap profile), over the
/// consistency check. The check as a whole holds more, quadratic in the members here, so its
/// peak can't tell the key's part apart.
fn key_scratch(members: u64) -> usize {
    let ontology = shared_target(members);
    let config = Config {
        portfolio: false,
        ..Config::default()
    };
    heap::start("consistency");
    let answer = consistency(&ontology, &config).answer;
    let steps = heap::steps();
    let _ = heap::finish();
    assert_eq!(
        answer,
        Answer::Inconsistent,
        "{members} members, the first two different"
    );
    steps
        .iter()
        .filter(|step| step.label == "dl: a key evaluated")
        .map(|step| step.peak)
        .max()
        .expect("the key is evaluated")
}

#[test]
fn a_key_holds_memory_linear_in_its_members_not_in_their_pairs() {
    let small = key_scratch(100);
    let large = key_scratch(400);
    // Four times the members may hold four times the memory, twice that for slack; their
    // pairs are 16 times as many.
    assert!(
        large <= small * 8,
        "the key held {small} bytes at most for 100 members, {large} for 400"
    );
}
