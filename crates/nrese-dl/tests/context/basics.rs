//! Hand-written ontologies with known taxonomies: each Horn rule and each compilation step
//! at least once, and the cases the Horn stage gives up.

use nrese_dl::context::Unsupported;
use nrese_owl::{Axiom, Characteristic, ClassExpr, ObjProp};

use super::support::{Build, Table, classify, holds, unsat};

/// The textbook EL case (Baader et al.): pericarditis is a heart disease, through an
/// existential, a conjunction and a role chain.
#[test]
fn pericarditis_is_a_heart_disease() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (tissue, heart, inflammation, disease) = (
        b.c("Tissue"),
        b.c("Heart"),
        b.c("Inflammation"),
        b.c("Disease"),
    );
    let (pericardium, pericarditis, heart_disease) =
        (b.c("Pericardium"), b.c("Pericarditis"), b.c("HeartDisease"));
    let (part_of, location) = (b.r("partOf"), b.r("hasLocation"));
    let part_of_heart = b.some(part_of, heart);
    let p = b.and(&[tissue, part_of_heart]);
    b.sub(pericardium, p);
    let at_pericardium = b.some(location, pericardium);
    let p = b.and(&[inflammation, at_pericardium]);
    b.axiom(Axiom::EquivalentClasses(vec![pericarditis, p]));
    b.sub(inflammation, disease);
    let at_heart = b.some(location, heart);
    let h = b.and(&[disease, at_heart]);
    b.axiom(Axiom::EquivalentClasses(vec![heart_disease, h]));
    b.axiom(Axiom::SubObjectPropertyOf(
        vec![location, part_of],
        location,
    ));
    let o = b.done();
    let c = classify(&o, 1).expect("Horn");
    assert!(holds(&mut table, &c, "Pericarditis", "HeartDisease"));
    assert!(holds(&mut table, &c, "Pericarditis", "Disease"));
    assert!(holds(&mut table, &c, "Pericardium", "Tissue"));
    assert!(!holds(&mut table, &c, "HeartDisease", "Pericarditis"));
    assert!(c.unsatisfiable.is_empty() && c.consistent);
}

/// Inverse roles and universals: what a successor knows travels back through Pred.
#[test]
fn universals_over_inverses_reach_the_predecessor() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, bb, cc, d) = (b.c("A"), b.c("B"), b.c("C"), b.c("D"));
    let r = b.r("r");
    // A ⊑ ∃r.B, B ⊑ ∀r⁻.C: an A is a C.
    let some = b.some(r, bb);
    b.sub(a, some);
    let back = b.all(ObjProp::Inverse(r.named()), cc);
    b.sub(bb, back);
    // D ⊑ ∀r.⊥ ⊓ … and D ⊑ A: D is unsatisfiable.
    let nothing = b.e(ClassExpr::Nothing);
    let none = b.all(r, nothing);
    b.sub(d, none);
    b.sub(d, a);
    let o = b.done();
    let c = classify(&o, 1).expect("Horn");
    assert!(holds(&mut table, &c, "A", "C"));
    assert!(!holds(&mut table, &c, "B", "C"));
    assert!(unsat(&mut table, &c, "D"));
    assert!(!unsat(&mut table, &c, "A"));
}

