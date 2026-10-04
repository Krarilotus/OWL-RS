//! The NI rule (JAIR 2009, Table 5) on the W3C tests that needed it, minimised: each was
//! "gave-up" before the rule existed. Every switch combination must decide them alike,
//! within a budget.

use std::time::Duration;

use super::build::Build;
use nrese_dl::tableau::{Answer, Config, consistency, satisfiable};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ObjProp, Ontology, normalise};

fn configs() -> impl Iterator<Item = Config> {
    (0..16u32).map(|bits| Config {
        semantic_branching: bits & 1 != 0,
        backjumping: bits & 2 != 0,
        anywhere_blocking: bits & 4 != 0,
        single_blocking: bits & 8 != 0,
        timeout: Some(Duration::from_secs(2)),
        max_nodes: 100_000,
        max_memory: 512 << 20,
        check_blocking: true,
        ..Config::default()
    })
}

/// The answer under the default switches. No other switch combination or at-most
/// encoding may answer differently; they may give up (chronological backtracking thrashes
/// on the multiplications, and `≤ 6` spelled out as clauses joins every 7 of d's
/// neighbours). `quick`: only with backjumping, at-most restrictions as atoms or up to 2
/// spelled out (what gives up elsewhere would take the budget each time).
fn decided(o: &Ontology, quick: bool) -> Answer {
    let default = Config {
        timeout: Some(Duration::from_secs(20)),
        ..configs().last().expect("16 configs")
    };
    let default = consistency(o, &default).answer;
    assert!(
        matches!(default, Answer::Consistent | Answer::Inconsistent),
        "{default:?}"
    );
    let expansions: &[u32] = if quick { &[0, 2] } else { &[0, 2, 8] };
    for &expand in expansions {
        for config in configs().filter(|c| !quick || c.backjumping) {
            let config = Config {
                expand_at_most_up_to: expand,
                ..config
            };
            let answer = consistency(o, &config).answer;
            assert!(
                answer == default || matches!(answer, Answer::GaveUp(_)),
                "{answer:?}, not {default:?}, under {config:?}"
            );
        }
    }
    default
}

fn inverse_pair(b: &mut Build, p: u64, q: u64) {
    b.axiom(Axiom::InverseObjectProperties(
        ObjProp::Named(p),
        ObjProp::Named(q),
    ));
}

/// DL-035: x: ≥k r, ⊤ ⊑ ∃p.{spy}, spy: ≤2 p⁻. Every element is a p-predecessor of spy,
/// so there are at most two: x's k distinct r-successors fit for k = 2, not for k = 3.
fn spy(k: u32) -> Ontology {
    let mut b = Build::default();
    let (r, p, inv_p) = (200, 201, 202);
    let x = b.class(1);
    let thing = b.e(ClassExpr::Thing);
    let many = b.e(ClassExpr::Min(k, ObjProp::Named(r), thing));
    b.sub(x, many);
    let spy = b.e(ClassExpr::OneOf(vec![300]));
    let to_spy = b.some(ObjProp::Named(p), spy);
    b.sub(thing, to_spy);
    inverse_pair(&mut b, p, inv_p);
    let two = b.e(ClassExpr::Max(2, ObjProp::Named(inv_p), thing));
    b.assert(two, 300);
    b.assert(x, 301);
    b.o
}

#[test]
fn every_element_a_neighbour_of_one_nominal() {
    assert_eq!(decided(&spy(3), false), Answer::Inconsistent);
    assert_eq!(decided(&spy(2), false), Answer::Consistent);
}

