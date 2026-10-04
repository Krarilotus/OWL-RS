//! Ontologies by hand, in the structural model (terms are plain numbers).

use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology, Term};

#[derive(Default)]
pub struct Build {
    pub o: Ontology,
}

impl Build {
    pub fn e(&mut self, expr: ClassExpr) -> ExprId {
        ExprId(self.o.classes.intern(expr))
    }

    pub fn class(&mut self, t: Term) -> ExprId {
        self.e(ClassExpr::Class(t))
    }

    pub fn not(&mut self, x: ExprId) -> ExprId {
        self.e(ClassExpr::Not(x))
    }

    fn sorted(xs: &[ExprId]) -> Vec<ExprId> {
        let mut v = xs.to_vec();
        v.sort();
        v.dedup();
        v
    }

    pub fn and(&mut self, xs: &[ExprId]) -> ExprId {
        self.e(ClassExpr::And(Self::sorted(xs)))
    }

    pub fn or(&mut self, xs: &[ExprId]) -> ExprId {
        self.e(ClassExpr::Or(Self::sorted(xs)))
    }

    pub fn some(&mut self, r: ObjProp, x: ExprId) -> ExprId {
        self.e(ClassExpr::Some(r, x))
    }

    pub fn all(&mut self, r: ObjProp, x: ExprId) -> ExprId {
        self.e(ClassExpr::All(r, x))
    }

    pub fn axiom(&mut self, a: Axiom) {
        self.o.axioms.push(a);
        self.o.sources.push(Vec::new());
    }

    pub fn sub(&mut self, a: ExprId, b: ExprId) {
        self.axiom(Axiom::SubClassOf(a, b));
    }

    pub fn equivalent(&mut self, a: ExprId, b: ExprId) {
        self.axiom(Axiom::EquivalentClasses(Self::sorted(&[a, b])));
    }

    pub fn disjoint(&mut self, a: ExprId, b: ExprId) {
        self.axiom(Axiom::DisjointClasses(Self::sorted(&[a, b])));
    }

    pub fn assert(&mut self, c: ExprId, a: Term) {
        self.axiom(Axiom::ClassAssertion(c, a));
    }

    pub fn role_assertion(&mut self, r: Term, a: Term, b: Term) {
        self.axiom(Axiom::ObjectPropertyAssertion(r, a, b));
    }

    pub fn characteristic(&mut self, c: Characteristic, r: ObjProp) {
        self.axiom(Axiom::ObjectCharacteristic(c, r));
    }

    pub fn same(&mut self, a: Term, b: Term) {
        self.axiom(Axiom::SameIndividual(vec![a.min(b), a.max(b)]));
    }

    pub fn different(&mut self, a: Term, b: Term) {
        self.axiom(Axiom::DifferentIndividuals(vec![a.min(b), a.max(b)]));
    }
}
