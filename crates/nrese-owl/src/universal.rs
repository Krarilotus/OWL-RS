//! The universal object property `owl:topObjectProperty` (U) through a hub: a fresh
//! individual `h` that every element reaches by a fresh role `u` (`⊤ ⊑ ∃u.{h}`). Then
//! `∀U.C ≡ ∀u.∀u⁻.C` and `∃U.C ≡ ∃u.∃u⁻.C`: from any element, `u` leads to `h` and `u⁻`
//! from `h` to every element. A model of the original ontology becomes one of the
//! encoding by `u := Δ × {h}` with `h` any element, and a model of the encoding is one of
//! the original as it is, so consistency and entailments over the original signature
//! are kept (the "nominal hub" form of SROIQ's universal-role elimination, Horrocks,
//! Kutz and Sattler, KR 2006, Lemma 8, where a reflexive, symmetric, transitive superrole
//! of every role plays the hub's part; the hub needs no role hierarchy and keeps `u`
//! simple).
//!
//! `u` and `h` reuse the term of `owl:topObjectProperty` (as a role and as an individual):
//! after the rewriting, nothing else means it.
//!
//! The other axioms over U: an assertion `U(a, b)` holds (left out); `¬U(a, b)` is
//! inconsistent; `R ⊑ U` and the characteristics U has (transitive, symmetric,
//! reflexive) hold; a domain or range of U holds of everything. What makes U a
//! subproperty, a cardinality over U, and the characteristics U lacks are reported
//! unsupported.

use crate::mapping::Ontology;
use crate::model::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Term};

/// What can't be encoded.
pub(crate) const UNSUPPORTED: &str = "the universal object property as a subproperty, in a cardinality, or with a characteristic it lacks";

/// The rewriting of an ontology that uses `owl:topObjectProperty`.
pub(crate) struct Rewritten {
    /// The ontology with U encoded; axioms keep their indexes (one left out becomes a
    /// declaration-free `⊤ ⊑ ⊤`).
    pub ontology: Ontology,
    /// The axioms that can't be encoded.
    pub unsupported: Vec<usize>,
    /// The axioms whose encoding goes through the hub, each a source of the hub axiom
    /// (none: no hub).
    pub uses: Vec<usize>,
    /// The hub's role and individual.
    pub hub: Term,
}

struct Rw<'a> {
    o: &'a mut Ontology,
    top: Term,
    /// The axiom being rewritten went through the hub.
    hubbed: bool,
}

impl Rw<'_> {
    fn e(&mut self, x: ClassExpr) -> ExprId {
        ExprId(self.o.classes.intern(x))
    }

    fn is_top(&self, r: ObjProp) -> bool {
        r.named() == self.top
    }

    /// `∃U.C` or `∀U.C` through the hub.
    fn through_hub(&mut self, some: bool, c: ExprId) -> ExprId {
        self.hubbed = true;
        let (u, back) = (ObjProp::Named(self.top), ObjProp::Inverse(self.top));
        if some {
            let inner = self.e(ClassExpr::Some(back, c));
            self.e(ClassExpr::Some(u, inner))
        } else {
            let inner = self.e(ClassExpr::All(back, c));
            self.e(ClassExpr::All(u, inner))
        }
    }

    /// `e` with U encoded; `None` where it can't be.
    fn expr(&mut self, e: ExprId) -> Option<ExprId> {
        let x = self.o.classes.get(e.0).clone();
        let list = |rw: &mut Self, xs: &[ExprId]| -> Option<Vec<ExprId>> {
            let mut out: Vec<ExprId> = xs.iter().map(|&x| rw.expr(x)).collect::<Option<_>>()?;
            out.sort();
            out.dedup();
            Some(out)
        };
        Some(match x {
            ClassExpr::And(xs) => {
                let xs = list(self, &xs)?;
                self.e(ClassExpr::And(xs))
            }
            ClassExpr::Or(xs) => {
                let xs = list(self, &xs)?;
                self.e(ClassExpr::Or(xs))
            }
            ClassExpr::Not(x) => {
                let x = self.expr(x)?;
                self.e(ClassExpr::Not(x))
            }
            ClassExpr::Some(r, c) | ClassExpr::All(r, c) => {
                let some = matches!(x, ClassExpr::Some(..));
                let c = self.expr(c)?;
                if self.is_top(r) {
                    self.through_hub(some, c)
                } else if some {
                    self.e(ClassExpr::Some(r, c))
                } else {
                    self.e(ClassExpr::All(r, c))
                }
            }
            // Every element is U-related to every individual and to itself.
            ClassExpr::HasValue(r, _) | ClassExpr::HasSelf(r) if self.is_top(r) => {
                self.e(ClassExpr::Thing)
            }
            ClassExpr::Min(_, r, _) | ClassExpr::Max(_, r, _) | ClassExpr::Exact(_, r, _)
                if self.is_top(r) =>
            {
                return None;
            }
            ClassExpr::Min(n, r, c) => {
                let c = self.expr(c)?;
                self.e(ClassExpr::Min(n, r, c))
            }
            ClassExpr::Max(n, r, c) => {
                let c = self.expr(c)?;
                self.e(ClassExpr::Max(n, r, c))
            }
            ClassExpr::Exact(n, r, c) => {
                let c = self.expr(c)?;
                self.e(ClassExpr::Exact(n, r, c))
            }
            ClassExpr::DataAll(..)
            | ClassExpr::DataSome(..)
            | ClassExpr::DataHasValue(..)
            | ClassExpr::DataMin(..)
            | ClassExpr::DataMax(..)
            | ClassExpr::DataExact(..)
            | ClassExpr::Class(_)
            | ClassExpr::Thing
            | ClassExpr::Nothing
            | ClassExpr::OneOf(_)
            | ClassExpr::HasValue(..)
            | ClassExpr::HasSelf(_) => e,
        })
    }

    fn exprs(&mut self, xs: &[ExprId]) -> Option<Vec<ExprId>> {
        let mut out: Vec<ExprId> = xs.iter().map(|&x| self.expr(x)).collect::<Option<_>>()?;
        out.sort();
        out.dedup();
        Some(out)
    }

    /// `axiom` with U encoded: `Some(None)` where it holds and is left out, `None` where
    /// it can't be encoded.
    fn axiom(&mut self, axiom: &Axiom) -> Option<Option<Axiom>> {
        let thing = self.e(ClassExpr::Thing);
        let nothing = self.e(ClassExpr::Nothing);
        let top_term = self.top;
        let top = |r: &ObjProp| r.named() == top_term;
        Some(Some(match axiom {
            Axiom::SubClassOf(a, b) => Axiom::SubClassOf(self.expr(*a)?, self.expr(*b)?),
            Axiom::EquivalentClasses(xs) => Axiom::EquivalentClasses(self.exprs(xs)?),
            Axiom::DisjointClasses(xs) => Axiom::DisjointClasses(self.exprs(xs)?),
            Axiom::DisjointUnion(c, xs) => Axiom::DisjointUnion(*c, self.exprs(xs)?),
            Axiom::ClassAssertion(c, a) => Axiom::ClassAssertion(self.expr(*c)?, *a),
            Axiom::DataPropertyDomain(d, c) => Axiom::DataPropertyDomain(*d, self.expr(*c)?),
            Axiom::HasKey(c, rs, ds) => {
                if rs.iter().any(top) {
                    return None;
                }
                Axiom::HasKey(self.expr(*c)?, rs.clone(), ds.clone())
            }
            Axiom::ObjectPropertyDomain(r, c) | Axiom::ObjectPropertyRange(r, c) => {
                let c = self.expr(*c)?;
                if top(r) {
                    Axiom::SubClassOf(thing, c)
                } else if matches!(axiom, Axiom::ObjectPropertyDomain(..)) {
                    Axiom::ObjectPropertyDomain(*r, c)
                } else {
                    Axiom::ObjectPropertyRange(*r, c)
                }
            }
            Axiom::ObjectPropertyAssertion(p, ..) if *p == self.top => return Some(None),
            Axiom::NegativeObjectPropertyAssertion(p, a, _) if *p == self.top => {
                Axiom::ClassAssertion(nothing, *a)
            }
            Axiom::SubObjectPropertyOf(chain, sup) => {
                if top(sup) {
                    return Some(None);
                }
                if chain.iter().any(top) {
                    return None;
                }
                axiom.clone()
            }
            Axiom::ObjectCharacteristic(kind, r) if top(r) => match kind {
                Characteristic::Transitive
                | Characteristic::Symmetric
                | Characteristic::Reflexive => return Some(None),
                _ => return None,
            },
            Axiom::EquivalentObjectProperties(rs) | Axiom::DisjointObjectProperties(rs)
                if rs.iter().any(top) =>
            {
                return None;
            }
            Axiom::InverseObjectProperties(a, b) if top(a) || top(b) => return None,
            other => other.clone(),
        }))
    }
}

