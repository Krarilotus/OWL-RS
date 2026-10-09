//! Retained searches against fresh searches and explicit ontology assumptions.
use super::build::Build;
use nrese_dl::tableau::{self, Answer, At, Cancel, Config, From, Prepared, Probe, Want};
use nrese_owl::{Axiom, ClassExpr, normalise};
use std::time::Duration;

fn choice() -> Build {
    let mut b = Build::default();
    let (a, c) = (b.class(1), b.class(2));
    let either = b.or(&[a, c]);
    b.assert(either, 100);
    b
}

fn want() -> Want {
    Want {
        individuals: true,
        elements: true,
        detached: false,
    }
}

#[test]
fn retained_probes_rollback_and_fall_back_like_explicit_assumptions() {
    let b = choice();
    let p = Prepared::new(&b.o, &normalise(&b.o), &[1, 2], &[100]);
    let config = Config::default();
    let (initial, base) = p.consistency_with_base(&config, want(), true);
    let base = base.unwrap();
    assert_eq!(initial.answer, Answer::Consistent);
    let chosen = initial.labels.as_ref().unwrap().individuals[0]
        .as_ref()
        .unwrap()
        .classes[0];
    let alternative = 1 - chosen;
    let mut floors = 0;
    // Revisit the same pool after contradictory assumptions and a floor fallback.
    for negative in [
        vec![chosen],
        vec![0, 1],
        vec![alternative],
        vec![],
        vec![chosen],
    ] {
        let probe = Probe {
            at: At::Individual(0),
            positive: &[],
            negative: &negative,
        };
        let (out, from) = base.probe(&probe, &config, want()).unwrap();
        floors += usize::from(from == From::Deterministic);
        let mut explicit = Build { o: b.o.clone() };
        for &c in &negative {
            let class = explicit.class(u64::from(c) + 1);
            let not = explicit.not(class);
            explicit.assert(not, 100);
        }
        let expected = tableau::consistency(&explicit.o, &config);
        assert_eq!(out.answer, expected.answer, "negative={negative:?}");
        assert_eq!(out.answer, p.probe(&probe, &config, want()).answer);
        if negative == [chosen] {
            assert_eq!(from, From::Deterministic);
            assert!(out.telemetry.clashes > 0, "floor attempt's work was lost");
            let label = out.labels.unwrap().individuals[0].clone().unwrap();
            assert!(!label.classes.contains(&chosen));
            assert!(label.classes.contains(&alternative));
        }
    }
    assert!(floors >= 2);
    let (again, _) = p.consistency_with_base(&config, want(), false);
    assert_eq!(
        again.labels.unwrap().individuals,
        initial.labels.unwrap().individuals
    );
}

#[test]
fn a_choice_free_model_is_the_deterministic_base() {
    let mut b = Build::default();
    let a = b.class(1);
    b.assert(a, 100);
    let p = Prepared::new(&b.o, &normalise(&b.o), &[1], &[100]);
    let config = Config::default();
    let (initial, base) = p.consistency_with_base(&config, want(), true);
    assert_eq!(initial.answer, Answer::Consistent);
    let base = base.unwrap();
    for negative in [&[][..], &[0][..], &[][..]] {
        let probe = Probe {
            at: At::Individual(0),
            positive: &[],
            negative,
        };
        let (out, from) = base.probe(&probe, &config, want()).unwrap();
        assert_eq!(from, From::Deterministic);
        assert_eq!(out.answer, p.probe(&probe, &config, want()).answer);
    }
}

#[test]
fn retained_probe_assumptions_follow_deterministic_merges() {
    for positive in [false, true] {
        let mut b = Build::default();
        let c = b.class(1);
        let asserted = if positive { b.not(c) } else { c };
        b.assert(asserted, 100);
        b.same(100, 101);
        let p = Prepared::new(&b.o, &normalise(&b.o), &[1], &[100, 101]);
        let config = Config::default();
        let (_, base) = p.consistency_with_base(&config, want(), true);
        let base = base.unwrap();
        for individual in [0, 1, 1, 0] {
            let probe = Probe {
                at: At::Individual(individual),
                positive: if positive { &[0] } else { &[] },
                negative: if positive { &[] } else { &[0] },
            };
            let (out, _) = base.probe(&probe, &config, want()).unwrap();
            assert_eq!(
                out.answer,
                Answer::Inconsistent,
                "individual={individual}, positive={positive}"
            );
            assert_eq!(out.answer, p.probe(&probe, &config, want()).answer);
        }
    }
}

