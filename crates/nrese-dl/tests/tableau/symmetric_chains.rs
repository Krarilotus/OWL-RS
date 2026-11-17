//! The hypertableau on a chain into a role that holds its pairs both ways. By *OWL 2 Direct
//! Semantics* §2.3.2 (Table 6), `p q ⊑ r` over `p(x, b)` and `q(b, z)` gives `r(x, z)`; with
//! `r` symmetric (or its own inverse) `r(z, x)` too, so `r`'s range `C` holds at both ends,
//! and an end that is a `D`, with `C` and `D` disjoint (Table 5), leaves no model.
//! `p(x, b)` comes from `x : ∃p.{b}` (§2.2.3, Table 4), so no `p` edge is stated.

use nrese_dl::tableau::Answer;
use nrese_owl::{Axiom, Characteristic, ClassExpr, ObjProp, Term};

use super::answer;
use super::build::Build;

const X: Term = 100;
const B: Term = 101;
const Z: Term = 102;
const C: Term = 103;
const P: ObjProp = ObjProp::Named(200);
const Q: ObjProp = ObjProp::Named(201);
const R: ObjProp = ObjProp::Named(202);
const S: ObjProp = ObjProp::Named(203);
const T: ObjProp = ObjProp::Named(204);

/// `x : ∃p.{b}`, `q(b, z)` and `links`; `r`'s range the class `C`, `C` and `D` disjoint,
/// and the individual `clash` a `D`; then `rbox`.
fn ontology(clash: Term, rbox: &[Axiom], links: &[(ObjProp, Term, Term)]) -> Build {
    let mut b = Build::default();
    let (c, d) = (b.class(1), b.class(2));
    let only_b = b.e(ClassExpr::OneOf(vec![B]));
    let to_b = b.some(P, only_b);
    b.assert(to_b, X);
    b.role_assertion(Q.named(), B, Z);
    for &(role, from, to) in links {
        b.role_assertion(role.named(), from, to);
    }
    b.axiom(Axiom::ObjectPropertyRange(R, c));
    b.disjoint(c, d);
    b.assert(d, clash);
    for axiom in rbox {
        b.axiom(axiom.clone());
    }
    b
}

fn chain(roles: &[ObjProp], sup: ObjProp) -> Axiom {
    Axiom::SubObjectPropertyOf(roles.to_vec(), sup)
}

fn symmetric(r: ObjProp) -> Axiom {
    Axiom::ObjectCharacteristic(Characteristic::Symmetric, r)
}

/// Spec: *OWL 2 Direct Semantics* Tables 1, 5 and 6, each case's argument on its line:
/// where the clashing end is a `C` in every model, the ontology is inconsistent; the
/// controls name a model.
#[test]
fn a_chain_into_a_role_that_holds_its_pairs_both_ways_gives_both_ends_its_range() {
    let cases: Vec<(&str, Build, Answer)> = vec![
        // r(x, z) by the chain, r(z, x) by symmetry: C(x).
        (
            "symmetric",
            ontology(X, &[symmetric(R), chain(&[P, Q], R)], &[]),
            Answer::Inconsistent,
        ),
        // r its own inverse: r(x, z) is r⁻(x, z), so r(z, x): C(x).
        (
            "its own inverse",
            ontology(
                X,
                &[Axiom::InverseObjectProperties(R, R), chain(&[P, Q], R)],
                &[],
            ),
            Answer::Inconsistent,
        ),
        // r ≡ r⁻: as above.
        (
            "equivalent to its inverse",
            ontology(
                X,
                &[
                    Axiom::EquivalentObjectProperties(vec![R, R.inverse()]),
                    chain(&[P, Q], R),
                ],
                &[],
            ),
            Answer::Inconsistent,
        ),
        // p q ⊑ r⁻ gives r(z, x), symmetry r(x, z): C(z), the end the inverse doesn't reach.
        (
            "into the inverse, symmetric",
            ontology(Z, &[symmetric(R), chain(&[P, Q], R.inverse())], &[]),
            Answer::Inconsistent,
        ),
        // p q t ⊑ r over p(x, b), q(b, c), t(c, z): r(x, z), r(z, x): C(x).
        (
            "a chain of three",
            ontology(
                X,
                &[symmetric(R), chain(&[P, Q, T], R)],
                &[(Q, B, C), (T, C, Z)],
            ),
            Answer::Inconsistent,
        ),
        // r transitive and symmetric: r(x, z), r(z, x): C(x).
        (
            "transitive and symmetric",
            ontology(
                X,
                &[
                    symmetric(R),
                    Axiom::ObjectCharacteristic(Characteristic::Transitive, R),
                    chain(&[P, Q], R),
                ],
                &[],
            ),
            Answer::Inconsistent,
        ),
        // r ≡ s⁻, p q ⊑ s: s(x, z), so r(z, x): C(x), symmetric s or not.
        (
            "r ≡ s⁻",
            ontology(
                X,
                &[
                    Axiom::EquivalentObjectProperties(vec![R, S.inverse()]),
                    chain(&[P, Q], S),
                ],
                &[],
            ),
            Answer::Inconsistent,
        ),
        (
            "r ≡ s⁻, s symmetric",
            ontology(
                X,
                &[
                    Axiom::EquivalentObjectProperties(vec![R, S.inverse()]),
                    symmetric(S),
                    chain(&[P, Q], S),
                ],
                &[],
            ),
            Answer::Inconsistent,
        ),
        // Controls. Without symmetry, r = {(x, z)} and C = {z} make a model: x isn't a C.
        (
            "control: no symmetry",
            ontology(X, &[chain(&[P, Q], R)], &[]),
            Answer::Consistent,
        ),
        // r ≡ s⁻, p q ⊑ s: r = {(z, x)} and C = {x} make a model: z isn't a C.
        (
            "control: r ≡ s⁻, at z",
            ontology(
                Z,
                &[
                    Axiom::EquivalentObjectProperties(vec![R, S.inverse()]),
                    chain(&[P, Q], S),
                ],
                &[],
            ),
            Answer::Consistent,
        ),
    ];
    let mut wrong = Vec::new();
    for (name, b, expected) in cases {
        let actual = answer(&b.o);
        println!("{name}: {actual:?}, expected {expected:?}");
        if actual != expected {
            wrong.push(format!("{name}: {actual:?}, expected {expected:?}"));
        }
    }
    assert!(wrong.is_empty(), "wrong answers:\n{}", wrong.join("\n"));
}