/// Domains, ranges, role hierarchies and transitivity (the automata, renamed Horn).
#[test]
fn roles_domains_ranges_and_transitivity() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (person, parent, elder, child, grand, desc) = (
        b.c("Person"),
        b.c("Parent"),
        b.c("Elder"),
        b.c("Child"),
        b.c("GrandChild"),
        b.c("PersonDescendant"),
    );
    let (has_parent, has_ancestor) = (b.r("hasParent"), b.r("hasAncestor"));
    b.axiom(Axiom::SubObjectPropertyOf(vec![has_parent], has_ancestor));
    b.axiom(Axiom::ObjectCharacteristic(
        Characteristic::Transitive,
        has_ancestor,
    ));
    b.axiom(Axiom::ObjectPropertyDomain(has_parent, person));
    b.axiom(Axiom::ObjectPropertyRange(has_parent, person));
    let p = b.some(has_parent, parent);
    b.sub(child, p);
    let e = b.some(has_parent, elder);
    b.sub(parent, e);
    let inner = b.some(has_ancestor, elder);
    let outer = b.some(has_ancestor, inner);
    b.axiom(Axiom::EquivalentClasses(vec![grand, outer]));
    let pp = b.some(has_parent, person);
    b.axiom(Axiom::EquivalentClasses(vec![desc, pp]));
    // Transitivity alone: an ancestor of an ancestor of an elder.
    let far = b.c("FarFromElder");
    let elder_anc = b.some(has_ancestor, elder);
    b.axiom(Axiom::EquivalentClasses(vec![far, elder_anc]));
    let o = b.done();
    let c = classify(&o, 1).expect("Horn after renaming");
    assert!(holds(&mut table, &c, "Child", "Person"));
    assert!(holds(&mut table, &c, "Child", "PersonDescendant"));
    assert!(holds(&mut table, &c, "Child", "GrandChild"));
    assert!(!holds(&mut table, &c, "Parent", "GrandChild"));
    assert!(holds(&mut table, &c, "Child", "FarFromElder"));
    assert!(holds(&mut table, &c, "GrandChild", "FarFromElder"));
}

/// `∃r.(A ⊓ B) ⊑ C` needs the renaming; `⊥` travels back along existentials.
#[test]
fn conjunctive_fillers_and_bottom_propagation() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, bb, cc, x, y, z) = (b.c("A"), b.c("B"), b.c("C"), b.c("X"), b.c("Y"), b.c("Z"));
    let r = b.r("r");
    let ab = b.and(&[a, bb]);
    let some = b.some(r, ab);
    b.sub(some, cc);
    let (sa, sb) = (b.some(r, a), b.some(r, bb));
    let both = b.and(&[sa, sb]);
    b.sub(x, both);
    let filler = b.and(&[a, bb]);
    let s = b.some(r, filler);
    b.sub(y, s);
    // Z ⊑ ∃r.(A ⊓ ¬A)
    let not_a = b.not(a);
    let clash = b.and(&[a, not_a]);
    let s = b.some(r, clash);
    b.sub(z, s);
    let o = b.done();
    let c = classify(&o, 1).expect("Horn after renaming");
    assert!(holds(&mut table, &c, "Y", "C"));
    assert!(!holds(&mut table, &c, "X", "C"), "two successors, not one");
    assert!(unsat(&mut table, &c, "Z"));
}

/// Classes equivalent to `owl:Thing`, and an inconsistent TBox and ABox.
#[test]
fn top_and_inconsistency() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, t) = (b.c("A"), b.c("T"));
    let thing = b.e(ClassExpr::Thing);
    b.sub(thing, t);
    let o = b.done();
    let c = classify(&o, 1).expect("Horn");
    let t_id = table.iri_id(&format!("{}T", super::support::EX));
    assert_eq!(c.top, vec![t_id]);
    assert!(holds(&mut table, &c, "A", "T"));
    let _ = a;

    let mut b = Build::new(&mut table);
    let (a, bot) = (b.c("A"), b.e(ClassExpr::Nothing));
    let thing = b.e(ClassExpr::Thing);
    b.sub(thing, a);
    b.sub(a, bot);
    let c = classify(&b.done(), 1).expect("Horn");
    assert!(!c.consistent && c.unsatisfiable.len() == c.classes.len());

    let mut b = Build::new(&mut table);
    let (a, bb, cc) = (b.c("A"), b.c("B"), b.c("C"));
    let ab = b.and(&[a, bb]);
    let bot = b.e(ClassExpr::Nothing);
    b.sub(ab, bot);
    let ind = b.t("i");
    b.axiom(Axiom::ClassAssertion(a, ind));
    b.axiom(Axiom::ClassAssertion(cc, ind));
    let consistent = classify(&b.o.clone(), 1).expect("Horn");
    assert!(consistent.consistent);
    b.axiom(Axiom::ClassAssertion(bb, ind));
    let c = classify(&b.done(), 1).expect("Horn");
    assert!(!c.consistent);
}

