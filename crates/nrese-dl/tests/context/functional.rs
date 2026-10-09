//! The Eq rule: functional and inverse-functional properties in the Horn stage. Each
//! merge (two successors, a successor with the predecessor, under an inverse, under a
//! guard), and what stays refused.

use nrese_dl::context::{self, Classification, Options, Unsupported};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ObjProp, Ontology};

use super::support::{Build, Table, holds};

/// Classifies with the Eq rule on.
fn classify(o: &Ontology, threads: usize) -> Result<Classification, Unsupported> {
    let mut options = Options {
        threads,
        equality: true,
        ..Options::default()
    };
    let unlimited = context::classify(o, &options).map(|(c, _)| c);
    options.budget.task_memory = Some(64 * 1024 * 1024);
    let budgeted = context::classify(o, &options).map(|(c, _)| c);
    assert_eq!(
        budgeted, unlimited,
        "accounting must preserve equality reasoning"
    );
    budgeted
}

/// `A ⊑ ∃r.B ⊓ ∃r.C`, `∃r.(B ⊓ C) ⊑ E`: `A ⊑ E` only where `r` is functional (the two
/// successors are one).
#[test]
fn two_successors_of_a_functional_property_are_one() {
    for functional in [false, true] {
        let mut table = Table::default();
        let mut b = Build::new(&mut table);
        let (a, bb, c, e) = (b.c("A"), b.c("B"), b.c("C"), b.c("E"));
        let r = b.r("r");
        let (rb, rc) = (b.some(r, bb), b.some(r, c));
        let both = b.and(&[rb, rc]);
        b.sub(a, both);
        let bc = b.and(&[bb, c]);
        let r_bc = b.some(r, bc);
        b.sub(r_bc, e);
        if functional {
            b.axiom(Axiom::ObjectCharacteristic(Characteristic::Functional, r));
        }
        let t = classify(&b.done(), 1).expect("Horn with equality");
        assert_eq!(holds(&mut table, &t, "A", "E"), functional);
    }
}

/// `A ⊑ ∃r⁻.B`, `B ⊑ ∃r.C`, `r` functional: the `B`'s one `r`-successor is the `A`, so
/// `A ⊑ C` (a successor merged with the predecessor, the merge sent back up by Pred).
#[test]
fn a_successor_and_the_predecessor_are_one() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, bb, c) = (b.c("A"), b.c("B"), b.c("C"));
    let r = b.r("r");
    let ObjProp::Named(rt) = r else {
        unreachable!()
    };
    let up = b.some(ObjProp::Inverse(rt), bb);
    b.sub(a, up);
    let down = b.some(r, c);
    b.sub(bb, down);
    b.axiom(Axiom::ObjectCharacteristic(Characteristic::Functional, r));
    let t = classify(&b.done(), 1).expect("Horn with equality");
    assert!(holds(&mut table, &t, "A", "C"));
    assert!(!holds(&mut table, &t, "B", "A"));
}

/// `A ⊑ ∃r.B`, `B ⊑ ∃r⁻.C`, `r` inverse-functional: the `B` has two `r`-predecessors, the
/// `A` and a `C`, so `A ⊑ C`.
#[test]
fn an_inverse_functional_property_merges_predecessors() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, bb, c) = (b.c("A"), b.c("B"), b.c("C"));
    let r = b.r("r");
    let ObjProp::Named(rt) = r else {
        unreachable!()
    };
    let down = b.some(r, bb);
    b.sub(a, down);
    let up = b.some(ObjProp::Inverse(rt), c);
    b.sub(bb, up);
    b.axiom(Axiom::ObjectCharacteristic(
        Characteristic::InverseFunctional,
        r,
    ));
    let t = classify(&b.done(), 1).expect("Horn with equality");
    assert!(holds(&mut table, &t, "A", "C"));
}

/// `G ⊑ ≤ 1 r.⊤`: only a `G`'s successors are merged.
#[test]
fn a_guarded_at_most_one_merges_under_its_guard() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, x, g, bb, c, e) = (b.c("A"), b.c("X"), b.c("G"), b.c("B"), b.c("C"), b.c("E"));
    let r = b.r("r");
    let top = b.e(ClassExpr::Thing);
    let at_most = b.e(ClassExpr::Max(1, r, top));
    b.sub(g, at_most);
    let (rb, rc) = (b.some(r, bb), b.some(r, c));
    let both = b.and(&[rb, rc]);
    b.sub(a, both);
    b.sub(x, both);
    b.sub(a, g);
    let bc = b.and(&[bb, c]);
    let r_bc = b.some(r, bc);
    b.sub(r_bc, e);
    let t = classify(&b.done(), 1).expect("Horn with equality");
    assert!(holds(&mut table, &t, "A", "E"));
    assert!(!holds(&mut table, &t, "X", "E"));
}

/// What stays refused: `≥ 2` below a functional property (one successor would be
/// merged with itself), and `x` merged with a neighbour (`∃r.Self` with a successor).
#[test]
fn what_the_eq_rule_leaves_out() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, bb) = (b.c("A"), b.c("B"));
    let (r, s) = (b.r("r"), b.r("s"));
    let two = b.e(ClassExpr::Min(2, s, bb));
    b.sub(a, two);
    b.axiom(Axiom::SubObjectPropertyOf(vec![s], r));
    b.axiom(Axiom::ObjectCharacteristic(Characteristic::Functional, r));
    assert!(matches!(
        classify(&b.done(), 1),
        Err(Unsupported::Equality { .. })
    ));

    let mut b = Build::new(&mut table);
    let (a, bb) = (b.c("A"), b.c("B"));
    let r = b.r("r");
    let own = b.e(ClassExpr::HasSelf(r));
    let rb = b.some(r, bb);
    let both = b.and(&[own, rb]);
    b.sub(a, both);
    b.axiom(Axiom::ObjectCharacteristic(Characteristic::Functional, r));
    assert!(matches!(
        classify(&b.done(), 1),
        Err(Unsupported::Equality { .. })
    ));
}
