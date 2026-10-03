//! The hypertableau on hand-made ontologies whose answers are known: each construct the
//! engine handles, consistent and inconsistent, with every optimisation on and off.

mod build;

use build::Build;
use nrese_dl::tableau::{Answer, Config, consistency, satisfiable};
use nrese_owl::{Characteristic, ClassExpr, ObjProp, normalise};

/// The answer under every combination of the switches; they must agree.
fn answer(o: &nrese_owl::Ontology) -> Answer {
    let mut answers = Vec::new();
    for bits in 0..16u32 {
        let config = Config {
            semantic_branching: bits & 1 != 0,
            backjumping: bits & 2 != 0,
            anywhere_blocking: bits & 4 != 0,
            single_blocking: bits & 8 != 0,
            ..Config::default()
        };
        answers.push(consistency(o, &config).answer);
    }
    assert!(
        answers.windows(2).all(|w| w[0] == w[1]),
        "switches change the answer: {answers:?}"
    );
    answers.remove(0)
}

#[test]
fn subsumption_and_disjointness() {
    let mut b = Build::default();
    let (a, bb) = (b.class(1), b.class(2));
    let nb = b.not(bb);
    b.sub(a, bb);
    b.sub(a, nb);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    let mut b = Build::default();
    let (a, bb) = (b.class(1), b.class(2));
    b.sub(a, bb);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
}

#[test]
fn disjunctions_branch_and_backtrack() {
    // A ⊑ B ⊔ C, B ⊑ ⊥, C ⊑ D ⊔ E, D ⊑ ⊥, E ⊑ ⊥: a:A is inconsistent.
    let mut b = Build::default();
    let [a, bb, c, d, e] = [1, 2, 3, 4, 5].map(|t| b.class(t));
    let nothing = b.e(ClassExpr::Nothing);
    let bc = b.or(&[bb, c]);
    let de = b.or(&[d, e]);
    b.sub(a, bc);
    b.sub(bb, nothing);
    b.sub(c, de);
    b.sub(d, nothing);
    b.sub(e, nothing);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // Without E ⊑ ⊥ it is consistent.
    b.o.axioms.pop();
    b.o.axioms
        .retain(|x| !matches!(x, nrese_owl::Axiom::SubClassOf(s, _) if *s == e));
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
}

#[test]
fn backjumping_skips_unrelated_choices() {
    // Many independent disjunctions, then a clash that depends on none of them.
    let mut b = Build::default();
    let a = b.class(1);
    for i in 0..12 {
        let (x, y) = (b.class(10 + 2 * i), b.class(11 + 2 * i));
        let xy = b.or(&[x, y]);
        b.sub(a, xy);
    }
    let (p, q) = (b.class(50), b.class(51));
    let some = b.some(ObjProp::Named(200), p);
    b.sub(a, some);
    let nq = b.not(q);
    b.sub(p, q);
    let all = b.all(ObjProp::Named(200), nq);
    b.sub(a, all);
    b.assert(a, 100);
    let config = Config::default();
    let out = consistency(&b.o, &config);
    assert_eq!(out.answer, Answer::Inconsistent);
}