/// Fuzz case 8414 of the EL gate, where the EL classifier (ELK's handling of ranges,
/// complete only under the EL profile's range restriction) answers satisfiable. By hand:
/// for `c ∈ C5`, `c p0 a` with `a ∈ C5` (range), `a p2 y`, so `c p3 y` (chain) and
/// `y ∈ C1` (range): `a ∈ ∃p0.⊤ ⊓ ∃p2.C1`, which the disjointness forbids.
#[test]
fn a_range_on_a_chains_superproperty() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (c1, c5) = (b.c("C1"), b.c("C5"));
    let (p0, p1, p2, p3) = (b.r("p0"), b.r("p1"), b.r("p2"), b.r("p3"));
    let some = b.some(p0, c1);
    b.sub(c5, some);
    let inner = b.some(p1, some);
    let outer = b.some(p2, inner);
    b.axiom(Axiom::EquivalentClasses(vec![outer, c5]));
    let thing = b.e(ClassExpr::Thing);
    let (any_p0, p2_c1) = (b.some(p0, thing), b.some(p2, c1));
    b.axiom(Axiom::DisjointClasses(vec![any_p0, p2_c1]));
    b.axiom(Axiom::SubObjectPropertyOf(vec![p0, p2], p3));
    b.axiom(Axiom::ObjectPropertyRange(p0, c5));
    b.axiom(Axiom::ObjectPropertyRange(p3, c1));
    let c = classify(&b.done(), 1).expect("Horn");
    assert!(unsat(&mut table, &c, "C5"));
    assert!(!unsat(&mut table, &c, "C1"));
}

#[test]
fn what_the_horn_stage_gives_up() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let (a, bb, cc) = (b.c("A"), b.c("B"), b.c("C"));
    let or = b.or(&[bb, cc]);
    b.sub(a, or);
    assert!(matches!(
        classify(&b.done(), 1),
        Err(Unsupported::NotHorn { .. })
    ));

    let mut b = Build::new(&mut table);
    let r = b.r("r");
    b.axiom(Axiom::ObjectCharacteristic(Characteristic::Functional, r));
    assert!(matches!(
        classify(&b.done(), 1),
        Err(Unsupported::Equality { .. })
    ));

    let mut b = Build::new(&mut table);
    let a = b.c("A");
    let i = b.t("i");
    let one = b.e(ClassExpr::OneOf(vec![i]));
    b.sub(a, one);
    assert!(matches!(
        classify(&b.done(), 1),
        Err(Unsupported::Nominals { .. })
    ));
}

/// The sequential and parallel saturations agree, on every rule.
#[test]
fn parallel_equals_sequential_on_the_examples() {
    let mut table = Table::default();
    let mut b = Build::new(&mut table);
    let classes: Vec<_> = (0..30).map(|i| b.c(&format!("K{i}"))).collect();
    let r = b.r("r");
    let s = b.r("s");
    b.axiom(Axiom::ObjectCharacteristic(Characteristic::Transitive, s));
    b.axiom(Axiom::SubObjectPropertyOf(vec![r], s));
    for i in 0..29 {
        let some = b.some(r, classes[i + 1]);
        b.sub(classes[i], some);
        if i % 3 == 0 {
            let all = b.all(ObjProp::Inverse(r.named()), classes[(i + 7) % 30]);
            b.sub(classes[i + 1], all);
        }
        if i % 5 == 0 {
            let back = b.some(s, classes[29]);
            b.sub(back, classes[i]);
        }
    }
    let o = b.done();
    let one = classify(&o, 1).expect("Horn");
    for threads in [2, 4, 8] {
        assert_eq!(
            classify(&o, threads).expect("Horn"),
            one,
            "{threads} threads"
        );
    }
    assert!(!one.subsumptions.is_empty());
}
