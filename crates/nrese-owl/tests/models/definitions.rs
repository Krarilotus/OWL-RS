//! Fresh definitions checked by the existing direct-semantics oracle, including every
//! auxiliary interpretation. Canonical extensions alone would miss a lost definition.

use std::collections::HashMap;

use nrese_owl::{
    Axiom, ClassExpr, ExprId, FreshOf, Normalised, ObjProp, Ontology, Options, Term, normalise_with,
};

use super::{Interp, clauses_hold, extend, holds};

fn expr(o: &mut Ontology, e: ClassExpr) -> ExprId {
    ExprId(o.classes.intern(e))
}

fn atoms(o: &mut Ontology, width: usize) -> Vec<ExprId> {
    (1..=width as Term)
        .map(|t| expr(o, ClassExpr::Class(t)))
        .collect()
}

fn interpretation(width: usize, all: u64, changed: Option<(Term, u64)>) -> Interp {
    let mut i = Interp {
        n: 1,
        concepts: (1..=width as Term).map(|t| (t, all)).collect(),
        roles: HashMap::from([(100, vec![1]), (101, vec![1])]),
        individuals: HashMap::from([(200, 0)]),
        fresh: Vec::new(),
    };
    if let Some((t, value)) = changed {
        i.concepts.insert(t, value);
    }
    i
}

/// Both directions of equisatisfiability; enumeration also detects definitions that
/// constrain an auxiliary in the wrong direction or omit any operand.
fn check_extensions(o: &Ontology, n: &Normalised, mut i: Interp) {
    assert!(n.unsupported.is_empty());
    assert!(n.fresh.len() <= 8, "a focused fixture must stay enumerable");
    let expected = o.axioms.iter().all(|a| holds(o, a, &i));
    extend(n, &mut i);
    assert_eq!(clauses_hold(n, &i), expected, "canonical: {i:?}");
    let extends = (0u64..1 << n.fresh.len()).any(|code| {
        for q in 0..n.fresh.len() {
            i.fresh[q] = (code >> q) & 1;
        }
        clauses_hold(n, &i)
    });
    assert_eq!(extends, expected, "arbitrary auxiliaries: {i:?}");
}

#[test]
fn wide_fresh_definitions_preserve_both_polarities() {
    for width in [0, 1, 15, 16, 17, 33] {
        for conjunction in [false, true] {
            for positive in [false, true] {
                let mut o = Ontology::default();
                let xs = atoms(&mut o, width);
                let e = expr(
                    &mut o,
                    if conjunction {
                        ClassExpr::And(xs)
                    } else {
                        ClassExpr::Or(xs)
                    },
                );
                let r = ObjProp::Named(100);
                let restriction = expr(
                    &mut o,
                    if positive {
                        ClassExpr::Some(r, e)
                    } else {
                        ClassExpr::Max(0, r, e)
                    },
                );
                o.axioms.push(Axiom::ClassAssertion(restriction, 200));
                let n = normalise_with(&o, Options::default());
                for all in [0, 1] {
                    check_extensions(&o, &n, interpretation(width, all, None));
                    for t in 1..=width as Term {
                        check_extensions(&o, &n, interpretation(width, all, Some((t, 1 - all))));
                    }
                }
            }
        }
    }
}

#[test]
fn nested_shared_conjunctions_keep_linear_definitions() {
    let width = 17;
    let mut o = Ontology::default();
    let xs = atoms(&mut o, width);
    let shared = expr(&mut o, ClassExpr::And(xs));
    let groups: Vec<ExprId> = (18..23)
        .map(|t| {
            let c = expr(&mut o, ClassExpr::Class(t));
            expr(&mut o, ClassExpr::And(vec![shared, c]))
        })
        .collect();
    // Distribution would produce 2^5 combinations, beyond the existing cutoff.
    let union = expr(&mut o, ClassExpr::Or(groups.clone()));
    for t in [30, 31] {
        let c = expr(&mut o, ClassExpr::Class(t));
        o.axioms.push(Axiom::SubClassOf(c, union));
    }
    let n = normalise_with(&o, Options::default());
    assert_eq!(
        n.fresh.len(),
        groups.len() + 1,
        "reuse the shared conjunction"
    );
    assert!(n.clauses.len() <= 2 + 2 * groups.len() + width);
    for all in [0, 1] {
        let changes = (1..=width as Term).map(|t| Some((t, 1 - all)));
        for changed in std::iter::once(None).chain(changes) {
            let mut i = interpretation(31, all, changed);
            i.concepts.insert(30, 1);
            i.concepts.insert(31, 1);
            check_extensions(&o, &n, i);
        }
    }
}

#[test]
fn reused_fillers_keep_opposite_polarities_distinct() {
    let width = 17;
    let mut o = Ontology::default();
    let xs = atoms(&mut o, width);
    let shared = expr(&mut o, ClassExpr::And(xs));
    let some = expr(&mut o, ClassExpr::Some(ObjProp::Named(100), shared));
    let none = expr(&mut o, ClassExpr::Max(0, ObjProp::Named(101), shared));
    for t in [110, 111] {
        let c = expr(&mut o, ClassExpr::Class(t));
        o.axioms.push(Axiom::SubClassOf(c, some));
        let c = expr(&mut o, ClassExpr::Class(t + 2));
        o.axioms.push(Axiom::SubClassOf(c, none));
    }
    let n = normalise_with(&o, Options::default());
    assert_eq!(n.fresh.len(), 2, "one shared name per polarity");
    for positive in [false, true] {
        assert!(n.fresh.iter().any(|of| matches!(
            of,
            FreshOf::Expr { expr, positive: p } if *expr == shared && *p == positive
        )));
    }
    for all in [0, 1] {
        for positive in [0, 1] {
            for negative in [0, 1] {
                for edge in [0, 1] {
                    let mut i = interpretation(width, all, None);
                    for t in [110, 111] {
                        i.concepts.insert(t, positive);
                        i.concepts.insert(t + 2, negative);
                    }
                    i.roles.insert(101, vec![edge]);
                    check_extensions(&o, &n, i);
                }
            }
        }
    }
}
