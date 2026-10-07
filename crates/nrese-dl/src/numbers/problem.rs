//! Layer 1 of the number module (docs/design/owl2-dl.md#number-reasoning-layers):
//! the ontology read as a [`NumberProblem`], within the fragment the layers above it
//! support. Every axiom outside it is kept in [`NumberProblem::outside`] with the reason:
//! the problem then still holds the rest, which is enough to refute the ontology (a subset
//! of its axioms), but never to claim a model of it.
//!
//! The fragment: singleton nominals; named and `⊤` classes; `∃R.B` with `B` named or `⊤`;
//! unqualified `≥ n R`, `≤ n R` and `= n R`; inclusions and equivalences between these;
//! inverse pairs, functional and inverse-functional properties, domains and ranges (named
//! or `⊤`); class assertions of these expressions. Anything else (chains, transitivity and
//! the other characteristics, sub- and equivalent properties, `Self`, keys, qualified
//! cardinalities, unions, negation, data, property assertions, equality and inequality of
//! individuals) is outside, and so are axioms the reader left out.

use hashbrown::{HashMap, HashSet};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology, Term};

/// A property in a direction: the property and whether it is read inverted, named inverse
/// pairs folded onto the smaller term.
pub type Dir = (Term, bool);

/// A class expression of the fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Expr {
    Thing,
    Class(Term),
    /// `{a}`.
    Nominal(Term),
    /// `∃R.B`; `None` for `⊤`.
    Some(Dir, Option<Term>),
    /// `≥ n R.⊤`, `≤ n R.⊤` and `= n R.⊤`.
    AtLeast(u32, Dir),
    AtMost(u32, Dir),
    Exact(u32, Dir),
}

/// `sub ⊑ sup`, from the axiom at `axiom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inclusion {
    pub sub: Expr,
    pub sup: Expr,
    pub axiom: usize,
}

/// The ontology as the number module reads it.
#[derive(Debug, Default)]
pub struct NumberProblem {
    pub inclusions: Vec<Inclusion>,
    /// Directions with at most one neighbour per element.
    pub functional: HashSet<Dir>,
    /// Elements with a neighbour in the direction are in the class (`None`: `⊤`):
    /// domains, and ranges as domains of the inverse.
    pub domains: Vec<(Dir, Option<Term>, usize)>,
    /// `a : C`.
    pub assertions: Vec<(Expr, Term, usize)>,
    /// Named inverse pairs, both ways.
    pub inverse: HashMap<Term, Term>,
    /// The axioms outside the fragment (`usize::MAX`: axioms the reader left out), and why.
    pub outside: Vec<(usize, &'static str)>,
}

impl NumberProblem {
    /// Whether every axiom of the ontology is in the problem: only then may a model of the
    /// problem be one of the ontology.
    pub fn complete(&self) -> bool {
        self.outside.is_empty()
    }

    /// A property expression as a direction.
    pub fn dir(&self, r: ObjProp) -> Dir {
        let (t, inverted) = match r {
            ObjProp::Named(t) => (t, false),
            ObjProp::Inverse(t) => (t, true),
        };
        match self.inverse.get(&t) {
            Some(&u) if u < t => (u, !inverted),
            _ => (t, inverted),
        }
    }

