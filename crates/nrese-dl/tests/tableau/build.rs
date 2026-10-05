//! Ontologies by hand, in the structural model (terms are plain numbers).

use nrese_owl::{
    Axiom, Characteristic, ClassExpr, DataRange, ExprId, Literal, ObjProp, Ontology, RangeId, Term,
};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

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

    /// A fresh term (above the hand-picked ones), with an IRI if `iri` is given.
    fn term(&mut self, iri: Option<String>) -> Term {
        let t = 1_000_000 + (self.o.data.literals.len() + self.o.data.iris.len()) as Term;
        if let Some(iri) = iri {
            self.o.data.iris.insert(t, iri);
        }
        t
    }

    /// A literal `"lexical"^^xsd:datatype`.
    pub fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        let t = self.term(None);
        self.o.data.literals.insert(
            t,
            Literal {
                lexical: lexical.to_owned(),
                datatype: Some(format!("{XSD}{datatype}")),
                language: None,
            },
        );
        t
    }

    pub fn range(&mut self, r: DataRange) -> RangeId {
        RangeId(self.o.ranges.intern(r))
    }

    /// `xsd:datatype`.
    pub fn datatype(&mut self, datatype: &str) -> RangeId {
        let t = self.term(Some(format!("{XSD}{datatype}")));
        self.range(DataRange::Datatype(t))
    }

    /// `xsd:datatype[facet value, …]`, each value an `xsd:integer`.
    pub fn restricted(&mut self, datatype: &str, facets: &[(&str, i64)]) -> RangeId {
        let t = self.term(Some(format!("{XSD}{datatype}")));
        let mut pairs = Vec::new();
        for &(facet, value) in facets {
            let f = self.term(Some(format!("{XSD}{facet}")));
            let v = self.literal(&value.to_string(), "integer");
            pairs.push((f, v));
        }
        pairs.sort();
        self.range(DataRange::Restriction(t, pairs))
    }
}