/// `ontology` with `owl:topObjectProperty` (its term `top`) encoded through the hub.
pub(crate) fn rewrite(ontology: &Ontology, top: Term) -> Rewritten {
    let mut o = ontology.clone();
    let (mut unsupported, mut uses) = (Vec::new(), Vec::new());
    let mut rw = Rw {
        o: &mut o,
        top,
        hubbed: false,
    };
    let mut axioms = Vec::with_capacity(ontology.axioms.len());
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        if !crate::properties::mentions(ontology, axiom, &|t| t == top) {
            axioms.push(axiom.clone());
            continue;
        }
        let thing = rw.e(ClassExpr::Thing);
        rw.hubbed = false;
        let rewritten = rw.axiom(axiom);
        // Only an axiom that goes through the hub needs it: `R ⊑ U`, a domain of U and
        // the like hold or become plain axioms, and a hub there would only add a nominal
        // to every element (no class of such an ontology could be decided by
        // saturation alone).
        if rw.hubbed {
            uses.push(index);
        }
        axioms.push(match rewritten {
            Some(Some(a)) => a,
            Some(None) => Axiom::SubClassOf(thing, thing),
            None => {
                unsupported.push(index);
                Axiom::SubClassOf(thing, thing)
            }
        });
    }
    o.axioms = axioms;
    Rewritten {
        ontology: o,
        unsupported,
        uses,
        hub: top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard: `R ⊑ U` holds and is left out; with no axiom going through the hub, the
    /// clauses get no hub (a hub's nominal would reach every element: no class could be
    /// decided by saturation alone; ore_ont_2738).
    #[test]
    fn a_subproperty_of_the_universal_property_needs_no_hub() {
        let (r, top) = (1, 2);
        let mut o = Ontology::default();
        o.builtin.top_object = Some(top);
        o.axioms = vec![Axiom::SubObjectPropertyOf(
            vec![ObjProp::Named(r)],
            ObjProp::Named(top),
        )];
        o.sources = vec![Vec::new()];
        assert!(rewrite(&o, top).uses.is_empty());
        // `∀U.C` goes through it.
        let c = ExprId(o.classes.intern(ClassExpr::Class(3)));
        let all = ExprId(o.classes.intern(ClassExpr::All(ObjProp::Named(top), c)));
        let thing = ExprId(o.classes.intern(ClassExpr::Thing));
        o.axioms.push(Axiom::SubClassOf(thing, all));
        o.sources.push(Vec::new());
        assert_eq!(rewrite(&o, top).uses, vec![1]);
    }
}
