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

#[test]
fn reductions_keep_one_deadline_and_no_dominates_unknown() {
    use super::super::entails_with;
    use consistency::Verdict;

    let mut o = Ontology::default();
    let classes = (1..=3)
        .map(|c| intern(&mut o, ClassExpr::Class(c)))
        .collect();
    let axiom = Axiom::EquivalentClasses(classes);
    let budget = budget();
    let deadline = Deadline::after(Some(Duration::from_secs(60)));
    assert_eq!(
        Entailed::No.and(Entailed::Unknown("later stop".into())),
        Entailed::No
    );
    for negative in [false, true] {
        let mut calls = 0;
        let answer = entails_with(
            &o,
            &axiom,
            &[100, 101, 102],
            &budget,
            deadline,
            |_, b, d| {
                calls += 1;
                assert_eq!(d, deadline, "every reduction inherits the exact instant");
                let config = consistency::tableau_config(b, d);
                assert_eq!(config.deadline, deadline.instant());
                assert_eq!(config.timeout, None, "no relative timeout to restart");
                if calls == 1 {
                    Verdict::Unknown("unsupported first reduction".into())
                } else if negative {
                    Verdict::Consistent
                } else {
                    Verdict::Inconsistent
                }
            },
        );
        if negative {
            assert_eq!(answer, Entailed::No);
            assert_eq!(calls, 2, "a countermodel ends the conjunction");
        } else {
            assert_eq!(
                answer,
                Entailed::Unknown("unsupported first reduction".into())
            );
            assert_eq!(calls, 6, "unknown alone does not skip later reductions");
        }
    }
}

#[test]
fn cancellation_between_reductions_stops_the_same_operation() {
    let mut o = Ontology::default();
    let a = intern(&mut o, ClassExpr::Class(1));
    let b = intern(&mut o, ClassExpr::Class(2));
    let cancel = tableau::Cancel::default();
    let budget = Budget {
        cancel: Some(cancel.clone()),
        ..budget()
    };
    let mut calls = 0;
    let answer = super::super::entails_with(
        &o,
        &Axiom::EquivalentClasses(vec![a, b]),
        &[100, 101, 102],
        &budget,
        Deadline::NONE,
        |_, b, d| {
            calls += 1;
            assert_eq!(d, Deadline::NONE);
            cancel.cancel();
            assert!(b.cancel.as_ref().unwrap().is_cancelled());
            consistency::Verdict::Inconsistent
        },
    );
    assert_eq!(calls, 1);
    assert_eq!(answer, Entailed::Unknown("cancelled".into()));
}

#[test]
fn stopped_absolute_deadlines_cover_compiled_scalar_and_nonempty_paths() {
    use super::super::{entails_ontology_until, entails_until, nonempty_until};
    use nrese_dl::limits::Limit;
    use std::time::Instant;

    let mut o = Ontology::default();
    let a = intern(&mut o, ClassExpr::Class(1));
    let b = intern(&mut o, ClassExpr::Class(2));
    let tests = [
        Test::Axiom(Axiom::ClassAssertion(a, 1)),
        Test::Axiom(Axiom::EquivalentClasses(vec![a, b])),
        Test::Nonempty(a),
    ];
    let compiled = Batch::new(&mut o, &tests, None);
    assert!(compiled.probes[0].is_some());
    assert!(compiled.probes[1..].iter().all(Option::is_none));
    let expired = Deadline::at(Instant::now());
    let cancel = tableau::Cancel::default();
    cancel.cancel();
    for (budget, deadline, cancelled) in [
        (budget(), expired, false),
        (
            Budget {
                timeout: Some(Duration::ZERO),
                ..budget()
            },
            Deadline::NONE,
            false,
        ),
        (
            Budget {
                cancel: Some(cancel),
                ..budget()
            },
            Deadline::NONE,
            true,
        ),
    ] {
        let assert_unknown = |answer| {
            let Entailed::Unknown(why) = answer else {
                panic!("stopped work decided: {answer:?}")
            };
            if cancelled {
                assert_eq!(why, "cancelled");
            } else {
                assert_eq!(Limit::named_in(&why), Some(Limit::Time));
            }
        };
        for (i, test) in tests.iter().enumerate() {
            assert_unknown(
                compiled.check_with(i, test, &o, &[], &budget, deadline, |_, _, _| {
                    panic!("a stopped operation must not probe")
                }),
            );
        }
        // Empty fresh names also prove that expiry is checked before reduction building.
        assert_unknown(entails_until(
            &o,
            &Axiom::EquivalentClasses(vec![a, b]),
            &[],
            &budget,
            deadline,
        ));
        assert_unknown(nonempty_until(&o, a, &budget, deadline));
        assert_unknown(entails_ontology_until(&o, &o, &[], &budget, deadline));
        assert!(matches!(
            consistency::check_until(&o, &budget, deadline).verdict,
            consistency::Verdict::Unknown(_)
        ));
    }
}

