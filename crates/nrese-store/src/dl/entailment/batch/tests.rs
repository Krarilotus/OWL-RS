use super::*;
use crate::dl::entailment::{entails, nonempty};
use std::time::Duration;

fn budget() -> Budget {
    Budget {
        workers: nrese_exec::workers::Workers::serial(),
        timeout: Some(Duration::from_secs(10)),
        memory_bytes: None,
        memory: None,
        cancel: None,
        max_nodes: Some(10_000),
        max_branch_points: Some(10_000),
        max_merges: None,
        max_join: None,
        explanation_tests: None,
    }
}

#[test]
fn one_program_matches_independent_reductions_without_leaking_assumptions() {
    let mut o = Ontology::default();
    o.builtin.top_object = Some(900);
    let a = intern(&mut o, ClassExpr::Class(1));
    let b = intern(&mut o, ClassExpr::Class(2));
    let c = intern(&mut o, ClassExpr::Class(3));
    let union = intern(&mut o, ClassExpr::Or(vec![b, c]));
    o.axioms = vec![
        Axiom::SubClassOf(a, union),
        Axiom::ClassAssertion(a, 10),
        Axiom::ObjectPropertyAssertion(20, 10, 11),
        Axiom::SameIndividual(vec![11, 12]),
    ];
    let tests = vec![
        Test::Axiom(Axiom::ClassAssertion(a, 10)),
        Test::Axiom(Axiom::ClassAssertion(b, 10)),
        Test::Axiom(Axiom::ClassAssertion(union, 10)),
        Test::Axiom(Axiom::ObjectPropertyAssertion(20, 10, 12)),
        Test::Axiom(Axiom::ObjectPropertyAssertion(20, 12, 10)),
        Test::Axiom(Axiom::SameIndividual(vec![11, 12])),
        Test::Axiom(Axiom::DifferentIndividuals(vec![11, 12])),
        Test::Nonempty(a),
        Test::Nonempty(b),
        Test::Nonempty(union),
    ];
    let original = o.axioms.clone();
    let expected: Vec<_> = tests
        .iter()
        .map(|test| match test {
            Test::Axiom(a) => entails(&o, a, &[100, 101, 102], &budget()),
            Test::Nonempty(c) => nonempty(&o, *c, &budget()),
        })
        .collect();
    COMPILATIONS.set(0);
    let compiled = Batch::new(&mut o, &tests, None);
    assert!(
        compiled.prepared.is_some(),
        "one compiled input for the whole batch"
    );
    assert!(
        compiled.probes.iter().all(Option::is_some),
        "every form is probed, not reduced"
    );
    assert_eq!(o.axioms, original, "selectors never enter the premise");
    for i in (0..tests.len())
        .chain((0..tests.len()).rev())
        .chain(0..tests.len())
    {
        assert_eq!(
            compiled.check(i, &tests[i], &o, &[100, 101, 102], &budget()),
            expected[i],
            "test {i}"
        );
    }
    assert_eq!(COMPILATIONS.get(), 1, "probes reuse one compilation");
}

#[test]
fn cancellation_and_fresh_class_name_collisions_stay_local() {
    let mut o = Ontology::default();
    let c = intern(&mut o, ClassExpr::Class(u64::MAX));
    o.axioms.push(Axiom::ClassAssertion(c, 1));
    let tests = [
        Test::Axiom(Axiom::ClassAssertion(c, 1)),
        Test::Axiom(Axiom::ClassAssertion(c, 2)),
    ];
    let compiled = Batch::new(&mut o, &tests, None);
    let token = nrese_sparql::CancellationToken::new();
    let mut cancelled = budget();
    cancelled.cancel = Some(tableau::Cancel::from_flag(token.flag()));
    token.cancel();
    let check = |i: usize, budget: &Budget| compiled.check(i, &tests[i], &o, &[], budget);
    assert!(matches!(check(0, &cancelled), Entailed::Unknown(_)));
    assert_eq!(check(0, &budget()), Entailed::Yes);
    assert_eq!(check(1, &budget()), Entailed::No);
}

#[test]
fn transitive_role_values_use_the_existing_non_simple_role_normalisation() {
    let mut o = Ontology {
        axioms: vec![
            Axiom::ObjectCharacteristic(nrese_owl::Characteristic::Transitive, ObjProp::Named(7)),
            Axiom::ObjectPropertyAssertion(7, 1, 2),
            Axiom::ObjectPropertyAssertion(7, 2, 3),
        ],
        ..Ontology::default()
    };
    let tests = [
        Test::Axiom(Axiom::ObjectPropertyAssertion(7, 1, 3)),
        Test::Axiom(Axiom::ObjectPropertyAssertion(7, 3, 1)),
    ];
    let compiled = Batch::new(&mut o, &tests, None);
    assert_eq!(
        compiled.check(0, &tests[0], &o, &[], &budget()),
        Entailed::Yes
    );
    assert_eq!(
        compiled.check(1, &tests[1], &o, &[], &budget()),
        Entailed::No
    );
}
