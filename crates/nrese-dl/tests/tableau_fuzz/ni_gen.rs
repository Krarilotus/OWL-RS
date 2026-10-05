//! Ontologies biased to the NI rule (JAIR 2009, §3.2.4): a nominal `o`, a role `p` into
//! it (`∃p.{o}` somewhere), an at-most restriction on `p⁻` at `o`, and existentials that
//! make chains of blockable nodes, each of which then has a `p`-edge into `o` and so is a
//! blockable non-successor neighbour of a root. Around that core, random axioms of the
//! same signature that make the at-most restriction bite or not: disjointness, universals
//! along both directions, functionality, unions, nominal choices, an ABox.
//!
//! Used by the `tableau_fuzz` test (a fixed set) and the `tableau_fuzz` example
//! (`--profile ni`, for long runs and differential runs on the reference reasoners).

use nrese_owl::fuzz::{Rng, Signature, Sizes};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology};

/// The signature's sizes: four classes, three simple roles, three individuals.
pub fn sizes() -> Sizes {
    Sizes {
        classes: 4,
        object_properties: 3,
        simple: 3,
        data_properties: 0,
        individuals: 3,
        literals: 0,
    }
}

struct G<'a> {
    o: Ontology,
    rng: &'a mut Rng,
}

impl G<'_> {
    fn e(&mut self, x: ClassExpr) -> ExprId {
        ExprId(self.o.classes.intern(x))
    }

    fn sorted(mut v: Vec<ExprId>) -> Vec<ExprId> {
        v.sort();
        v.dedup();
        v
    }

    fn and(&mut self, xs: Vec<ExprId>) -> ExprId {
        let xs = Self::sorted(xs);
        if xs.len() == 1 {
            return xs[0];
        }
        self.e(ClassExpr::And(xs))
    }

    fn or(&mut self, xs: Vec<ExprId>) -> ExprId {
        let xs = Self::sorted(xs);
        if xs.len() == 1 {
            return xs[0];
        }
        self.e(ClassExpr::Or(xs))
    }

    fn not(&mut self, x: ExprId) -> ExprId {
        self.e(ClassExpr::Not(x))
    }

    fn axiom(&mut self, a: Axiom) {
        self.o.axioms.push(a);
        self.o.sources.push(Vec::new());
    }

    fn sub(&mut self, a: ExprId, b: ExprId) {
        self.axiom(Axiom::SubClassOf(a, b));
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.rng.below(items.len() as u64) as usize]
    }
}