    /// The expression `e` in the fragment, if it is.
    fn expr(&self, o: &Ontology, e: ExprId) -> Option<Expr> {
        let thing = |f: ExprId| matches!(o.classes.get(f.0), ClassExpr::Thing);
        Some(match *o.classes.get(e.0) {
            ClassExpr::Thing => Expr::Thing,
            ClassExpr::Class(a) => Expr::Class(a),
            ClassExpr::OneOf(ref xs) if xs.len() == 1 => Expr::Nominal(xs[0]),
            ClassExpr::Some(r, f) => match *o.classes.get(f.0) {
                ClassExpr::Thing => Expr::Some(self.dir(r), None),
                ClassExpr::Class(b) => Expr::Some(self.dir(r), Some(b)),
                _ => return None,
            },
            ClassExpr::Min(1, r, f) if !thing(f) => match *o.classes.get(f.0) {
                ClassExpr::Class(b) => Expr::Some(self.dir(r), Some(b)),
                _ => return None,
            },
            ClassExpr::Min(n, r, f) if thing(f) => Expr::AtLeast(n, self.dir(r)),
            ClassExpr::Max(n, r, f) if thing(f) => Expr::AtMost(n, self.dir(r)),
            ClassExpr::Exact(n, r, f) if thing(f) => Expr::Exact(n, self.dir(r)),
            _ => return None,
        })
    }
}

/// `ontology` as a [`NumberProblem`].
pub fn extract(o: &Ontology) -> NumberProblem {
    let mut p = NumberProblem::default();
    if o.diagnostics.iter().any(|d| d.is_fatal()) {
        p.outside.push((usize::MAX, "axioms the reader left out"));
    }
    // The inverse pairs first: every direction is folded by them.
    for axiom in &o.axioms {
        if let Axiom::InverseObjectProperties(ObjProp::Named(a), ObjProp::Named(b)) = *axiom
            && a != b
        {
            p.inverse.insert(a, b);
            p.inverse.insert(b, a);
        }
    }
    let builtin = [
        o.builtin.top_object,
        o.builtin.bottom_object,
        o.builtin.top_data,
        o.builtin.bottom_data,
    ];
    for (i, axiom) in o.axioms.iter().enumerate() {
        if builtin
            .iter()
            .flatten()
            .any(|&t| axiom_mentions_property(axiom, t))
        {
            p.outside.push((i, "a built-in property"));
            continue;
        }
        let out = |p: &mut NumberProblem, why| p.outside.push((i, why));
        match axiom {
            Axiom::Declaration(..) => {}
            Axiom::InverseObjectProperties(ObjProp::Named(a), ObjProp::Named(b)) if a != b => {}
            Axiom::SubClassOf(a, b) => match (p.expr(o, *a), p.expr(o, *b)) {
                (Some(sub), Some(sup)) => p.inclusions.push(Inclusion { sub, sup, axiom: i }),
                _ => out(&mut p, "a class expression outside the fragment"),
            },
            Axiom::EquivalentClasses(xs) => {
                let es: Option<Vec<Expr>> = xs.iter().map(|&x| p.expr(o, x)).collect();
                match es {
                    Some(es) => {
                        for pair in es.windows(2) {
                            p.inclusions.push(Inclusion {
                                sub: pair[0],
                                sup: pair[1],
                                axiom: i,
                            });
                            p.inclusions.push(Inclusion {
                                sub: pair[1],
                                sup: pair[0],
                                axiom: i,
                            });
                        }
                    }
                    None => out(&mut p, "a class expression outside the fragment"),
                }
            }
            Axiom::ObjectCharacteristic(Characteristic::Functional, r) => {
                let d = p.dir(*r);
                p.functional.insert(d);
            }
            Axiom::ObjectCharacteristic(Characteristic::InverseFunctional, r) => {
                let d = p.dir(r.inverse());
                p.functional.insert(d);
            }
            Axiom::ObjectPropertyDomain(r, c) | Axiom::ObjectPropertyRange(r, c) => {
                let d = match axiom {
                    Axiom::ObjectPropertyDomain(..) => p.dir(*r),
                    _ => p.dir(r.inverse()),
                };
                match o.classes.get(c.0) {
                    ClassExpr::Thing => p.domains.push((d, None, i)),
                    ClassExpr::Class(a) => p.domains.push((d, Some(*a), i)),
                    _ => out(&mut p, "a domain or range outside the fragment"),
                }
            }
            Axiom::ClassAssertion(c, a) => match p.expr(o, *c) {
                Some(e) => p.assertions.push((e, *a, i)),
                None => out(&mut p, "a class expression outside the fragment"),
            },
            _ => out(&mut p, "an axiom kind outside the fragment"),
        }
    }
    p
}

/// Whether `axiom` uses the property `t` (the built-in ones are outside the fragment).
fn axiom_mentions_property(axiom: &Axiom, t: Term) -> bool {
    let is = |r: &ObjProp| r.named() == t;
    match axiom {
        Axiom::ObjectCharacteristic(_, r)
        | Axiom::ObjectPropertyDomain(r, _)
        | Axiom::ObjectPropertyRange(r, _) => is(r),
        Axiom::InverseObjectProperties(a, b) => is(a) || is(b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_owl::DataRange;

    /// DL-906's shape at `(n, m, k)`: `O ≡ {d} ≡ (= n p⁻) ≡ (= k r⁻)`, `N ≡ ∃p.O ≡ (= m q⁻)`,
    /// `NM ≡ ∃q.N ≡ ∃r.O`, `p`, `q`, `r` functional with domains and ranges, `d : ⊤`.
    pub(crate) fn product(n: u32, m: u32, k: u32) -> Ontology {
        let mut o = Ontology::default();
        let (only_d, card_n, card_nm, d) = (1, 2, 3, 100);
        let (p, inv_p, q, inv_q, r, inv_r) = (10, 11, 12, 13, 14, 15);
        let e = |o: &mut Ontology, x: ClassExpr| ExprId(o.classes.intern(x));
        let thing = e(&mut o, ClassExpr::Thing);
        let [od, cn, cnm] = [only_d, card_n, card_nm].map(|c| e(&mut o, ClassExpr::Class(c)));
        let one = e(&mut o, ClassExpr::OneOf(vec![d]));
        let exact = |o: &mut Ontology, n: u32, r: Term| {
            ExprId(
                o.classes
                    .intern(ClassExpr::Exact(n, ObjProp::Named(r), thing)),
            )
        };
        let some = |o: &mut Ontology, r: Term, f: ExprId| {
            ExprId(o.classes.intern(ClassExpr::Some(ObjProp::Named(r), f)))
        };
        let (n_p, k_r, m_q) = (
            exact(&mut o, n, inv_p),
            exact(&mut o, k, inv_r),
            exact(&mut o, m, inv_q),
        );
        let (p_o, q_n, r_o) = (
            some(&mut o, p, od),
            some(&mut o, q, cn),
            some(&mut o, r, od),
        );
        let mut axioms = vec![
            Axiom::EquivalentClasses(vec![od, one]),
            Axiom::EquivalentClasses(vec![od, n_p]),
            Axiom::EquivalentClasses(vec![od, k_r]),
            Axiom::EquivalentClasses(vec![cn, p_o]),
            Axiom::EquivalentClasses(vec![cn, m_q]),
            Axiom::EquivalentClasses(vec![cnm, q_n]),
            Axiom::EquivalentClasses(vec![cnm, r_o]),
            Axiom::ClassAssertion(thing, d),
        ];
        for (f, inv, dom, ran) in [(p, inv_p, cn, od), (q, inv_q, cnm, cn), (r, inv_r, cnm, od)] {
            axioms.push(Axiom::ObjectCharacteristic(
                Characteristic::Functional,
                ObjProp::Named(f),
            ));
            axioms.push(Axiom::InverseObjectProperties(
                ObjProp::Named(f),
                ObjProp::Named(inv),
            ));
            axioms.push(Axiom::ObjectPropertyDomain(ObjProp::Named(f), dom));
            axioms.push(Axiom::ObjectPropertyRange(ObjProp::Named(f), ran));
        }
        o.sources = vec![Vec::new(); axioms.len()];
        o.axioms = axioms;
        o
    }

    /// DL-906's shape lies in the fragment: every axiom read, inverses folded.
    #[test]
    fn the_product_of_906_is_in_the_fragment() {
        let o = product(20, 30, 600);
        let p = extract(&o);
        assert!(p.complete(), "{:?}", p.outside);
        // `= 20 invP` is `= 20 p⁻`: one direction, the inverse folded onto `p`.
        assert!(
            p.inclusions
                .iter()
                .any(|i| i.sup == Expr::Exact(20, (10, true)))
        );
        assert_eq!(p.functional.len(), 3);
        assert_eq!(p.domains.len(), 6);
    }

    /// A mutant: what it adds, and how.
    type Mutant = (&'static str, Box<dyn Fn(&mut Ontology) -> Axiom>);

    /// Mutants: one axiom outside the fragment each, and the problem is no longer complete
    /// (the module must decline the model claim); the rest is still read.
    #[test]
    fn axioms_outside_the_fragment_are_declined() {
        let mutants: Vec<Mutant> = vec![
            (
                "transitive",
                Box::new(|_| {
                    Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(10))
                }),
            ),
            (
                "a subproperty",
                Box::new(|_| {
                    Axiom::SubObjectPropertyOf(vec![ObjProp::Named(10)], ObjProp::Named(14))
                }),
            ),
            (
                "a chain",
                Box::new(|_| {
                    Axiom::SubObjectPropertyOf(
                        vec![ObjProp::Named(12), ObjProp::Named(10)],
                        ObjProp::Named(14),
                    )
                }),
            ),
            (
                "a qualified cardinality",
                Box::new(|o| {
                    let c = ExprId(o.classes.intern(ClassExpr::Class(2)));
                    let q = ExprId(o.classes.intern(ClassExpr::Max(3, ObjProp::Named(13), c)));
                    let a = ExprId(o.classes.intern(ClassExpr::Class(2)));
                    Axiom::SubClassOf(a, q)
                }),
            ),
            (
                "Self",
                Box::new(|o| {
                    let s = ExprId(o.classes.intern(ClassExpr::HasSelf(ObjProp::Named(10))));
                    let a = ExprId(o.classes.intern(ClassExpr::Class(2)));
                    Axiom::SubClassOf(a, s)
                }),
            ),
            (
                "a union",
                Box::new(|o| {
                    let [a, b] = [2, 3].map(|c| ExprId(o.classes.intern(ClassExpr::Class(c))));
                    let u = ExprId(o.classes.intern(ClassExpr::Or(vec![a, b])));
                    Axiom::SubClassOf(a, u)
                }),
            ),
            (
                "a key",
                Box::new(|o| {
                    let a = ExprId(o.classes.intern(ClassExpr::Class(2)));
                    Axiom::HasKey(a, vec![ObjProp::Named(10)], Vec::new())
                }),
            ),
            (
                "disjoint properties",
                Box::new(|_| {
                    Axiom::DisjointObjectProperties(vec![ObjProp::Named(10), ObjProp::Named(12)])
                }),
            ),
            (
                "a negative assertion",
                Box::new(|_| Axiom::NegativeObjectPropertyAssertion(10, 101, 100)),
            ),
            (
                "a property assertion",
                Box::new(|_| Axiom::ObjectPropertyAssertion(12, 101, 102)),
            ),
            (
                "equal individuals",
                Box::new(|_| Axiom::SameIndividual(vec![100, 101])),
            ),
            (
                "data",
                Box::new(|o| {
                    let r = nrese_owl::RangeId(o.ranges.intern(DataRange::Literal));
                    Axiom::DataPropertyRange(20, r)
                }),
            ),
        ];
        for (what, mutant) in mutants {
            let mut o = product(20, 30, 600);
            let a = mutant(&mut o);
            o.axioms.push(a);
            o.sources.push(Vec::new());
            let p = extract(&o);
            assert!(!p.complete(), "{what} was read as in the fragment");
            assert_eq!(p.outside.len(), 1, "{what}: {:?}", p.outside);
            assert!(p.inclusions.len() >= 14, "{what}: the rest is still read");
        }
    }
}