#[test]
fn unsupported_probe_fallback_inherits_controls_and_gave_up_is_terminal() {
    let mut o = Ontology::default();
    let a = intern(&mut o, ClassExpr::Class(1));
    o.axioms.push(Axiom::ClassAssertion(a, 1));
    let tests = [Test::Axiom(Axiom::ClassAssertion(a, 1))];
    let compiled = Batch::new(&mut o, &tests, None);
    let fresh = [100, 101, 102];
    for deadline in [
        Deadline::NONE,
        Deadline::after(Some(Duration::from_secs(60))),
    ] {
        let cancel = tableau::Cancel::default();
        let budget = Budget {
            timeout: None,
            cancel: Some(cancel.clone()),
            ..budget()
        };
        for stop in [false, true] {
            let answer = compiled.check_with(
                0,
                &tests[0],
                &o,
                &fresh,
                &budget,
                deadline,
                |_, _, config| {
                    assert_eq!(config.deadline, deadline.instant());
                    assert_eq!(config.timeout, None);
                    if stop {
                        cancel.cancel();
                    }
                    tableau::Answer::Unsupported("use scalar reduction".into())
                },
            );
            assert_eq!(
                answer,
                if stop {
                    Entailed::Unknown("cancelled".into())
                } else {
                    Entailed::Yes
                }
            );
        }
    }
    let answer = compiled.check_with(
        0,
        &tests[0],
        &o,
        &fresh,
        &budget(),
        Deadline::NONE,
        |_, _, _| tableau::Answer::GaveUp("test budget".into()),
    );
    assert_eq!(answer, Entailed::Unknown("gave up: test budget".into()));
    // A conclusive refutation is not erased by cancellation observed after the solver.
    let cancel = tableau::Cancel::default();
    let budget = Budget {
        cancel: Some(cancel.clone()),
        ..budget()
    };
    let answer = compiled.check_with(
        0,
        &tests[0],
        &o,
        &fresh,
        &budget,
        Deadline::NONE,
        |_, _, _| {
            cancel.cancel();
            tableau::Answer::Inconsistent
        },
    );
    assert_eq!(answer, Entailed::Yes);
}

#[test]
fn unlimited_scalar_and_public_zero_timeout_contracts() {
    use super::super::entails_ontology;

    let mut o = Ontology::default();
    let a = intern(&mut o, ClassExpr::Class(1));
    let fresh = [100, 101, 102];
    let axiom = Axiom::SubClassOf(a, a);
    let mut budget = Budget {
        timeout: None,
        ..budget()
    };
    assert_eq!(entails(&o, &axiom, &fresh, &budget), Entailed::Yes);
    assert_eq!(nonempty(&o, a, &budget), Entailed::No);
    assert_eq!(entails_ontology(&o, &o, &fresh, &budget), Entailed::Yes);
    budget.timeout = Some(Duration::ZERO);
    assert!(matches!(
        entails(&o, &axiom, &fresh, &budget),
        Entailed::Unknown(_)
    ));
    assert!(matches!(nonempty(&o, a, &budget), Entailed::Unknown(_)));
    assert!(matches!(
        entails_ontology(&o, &o, &fresh, &budget),
        Entailed::Unknown(_)
    ));
}