/// An ontology of the NI pattern over `sig` (of [`sizes`]).
pub fn ontology(rng: &mut Rng, sig: &Signature) -> Ontology {
    let mut g = G {
        o: Ontology::default(),
        rng,
    };
    g.o.axioms = sig.declarations();
    g.o.sources = vec![Vec::new(); g.o.axioms.len()];
    let classes: Vec<ExprId> = sig
        .classes
        .iter()
        .map(|&c| ExprId(g.o.classes.intern(ClassExpr::Class(c))))
        .collect();
    let [a, b, c, d] = [classes[0], classes[1], classes[2], classes[3]];
    let [r, p, s] = [0, 1, 2].map(|i| sig.object_properties[i]);
    let [o, i1, i2] = [0, 1, 2].map(|i| sig.individuals[i]);
    let thing = g.e(ClassExpr::Thing);
    let nominal = g.e(ClassExpr::OneOf(vec![o]));
    // The role into the nominal, either way round, and back.
    let into = if g.rng.one_in(2) {
        ObjProp::Named(p)
    } else {
        ObjProp::Inverse(p)
    };
    let back = into.inverse();
    let succ = if g.rng.one_in(4) {
        ObjProp::Inverse(r)
    } else {
        ObjProp::Named(r)
    };

    // Into the nominal: from everything, from A, or with a filler.
    let filler = if g.rng.one_in(3) {
        let x = g.pick(&[b, c]);
        g.and(vec![x, nominal])
    } else {
        nominal
    };
    let to_o = g.e(ClassExpr::Some(into, filler));
    let from = g.pick(&[thing, a, b]);
    g.sub(from, to_o);

    // The at-most restriction at the nominal.
    let n = 1 + g.rng.below(3) as u32;
    let f = g.pick(&[thing, thing, a, b]);
    match g.rng.below(4) {
        0 => {
            let max = g.e(ClassExpr::Max(n, back, f));
            g.sub(nominal, max);
        }
        1 => {
            let max = g.e(ClassExpr::Max(n, back, f));
            g.axiom(Axiom::ClassAssertion(max, o));
        }
        2 => g.axiom(Axiom::ObjectCharacteristic(
            Characteristic::Functional,
            back,
        )),
        _ => g.axiom(Axiom::ObjectCharacteristic(
            Characteristic::InverseFunctional,
            into,
        )),
    }

    // Chains of blockable nodes.
    for _ in 0..1 + g.rng.below(2) {
        let (x, y) = (g.pick(&[a, b]), g.pick(&[a, b, c]));
        let filler = if g.rng.one_in(3) {
            let z = g.pick(&[c, d]);
            g.or(vec![y, z])
        } else {
            y
        };
        let k = if g.rng.one_in(4) { 2 } else { 1 };
        let some = g.e(ClassExpr::Min(k, succ, filler));
        g.sub(x, some);
    }

    // The ABox: something starts a chain.
    g.axiom(Axiom::ClassAssertion(a, i1));
    if g.rng.one_in(2) {
        g.axiom(Axiom::ClassAssertion(b, i2));
    }
    if g.rng.one_in(3) {
        g.axiom(Axiom::ObjectPropertyAssertion(r, i1, i2));
    }
    if g.rng.one_in(3) {
        let pair = g.pick(&[[o, i1], [i1, i2], [o, i2]]);
        let mut pair = pair.to_vec();
        pair.sort_unstable();
        g.axiom(Axiom::DifferentIndividuals(pair));
    }

    // Random axioms around it.
    for _ in 0..1 + g.rng.below(4) {
        match g.rng.below(11) {
            0 => {
                let (x, y) = (g.pick(&[a, b, c]), g.pick(&[b, c, d]));
                if x != y {
                    g.axiom(Axiom::DisjointClasses(G::sorted(vec![x, y])));
                }
            }
            1 => {
                let (x, y) = (g.pick(&[a, b]), g.pick(&[a, b, c, d]));
                let ny = g.not(y);
                let all = g.e(ClassExpr::All(succ, ny));
                g.sub(x, all);
            }
            2 => {
                let (x, y) = (g.pick(&[a, b, c, d]), g.pick(&[a, c]));
                let all = g.e(ClassExpr::All(succ.inverse(), y));
                g.sub(x, all);
            }
            3 => {
                let y = g.pick(&[a, b, d]);
                let all = g.e(ClassExpr::All(back, y));
                g.sub(nominal, all);
            }
            4 => {
                let x = g.pick(&[a, b]);
                let yz = g.or(vec![c, d]);
                g.sub(x, yz);
            }
            5 => {
                let cd = g.and(vec![c, d]);
                let nothing = g.e(ClassExpr::Nothing);
                g.sub(cd, nothing);
            }
            6 => {
                let kind = g.pick(&[
                    Characteristic::Functional,
                    Characteristic::InverseFunctional,
                ]);
                g.axiom(Axiom::ObjectCharacteristic(kind, ObjProp::Named(r)));
            }
            7 => {
                let x = g.pick(&[a, b, c]);
                let two = g.e(ClassExpr::OneOf(vec![o, i1]));
                g.sub(x, two);
            }
            8 => {
                let x = g.pick(&[b, c]);
                let none = g.e(ClassExpr::Max(1, succ.inverse(), thing));
                g.sub(x, none);
            }
            9 => {
                let x = g.pick(&[a, b, c]);
                let side = g.e(ClassExpr::Some(ObjProp::Named(s), nominal));
                g.sub(x, side);
                let max = g.e(ClassExpr::Max(1, ObjProp::Inverse(s), thing));
                g.sub(nominal, max);
            }
            _ => {
                let x = g.pick(&[a, b]);
                let nx = g.not(x);
                let all = g.e(ClassExpr::All(back, nx));
                g.sub(nominal, all);
            }
        }
    }
    g.o
}