/// DL-905 and DL-909/910 scaled down (integer multiplication): d has exactly N p-
/// predecessors, each exactly M q-predecessors, and d exactly K r-predecessors, every q-
/// predecessor being an r-predecessor of d (all three properties functional). Consistent
/// iff K = N·M.
fn multiplication(n: u32, m: u32, k: u32) -> Ontology {
    let mut b = Build::default();
    let (p, inv_p, q, inv_q, r, inv_r) = (300, 301, 302, 303, 304, 305);
    let [card_n, only_d, card_nm] = [10, 11, 12].map(|t| b.class(t));
    let thing = b.e(ClassExpr::Thing);
    let d = b.e(ClassExpr::OneOf(vec![100]));
    b.equivalent(only_d, d);
    let exact =
        |b: &mut Build, n: u32, role: u64| b.e(ClassExpr::Exact(n, ObjProp::Named(role), thing));
    let some_p = b.some(ObjProp::Named(p), only_d);
    b.equivalent(card_n, some_p);
    let m_q = exact(&mut b, m, inv_q);
    b.equivalent(card_n, m_q);
    let n_p = exact(&mut b, n, inv_p);
    b.equivalent(only_d, n_p);
    let k_r = exact(&mut b, k, inv_r);
    b.equivalent(only_d, k_r);
    let some_q = b.some(ObjProp::Named(q), card_n);
    b.equivalent(card_nm, some_q);
    let some_r = b.some(ObjProp::Named(r), only_d);
    b.equivalent(card_nm, some_r);
    for (role, inverse, domain, range) in [
        (p, inv_p, card_n, only_d),
        (q, inv_q, card_nm, card_n),
        (r, inv_r, card_nm, only_d),
    ] {
        inverse_pair(&mut b, role, inverse);
        b.axiom(Axiom::ObjectPropertyDomain(ObjProp::Named(role), domain));
        b.axiom(Axiom::ObjectPropertyRange(ObjProp::Named(role), range));
        b.characteristic(Characteristic::Functional, ObjProp::Named(role));
    }
    b.assert(thing, 100);
    b.o
}

#[test]
fn integer_multiplication_through_nominals() {
    assert_eq!(decided(&multiplication(2, 3, 6), true), Answer::Consistent);
    assert_eq!(
        decided(&multiplication(1, 2, 3), true),
        Answer::Inconsistent
    );
    assert_eq!(
        decided(&multiplication(2, 2, 3), true),
        Answer::Inconsistent
    );
}

/// one=two and Consistent-but-all-unsat (Horrocks's): a ⊑ {i, j, k} with functional and
/// inverse-functional roles that force twice as many 2a-elements as a-elements, and as
/// many as b- and c-elements together. With three distinct individuals the ontology is
/// inconsistent; without the assertion, `a` is unsatisfiable.
fn one_equals_two(different: bool) -> (Ontology, u64) {
    let mut b = Build::default();
    let [two_a, b_or_c, a, bb, c] = [1, 2, 3, 4, 5].map(|t| b.class(t));
    let roles = [200, 201, 202, 203, 204, 205, 206, 207];
    let [
        to_a,
        to_bc,
        a_to_2a,
        a_to_b,
        bc_to_2a,
        b_to_a,
        b_to_c,
        c_to_b,
    ] = roles;
    for (sub, role, filler) in [
        (two_a, to_bc, b_or_c),
        (two_a, to_a, a),
        (b_or_c, bc_to_2a, two_a),
        (a, a_to_b, bb),
        (a, a_to_2a, two_a),
        (bb, b_to_a, a),
        (bb, b_to_c, c),
        (c, c_to_b, bb),
    ] {
        let some = b.some(ObjProp::Named(role), filler);
        b.sub(sub, some);
    }
    let either = b.or(&[bb, c]);
    b.equivalent(b_or_c, either);
    let nominals = b.e(ClassExpr::OneOf(vec![100, 101, 102]));
    if different {
        b.equivalent(a, nominals);
    } else {
        b.sub(a, nominals);
    }
    for (x, y) in [
        (two_a, b_or_c),
        (two_a, a),
        (two_a, bb),
        (two_a, c),
        (a, bb),
        (a, c),
        (bb, c),
    ] {
        b.disjoint(x, y);
    }
    for (x, y) in [
        (to_a, a_to_2a),
        (to_bc, bc_to_2a),
        (a_to_b, b_to_a),
        (b_to_c, c_to_b),
    ] {
        inverse_pair(&mut b, x, y);
    }
    for role in roles {
        b.characteristic(Characteristic::Functional, ObjProp::Named(role));
        b.characteristic(Characteristic::InverseFunctional, ObjProp::Named(role));
    }
    if different {
        b.axiom(Axiom::DifferentIndividuals(vec![100, 101, 102]));
    }
    (b.o, 3)
}

#[test]
fn one_equals_two_and_its_unsatisfiable_class() {
    let (o, _) = one_equals_two(true);
    assert_eq!(decided(&o, false), Answer::Inconsistent);
    let (o, a) = one_equals_two(false);
    assert_eq!(decided(&o, false), Answer::Consistent);
    let n = normalise(&o);
    for config in configs() {
        assert_eq!(
            satisfiable(&o, &n, a, &config).answer,
            Answer::Inconsistent,
            "{config:?}"
        );
    }
}