#[test]
fn retained_probe_assumptions_preserve_choice_merge_dependencies() {
    let mut b = Build::default();
    let c = b.class(1);
    let not = b.not(c);
    b.assert(c, 100);
    b.assert(not, 101);
    let either = b.e(ClassExpr::OneOf(vec![100, 101]));
    b.assert(either, 102);
    let p = Prepared::new(&b.o, &normalise(&b.o), &[1], &[100, 101, 102]);
    let config = Config::default();
    let (initial, base) = p.consistency_with_base(&config, want(), true);
    assert_eq!(initial.answer, Answer::Consistent);
    let chosen = initial.labels.unwrap().individuals[2]
        .as_ref()
        .unwrap()
        .classes
        .contains(&0);
    let base = base.unwrap();
    // Contradict the chosen alias, then revisit the same pooled model after fallback.
    for positive in [!chosen, chosen, !chosen] {
        let probe = Probe {
            at: At::Individual(2),
            positive: if positive { &[0] } else { &[] },
            negative: if positive { &[] } else { &[0] },
        };
        let (out, from) = base.probe(&probe, &config, want()).unwrap();
        let mut explicit = Build { o: b.o.clone() };
        explicit.assert(if positive { c } else { not }, 102);
        assert_eq!(
            out.answer,
            tableau::consistency(&explicit.o, &config).answer
        );
        assert_eq!(out.answer, p.probe(&probe, &config, want()).answer);
        assert_eq!(out.answer, Answer::Consistent);
        let labels = out.labels.unwrap();
        let label = labels.individuals[2].as_ref().unwrap();
        assert_eq!(&labels.probe, label);
        assert_eq!(label.classes.contains(&0), positive);
        if positive != chosen {
            assert_eq!(from, From::Deterministic);
        }
    }
}

#[test]
fn retained_probes_use_the_requested_controls_and_decline_changed_strategy() {
    let b = choice();
    let p = Prepared::new(&b.o, &normalise(&b.o), &[1, 2], &[100]);
    let original = Cancel::default();
    let config = Config {
        cancel: Some(original.clone()),
        ..Config::default()
    };
    let (_, base) = p.consistency_with_base(&config, want(), true);
    let base = base.unwrap();
    let probe = Probe {
        at: At::Individual(0),
        positive: &[],
        negative: &[],
    };
    original.cancel();
    // The retained state does not keep the old request's cancellation contract.
    let current = Config::default();
    assert_eq!(
        base.probe(&probe, &current, want()).unwrap().0.answer,
        Answer::Consistent
    );
    let stopped = Cancel::default();
    stopped.cancel();
    for request in [
        Config {
            cancel: Some(stopped),
            ..current.clone()
        },
        Config {
            timeout: Some(Duration::ZERO),
            ..current.clone()
        },
    ] {
        let out = base.probe(&probe, &request, want()).unwrap().0;
        assert!(matches!(out.answer, Answer::GaveUp(_)));
        assert!(out.labels.is_none());
        assert_eq!(out.telemetry.nodes_created, 0);
        assert_eq!(out.telemetry.plans_tried, 0);
    }
    for request in [
        Config {
            semantic_branching: false,
            ..current.clone()
        },
        Config {
            max_nodes: 0,
            ..current.clone()
        },
        Config {
            max_memory: 0,
            ..current.clone()
        },
        Config {
            workers: Some(nrese_exec::workers::Workers::serial()),
            ..current.clone()
        },
    ] {
        assert!(base.probe(&probe, &request, want()).is_none());
    }
    assert_eq!(
        base.probe(&probe, &current, want()).unwrap().0.answer,
        Answer::Consistent
    );
}