#[test]
fn existentials_and_blocking() {
    // A ⊑ ∃R.A: an infinite chain, cut by blocking.
    let mut b = Build::default();
    let a = b.class(1);
    let some = b.some(ObjProp::Named(200), a);
    b.sub(a, some);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
    // ... and ∀R.¬A at the start: inconsistent.
    let na = b.not(a);
    let all = b.all(ObjProp::Named(200), na);
    let start = b.class(2);
    b.sub(start, all);
    b.assert(start, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn universal_restrictions_reach_asserted_successors() {
    let mut b = Build::default();
    let (a, bb) = (b.class(1), b.class(2));
    let all = b.all(ObjProp::Named(200), bb);
    b.sub(a, all);
    b.assert(a, 100);
    b.role_assertion(200, 100, 101);
    let nb = b.not(bb);
    b.assert(nb, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn inverses_propagate_back() {
    // A ⊑ ∃R.B, B ⊑ ∀R⁻.C, C ⊓ A ⊑ ⊥: a:A inconsistent.
    let mut b = Build::default();
    let [a, bb, c] = [1, 2, 3].map(|t| b.class(t));
    let some = b.some(ObjProp::Named(200), bb);
    b.sub(a, some);
    let back = b.all(ObjProp::Inverse(200), c);
    b.sub(bb, back);
    let both = b.and(&[a, c]);
    let nothing = b.e(ClassExpr::Nothing);
    b.sub(both, nothing);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn pairwise_blocking_with_inverses() {
    // A ⊑ ∃R.A, A ⊑ ∀R⁻.B... consistent; with B ⊑ ⊥ inconsistent only below the root.
    let mut b = Build::default();
    let [a, bb] = [1, 2].map(|t| b.class(t));
    let some = b.some(ObjProp::Named(200), a);
    b.sub(a, some);
    let back = b.all(ObjProp::Inverse(200), bb);
    b.sub(a, back);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
    let nb = b.not(bb);
    b.sub(a, nb);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn functional_roles_merge() {
    // Functional R, a: ∃R.B ⊓ ∃R.C, B ⊓ C ⊑ ⊥.
    let mut b = Build::default();
    let [bb, c] = [2, 3].map(|t| b.class(t));
    let (sb, sc) = (
        b.some(ObjProp::Named(200), bb),
        b.some(ObjProp::Named(200), c),
    );
    let both = b.and(&[sb, sc]);
    b.assert(both, 100);
    b.characteristic(Characteristic::Functional, ObjProp::Named(200));
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.disjoint(bb, c);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn number_restrictions() {
    // ≥ 3 R ⊓ ≤ 2 R is unsatisfiable; ≥ 2 R ⊓ ≤ 2 R isn't.
    for (min, max, expected) in [(3, 2, Answer::Inconsistent), (2, 2, Answer::Consistent)] {
        for expand in [0, 2, 8] {
            let mut b = Build::default();
            let thing = b.e(ClassExpr::Thing);
            let at_least = b.e(ClassExpr::Min(min, ObjProp::Named(200), thing));
            let at_most = b.e(ClassExpr::Max(max, ObjProp::Named(200), thing));
            let both = b.and(&[at_least, at_most]);
            b.assert(both, 100);
            let config = Config {
                expand_at_most_up_to: expand,
                ..Config::default()
            };
            assert_eq!(
                consistency(&b.o, &config).answer,
                expected,
                "{min} {max} {expand}"
            );
        }
    }
}

#[test]
fn qualified_at_most_merges_by_filler() {
    // a: ≥2 R.B ⊓ ≤1 R.B is inconsistent; ≥2 R.B ⊓ ≤1 R.C is fine.
    for expand in [0, 2] {
        let mut b = Build::default();
        let bb = b.class(2);
        let at_least = b.e(ClassExpr::Min(2, ObjProp::Named(200), bb));
        let at_most = b.e(ClassExpr::Max(1, ObjProp::Named(200), bb));
        let both = b.and(&[at_least, at_most]);
        b.assert(both, 100);
        let config = Config {
            expand_at_most_up_to: expand,
            ..Config::default()
        };
        assert_eq!(consistency(&b.o, &config).answer, Answer::Inconsistent);
        let mut b = Build::default();
        let [bb, c] = [2, 3].map(|t| b.class(t));
        let at_least = b.e(ClassExpr::Min(2, ObjProp::Named(200), bb));
        let at_most = b.e(ClassExpr::Max(1, ObjProp::Named(200), c));
        let both = b.and(&[at_least, at_most]);
        b.assert(both, 100);
        assert_eq!(consistency(&b.o, &config).answer, Answer::Consistent);
    }
}

#[test]
fn transitive_roles_through_automata() {
    let mut b = Build::default();
    let [a, bb] = [1, 2].map(|t| b.class(t));
    b.characteristic(Characteristic::Transitive, ObjProp::Named(200));
    let all = b.all(ObjProp::Named(200), bb);
    b.sub(a, all);
    b.assert(a, 100);
    b.role_assertion(200, 100, 101);
    b.role_assertion(200, 101, 102);
    let nb = b.not(bb);
    b.assert(nb, 102);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn individuals_and_equality() {
    let mut b = Build::default();
    let a = b.class(1);
    let na = b.not(a);
    b.assert(a, 100);
    b.assert(na, 101);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.same(100, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    let mut b = Build::default();
    b.same(100, 101);
    b.different(100, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn nominals() {
    // A ⊑ {o}, a:A, b:A, a ≠ b: inconsistent.
    let mut b = Build::default();
    let a = b.class(1);
    let o = b.e(ClassExpr::OneOf(vec![300]));
    b.sub(a, o);
    b.assert(a, 100);
    b.assert(a, 101);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.different(100, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn class_satisfiability() {
    let mut b = Build::default();
    let [a, bb, c] = [1, 2, 3].map(|t| b.class(t));
    let nb = b.not(bb);
    b.sub(a, bb);
    b.sub(c, nb);
    let ac = b.and(&[a, c]);
    let d = b.class(4);
    b.equivalent(d, ac);
    let n = normalise(&b.o);
    let config = Config::default();
    assert_eq!(
        satisfiable(&b.o, &n, 4, &config).answer,
        Answer::Inconsistent
    );
    assert_eq!(satisfiable(&b.o, &n, 1, &config).answer, Answer::Consistent);
    // A class the clauses never mention.
    assert_eq!(
        satisfiable(&b.o, &n, 99, &config).answer,
        Answer::Consistent
    );
}

#[test]
fn datatypes_are_unsupported_not_consistent() {
    let mut b = Build::default();
    let a = b.class(1);
    b.assert(a, 100);
    b.o.axioms
        .push(nrese_owl::Axiom::FunctionalDataProperty(400));
    b.o.sources.push(Vec::new());
    assert!(matches!(answer(&b.o), Answer::Unsupported(_)));
    // An inconsistency without the data part still stands.
    let na = b.not(a);
    b.assert(na, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn budgets_give_up() {
    // ∃R.A with A ⊑ ∃R.B ⊓ ∃R.C ... grows; a tiny node budget gives up.
    let mut b = Build::default();
    let a = b.class(1);
    let some = b.some(ObjProp::Named(200), a);
    b.sub(a, some);
    b.assert(a, 100);
    let config = Config {
        max_nodes: 1,
        ..Config::default()
    };
    assert!(matches!(
        consistency(&b.o, &config).answer,
        Answer::GaveUp(_)
    ));
}

#[test]
fn the_ni_rule_is_never_skipped_silently() {
    // JAIR 2009's caterpillar (8): S(a, a), a: ∃R.B, B ⊑ ∃R.C, C ⊑ ∃S.D, D ⊑ {a}, S
    // inverse-functional. Consistent; the derivation needs the NI rule (on c ≈ c), which
    // the engine lacks: it must give up, not answer from a model that isn't one.
    let mut b = Build::default();
    let [bb, c, d] = [2, 3, 4].map(|t| b.class(t));
    let (r, s) = (ObjProp::Named(200), ObjProp::Named(201));
    b.role_assertion(201, 100, 100);
    let rb = b.some(r, bb);
    b.assert(rb, 100);
    let rc = b.some(r, c);
    b.sub(bb, rc);
    let sd = b.some(s, d);
    b.sub(c, sd);
    let a = b.e(ClassExpr::OneOf(vec![100]));
    b.sub(d, a);
    b.characteristic(Characteristic::InverseFunctional, s);
    for expand in [0, 2] {
        let config = Config {
            expand_at_most_up_to: expand,
            ..Config::default()
        };
        let answer = consistency(&b.o, &config).answer;
        eprintln!("caterpillar, expand {expand}: {answer:?}");
        assert!(
            matches!(answer, Answer::Consistent | Answer::GaveUp(_)),
            "{answer:?}"
        );
    }
}

#[test]
fn premature_blocking_without_the_ni_rule_is_caught() {
    // JAIR 2009's (9): A(a), a: ∃R.B, A ⊑ ∀R⁻.⊥, B ⊑ ∃R.B, B ⊑ ∃S.{a}, R
    // inverse-functional, ⊤ ⊑ ≤3 S⁻.⊤. Inconsistent; without the NI rule a blocked chain
    // looks like a model (Figure 9a). The engine may give up, never answer consistent.
    let mut b = Build::default();
    let [a, bb] = [1, 2].map(|t| b.class(t));
    let (r, s) = (ObjProp::Named(200), ObjProp::Named(201));
    let thing = b.e(ClassExpr::Thing);
    let nothing = b.e(ClassExpr::Nothing);
    b.assert(a, 100);
    let rb = b.some(r, bb);
    b.assert(rb, 100);
    let none_back = b.all(ObjProp::Inverse(200), nothing);
    b.sub(a, none_back);
    b.sub(bb, rb);
    let at_a = b.e(ClassExpr::OneOf(vec![100]));
    let s_a = b.some(s, at_a);
    b.sub(bb, s_a);
    b.characteristic(Characteristic::InverseFunctional, r);
    let at_most = b.e(ClassExpr::Max(3, ObjProp::Inverse(201), thing));
    b.sub(thing, at_most);
    for expand in [0, 2, 3] {
        let config = Config {
            expand_at_most_up_to: expand,
            ..Config::default()
        };
        let answer = consistency(&b.o, &config).answer;
        eprintln!("premature blocking, expand {expand}: {answer:?}");
        assert!(
            matches!(answer, Answer::Inconsistent | Answer::GaveUp(_)),
            "{expand}: {answer:?}"
        );
    }
}
