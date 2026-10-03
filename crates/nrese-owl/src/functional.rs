//! Axioms in the OWL 2 Functional-Style Syntax (W3C *OWL 2 Structural Specification*),
//! terms named by the caller: for diagnostics, explanations and tests (two readings of
//! one ontology compare equal by their text, whatever ids they interned).

use crate::mapping::Ontology;
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, EntityKind, ExprId, ObjProp, RangeId, Term,
};

impl Ontology {
    /// `axiom` in the functional syntax; `name` writes a term (an IRI as `<…>`, a
    /// literal as `"…"^^<…>`).
    pub fn functional(&self, axiom: &Axiom, name: &dyn Fn(Term) -> String) -> String {
        let f = Functional {
            ontology: self,
            name,
        };
        f.axiom(axiom)
    }
}

struct Functional<'a> {
    ontology: &'a Ontology,
    name: &'a dyn Fn(Term) -> String,
}

impl Functional<'_> {
    fn n(&self, term: Term) -> String {
        (self.name)(term)
    }

    fn terms(&self, terms: &[Term]) -> String {
        terms
            .iter()
            .map(|&t| self.n(t))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn p(&self, property: ObjProp) -> String {
        match property {
            ObjProp::Named(p) => self.n(p),
            ObjProp::Inverse(p) => format!("ObjectInverseOf({})", self.n(p)),
        }
    }

    fn ps(&self, properties: &[ObjProp]) -> String {
        properties
            .iter()
            .map(|&p| self.p(p))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn c(&self, id: ExprId) -> String {
        match self.ontology.class(id) {
            ClassExpr::Class(t) => self.n(*t),
            ClassExpr::Thing => "owl:Thing".to_owned(),
            ClassExpr::Nothing => "owl:Nothing".to_owned(),
            ClassExpr::And(xs) => format!("ObjectIntersectionOf({})", self.cs(xs)),
            ClassExpr::Or(xs) => format!("ObjectUnionOf({})", self.cs(xs)),
            ClassExpr::Not(x) => format!("ObjectComplementOf({})", self.c(*x)),
            ClassExpr::OneOf(xs) => format!("ObjectOneOf({})", self.terms(xs)),
            ClassExpr::Some(p, x) => format!("ObjectSomeValuesFrom({} {})", self.p(*p), self.c(*x)),
            ClassExpr::All(p, x) => format!("ObjectAllValuesFrom({} {})", self.p(*p), self.c(*x)),
            ClassExpr::HasValue(p, a) => format!("ObjectHasValue({} {})", self.p(*p), self.n(*a)),
            ClassExpr::HasSelf(p) => format!("ObjectHasSelf({})", self.p(*p)),
            ClassExpr::Min(n, p, x) => {
                format!("ObjectMinCardinality({n} {} {})", self.p(*p), self.c(*x))
            }
            ClassExpr::Max(n, p, x) => {
                format!("ObjectMaxCardinality({n} {} {})", self.p(*p), self.c(*x))
            }
            ClassExpr::Exact(n, p, x) => {
                format!("ObjectExactCardinality({n} {} {})", self.p(*p), self.c(*x))
            }
            ClassExpr::DataSome(p, r) => {
                format!("DataSomeValuesFrom({} {})", self.n(*p), self.r(*r))
            }
            ClassExpr::DataAll(p, r) => format!("DataAllValuesFrom({} {})", self.n(*p), self.r(*r)),
            ClassExpr::DataHasValue(p, v) => format!("DataHasValue({} {})", self.n(*p), self.n(*v)),
            ClassExpr::DataMin(n, p, r) => {
                format!("DataMinCardinality({n} {} {})", self.n(*p), self.r(*r))
            }
            ClassExpr::DataMax(n, p, r) => {
                format!("DataMaxCardinality({n} {} {})", self.n(*p), self.r(*r))
            }
            ClassExpr::DataExact(n, p, r) => {
                format!("DataExactCardinality({n} {} {})", self.n(*p), self.r(*r))
            }
        }
    }

    /// Operands in a stable order: by their text (ids differ between readings).
    fn cs(&self, xs: &[ExprId]) -> String {
        let mut parts: Vec<String> = xs.iter().map(|&x| self.c(x)).collect();
        parts.sort();
        parts.join(" ")
    }

    fn r(&self, id: RangeId) -> String {
        match self.ontology.range(id) {
            DataRange::Datatype(t) => self.n(*t),
            DataRange::And(xs) => format!("DataIntersectionOf({})", self.rs(xs)),
            DataRange::Or(xs) => format!("DataUnionOf({})", self.rs(xs)),
            DataRange::Not(x) => format!("DataComplementOf({})", self.r(*x)),
            DataRange::OneOf(xs) => format!("DataOneOf({})", self.terms(xs)),
            DataRange::Restriction(d, facets) => format!(
                "DatatypeRestriction({} {})",
                self.n(*d),
                facets
                    .iter()
                    .map(|&(f, v)| format!("{} {}", self.n(f), self.n(v)))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        }
    }

    fn rs(&self, xs: &[RangeId]) -> String {
        let mut parts: Vec<String> = xs.iter().map(|&x| self.r(x)).collect();
        parts.sort();
        parts.join(" ")
    }

    fn axiom(&self, axiom: &Axiom) -> String {
        match axiom {
            Axiom::Declaration(kind, t) => {
                let kind = match kind {
                    EntityKind::Class => "Class",
                    EntityKind::ObjectProperty => "ObjectProperty",
                    EntityKind::DataProperty => "DataProperty",
                    EntityKind::AnnotationProperty => "AnnotationProperty",
                    EntityKind::Datatype => "Datatype",
                    EntityKind::NamedIndividual => "NamedIndividual",
                };
                format!("Declaration({kind}({}))", self.n(*t))
            }
            Axiom::SubClassOf(a, b) => format!("SubClassOf({} {})", self.c(*a), self.c(*b)),
            Axiom::EquivalentClasses(xs) => format!("EquivalentClasses({})", self.cs(xs)),
            Axiom::DisjointClasses(xs) => format!("DisjointClasses({})", self.cs(xs)),
            Axiom::DisjointUnion(c, xs) => format!("DisjointUnion({} {})", self.n(*c), self.cs(xs)),
            Axiom::SubObjectPropertyOf(chain, sup) => match chain[..] {
                [sub] => format!("SubObjectPropertyOf({} {})", self.p(sub), self.p(*sup)),
                _ => format!(
                    "SubObjectPropertyOf(ObjectPropertyChain({}) {})",
                    self.ps(chain),
                    self.p(*sup)
                ),
            },
            Axiom::EquivalentObjectProperties(ps) => {
                format!("EquivalentObjectProperties({})", self.ps(ps))
            }
            Axiom::DisjointObjectProperties(ps) => {
                format!("DisjointObjectProperties({})", self.ps(ps))
            }
            Axiom::InverseObjectProperties(a, b) => {
                format!("InverseObjectProperties({} {})", self.p(*a), self.p(*b))
            }
            Axiom::ObjectPropertyDomain(p, x) => {
                format!("ObjectPropertyDomain({} {})", self.p(*p), self.c(*x))
            }
            Axiom::ObjectPropertyRange(p, x) => {
                format!("ObjectPropertyRange({} {})", self.p(*p), self.c(*x))
            }
            Axiom::ObjectCharacteristic(kind, p) => {
                let kind = match kind {
                    Characteristic::Functional => "FunctionalObjectProperty",
                    Characteristic::InverseFunctional => "InverseFunctionalObjectProperty",
                    Characteristic::Reflexive => "ReflexiveObjectProperty",
                    Characteristic::Irreflexive => "IrreflexiveObjectProperty",
                    Characteristic::Symmetric => "SymmetricObjectProperty",
                    Characteristic::Asymmetric => "AsymmetricObjectProperty",
                    Characteristic::Transitive => "TransitiveObjectProperty",
                };
                format!("{kind}({})", self.p(*p))
            }
            Axiom::SubDataPropertyOf(a, b) => {
                format!("SubDataPropertyOf({} {})", self.n(*a), self.n(*b))
            }
            Axiom::EquivalentDataProperties(ps) => {
                format!("EquivalentDataProperties({})", self.terms(ps))
            }
            Axiom::DisjointDataProperties(ps) => {
                format!("DisjointDataProperties({})", self.terms(ps))
            }
            Axiom::DataPropertyDomain(p, x) => {
                format!("DataPropertyDomain({} {})", self.n(*p), self.c(*x))
            }
            Axiom::DataPropertyRange(p, r) => {
                format!("DataPropertyRange({} {})", self.n(*p), self.r(*r))
            }
            Axiom::FunctionalDataProperty(p) => format!("FunctionalDataProperty({})", self.n(*p)),
            Axiom::DatatypeDefinition(d, r) => {
                format!("DatatypeDefinition({} {})", self.n(*d), self.r(*r))
            }
            Axiom::HasKey(c, ops, dps) => format!(
                "HasKey({} ({}) ({}))",
                self.c(*c),
                self.ps(ops),
                self.terms(dps)
            ),
            Axiom::ClassAssertion(c, a) => format!("ClassAssertion({} {})", self.c(*c), self.n(*a)),
            Axiom::ObjectPropertyAssertion(p, a, b) => format!(
                "ObjectPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*b)
            ),
            Axiom::NegativeObjectPropertyAssertion(p, a, b) => format!(
                "NegativeObjectPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*b)
            ),
            Axiom::DataPropertyAssertion(p, a, v) => format!(
                "DataPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*v)
            ),
            Axiom::NegativeDataPropertyAssertion(p, a, v) => format!(
                "NegativeDataPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*v)
            ),
            Axiom::SameIndividual(xs) => format!("SameIndividual({})", self.terms(xs)),
            Axiom::DifferentIndividuals(xs) => format!("DifferentIndividuals({})", self.terms(xs)),
        }
    }
}