#[test]
fn unknown_consistency_keeps_deterministic_state_without_claiming_a_model() {
    let b = choice();
    let p = Prepared::new(&b.o, &normalise(&b.o), &[1, 2], &[100]);
    let config = Config {
        max_branch_points: Some(0),
        ..Config::default()
    };
    let (out, base) = p.consistency_with_base(&config, want(), true);
    assert!(matches!(out.answer, Answer::GaveUp(_)));
    assert!(out.labels.is_none());
    let base = base.expect("the saturated deterministic state is still usable");
    let open = Probe {
        at: At::Individual(0),
        positive: &[],
        negative: &[],
    };
    let (unknown, from) = base.probe(&open, &config, want()).unwrap();
    assert!(matches!(unknown.answer, Answer::GaveUp(_)));
    assert_eq!(from, From::Deterministic);
    // An assumption resolves the choice without spending any branch points.
    let resolved = Probe {
        positive: &[0],
        ..open
    };
    let (out, from) = base.probe(&resolved, &config, want()).unwrap();
    assert_eq!(out.answer, Answer::Consistent);
    assert_eq!(from, From::Deterministic);
    assert_eq!(out.answer, p.probe(&resolved, &config, want()).answer);
}

#[test]
fn an_unknown_resumed_probe_is_not_retried_and_does_not_poison_the_pool() {
    let b = choice();
    let p = Prepared::new(&b.o, &normalise(&b.o), &[1, 2], &[100]);
    let config = Config {
        max_nodes: 1,
        ..Config::default()
    };
    let (_, base) = p.consistency_with_base(&config, want(), true);
    let base = base.unwrap();
    let fresh = Probe {
        at: At::Fresh,
        positive: &[],
        negative: &[],
    };
    let (unknown, from) = base.probe(&fresh, &config, want()).unwrap();
    assert!(matches!(unknown.answer, Answer::GaveUp(_)));
    assert_eq!(from, From::Model);
    assert!(unknown.labels.is_none());
    let existing = Probe {
        at: At::Individual(0),
        ..fresh
    };
    assert_eq!(
        base.probe(&existing, &config, want()).unwrap().0.answer,
        Answer::Consistent
    );
}

#[test]
fn initial_retention_preserves_shortcuts_unknowns_and_no_retention_paths() {
    let b = choice();
    let p = Prepared::new(&b.o, &normalise(&b.o), &[1, 2], &[100]);
    let cancelled = Cancel::default();
    cancelled.cancel();
    for config in [
        Config {
            cancel: Some(cancelled),
            ..Config::default()
        },
        Config {
            timeout: Some(Duration::ZERO),
            ..Config::default()
        },
    ] {
        for retain in [false, true] {
            let (out, base) = p.consistency_with_base(&config, want(), retain);
            assert!(matches!(out.answer, Answer::GaveUp(_)));
            assert!(out.labels.is_none() && base.is_none());
            assert_eq!(out.telemetry.nodes_created, 0);
        }
    }
    let config = Config::default();
    let (out, base) = p.consistency_with_base(&config, want(), false);
    let plain = p.probe(
        &Probe {
            at: At::Nothing,
            positive: &[],
            negative: &[],
        },
        &config,
        want(),
    );
    assert!(base.is_none());
    assert_eq!(out.answer, plain.answer);
    assert_eq!(out.telemetry.nodes_created, plain.telemetry.nodes_created);
    assert_eq!(
        out.labels.unwrap().individuals,
        plain.labels.unwrap().individuals
    );

    let bad_product = super::multiplication(2, 3, 7, true);
    let product = Prepared::new(&bad_product, &normalise(&bad_product), &[], &[]);
    let (out, base) = product.consistency_with_base(&config, want(), true);
    assert_eq!(out.answer, Answer::Inconsistent);
    assert_eq!(
        out.telemetry.nodes_created, 0,
        "the existing counting shortcut was bypassed"
    );
    assert!(base.is_none());

    let mut tbox = b.o;
    tbox.axioms
        .retain(|a| !matches!(a, Axiom::ClassAssertion(..)));
    tbox.sources = vec![vec![]; tbox.axioms.len()];
    let p = Prepared::new(&tbox, &normalise(&tbox), &[1, 2], &[]);
    assert!(p.consistency_with_base(&config, want(), true).1.is_none());
}
