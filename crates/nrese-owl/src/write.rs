//! The forward OWL 2 RDF mapping (W3C *OWL 2 Mapping to RDF Graphs*, §2): an ontology's
//! axioms as triples over the same term ids, blank nodes and number literals made by the
//! caller. Reading them back ([`crate::read`]) gives the same axioms: the round trip
//! tests both directions.

use crate::mapping::Ontology;
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, EntityKind, ExprId, ObjProp, RangeId, Term,
};
use crate::vocab::Vocabulary;

/// Makes the terms writing needs.
pub trait Make {
    /// A fresh blank node.
    fn blank(&mut self) -> Term;
    /// The literal `lexical` of datatype `datatype` (an IRI).
    fn literal(&mut self, lexical: &str, datatype: &str) -> Term;
}

/// The triples of `ontology`'s axioms. `vocabulary` must have every term (the caller
/// interns them first).
pub fn write(ontology: &Ontology, vocabulary: &Vocabulary, make: &mut dyn Make) -> Vec<[Term; 3]> {
    let mut writer = Writer {
        ontology,
        v: vocabulary,
        make,
        out: Vec::new(),
    };
    for axiom in &ontology.axioms {
        writer.axiom(axiom);
    }
    writer.out
}

struct Writer<'a> {
    ontology: &'a Ontology,
    v: &'a Vocabulary,
    make: &'a mut dyn Make,
    out: Vec<[Term; 3]>,
}

/// A vocabulary term the caller must have interned.
macro_rules! term {
    ($self:ident . $field:ident) => {
        $self
            .v
            .$field
            .expect(concat!("the vocabulary has ", stringify!($field)))
    };
}

impl Writer<'_> {
    fn push(&mut self, s: Term, p: Term, o: Term) {
        self.out.push([s, p, o]);
    }

    fn list(&mut self, members: &[Term]) -> Term {
        let mut head = term!(self.nil);
        for &member in members.iter().rev() {
            let cell = self.make.blank();
            self.push(cell, term!(self.first), member);
            self.push(cell, term!(self.rest), head);
            head = cell;
        }
        head
    }

    fn number(&mut self, n: u32) -> Term {
        self.make.literal(
            &n.to_string(),
            "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
        )
    }

    fn property(&mut self, property: ObjProp) -> Term {
        match property {
            ObjProp::Named(p) => p,
            ObjProp::Inverse(p) => {
                let node = self.make.blank();
                self.push(node, term!(self.owl_inverse_of), p);
                node
            }
        }
    }

    fn class(&mut self, id: ExprId) -> Term {
        let expr = self.ontology.class(id).clone();
        let structure = |w: &mut Self, kind: Term| {
            let node = w.make.blank();
            w.push(node, term!(w.rdf_type), kind);
            node
        };
        match expr {
            ClassExpr::Class(t) => t,
            ClassExpr::Thing => term!(self.owl_thing),
            ClassExpr::Nothing => term!(self.owl_nothing),
            ClassExpr::And(xs) | ClassExpr::Or(xs) => {
                let union = matches!(self.ontology.class(id), ClassExpr::Or(_));
                let members: Vec<Term> = xs.iter().map(|&x| self.class(x)).collect();
                let list = self.list(&members);
                let node = structure(self, term!(self.owl_class));
                let operator = if union {
                    term!(self.owl_union_of)
                } else {
                    term!(self.owl_intersection_of)
                };
                self.push(node, operator, list);
                node
            }
            ClassExpr::Not(x) => {
                let operand = self.class(x);
                let node = structure(self, term!(self.owl_class));
                self.push(node, term!(self.owl_complement_of), operand);
                node
            }
            ClassExpr::OneOf(individuals) => {
                let list = self.list(&individuals);
                let node = structure(self, term!(self.owl_class));
                self.push(node, term!(self.owl_one_of), list);
                node
            }
            ClassExpr::Some(p, x) | ClassExpr::All(p, x) => {
                let some = matches!(self.ontology.class(id), ClassExpr::Some(..));
                let filler = self.class(x);
                let property = self.property(p);
                let node = self.restriction(property);
                let predicate = if some {
                    term!(self.owl_some_values_from)
                } else {
                    term!(self.owl_all_values_from)
                };
                self.push(node, predicate, filler);
                node
            }
            ClassExpr::HasValue(p, value) => {
                let property = self.property(p);
                let node = self.restriction(property);
                self.push(node, term!(self.owl_has_value), value);
                node
            }
            ClassExpr::HasSelf(p) => {
                let property = self.property(p);
                let node = self.restriction(property);
                let yes = self
                    .make
                    .literal("true", "http://www.w3.org/2001/XMLSchema#boolean");
                self.push(node, term!(self.owl_has_self), yes);
                node
            }
            ClassExpr::Min(n, p, x) | ClassExpr::Max(n, p, x) | ClassExpr::Exact(n, p, x) => {
                let which = match self.ontology.class(id) {
                    ClassExpr::Min(..) => 0,
                    ClassExpr::Max(..) => 1,
                    _ => 2,
                };
                let property = self.property(p);
                let node = self.restriction(property);
                let number = self.number(n);
                if matches!(self.ontology.class(x), ClassExpr::Thing) {
                    let predicate = [
                        term!(self.owl_min_cardinality),
                        term!(self.owl_max_cardinality),
                        term!(self.owl_cardinality),
                    ][which];
                    self.push(node, predicate, number);
                } else {
                    let predicate = [
                        term!(self.owl_min_qualified),
                        term!(self.owl_max_qualified),
                        term!(self.owl_qualified),
                    ][which];
                    let filler = self.class(x);
                    self.push(node, predicate, number);
                    self.push(node, term!(self.owl_on_class), filler);
                }
                node
            }
            ClassExpr::DataSome(p, r) | ClassExpr::DataAll(p, r) => {
                let some = matches!(self.ontology.class(id), ClassExpr::DataSome(..));
                let filler = self.range(r);
                let node = self.restriction(p);
                let predicate = if some {
                    term!(self.owl_some_values_from)
                } else {
                    term!(self.owl_all_values_from)
                };
                self.push(node, predicate, filler);
                node
            }
            ClassExpr::DataHasValue(p, value) => {
                let node = self.restriction(p);
                self.push(node, term!(self.owl_has_value), value);
                node
            }
            ClassExpr::DataMin(n, p, r)
            | ClassExpr::DataMax(n, p, r)
            | ClassExpr::DataExact(n, p, r) => {
                let which = match self.ontology.class(id) {
                    ClassExpr::DataMin(..) => 0,
                    ClassExpr::DataMax(..) => 1,
                    _ => 2,
                };
                let node = self.restriction(p);
                let number = self.number(n);
                let literal = matches!(
                    self.ontology.range(r),
                    DataRange::Datatype(t) if Some(*t) == self.v.rdfs_literal
                );
                if literal {
                    let predicate = [
                        term!(self.owl_min_cardinality),
                        term!(self.owl_max_cardinality),
                        term!(self.owl_cardinality),
                    ][which];
                    self.push(node, predicate, number);
                } else {
                    let predicate = [
                        term!(self.owl_min_qualified),
                        term!(self.owl_max_qualified),
                        term!(self.owl_qualified),
                    ][which];
                    let filler = self.range(r);
                    self.push(node, predicate, number);
                    self.push(node, term!(self.owl_on_data_range), filler);
                }
                node
            }
        }
    }

    fn restriction(&mut self, property: Term) -> Term {
        let node = self.make.blank();
        self.push(node, term!(self.rdf_type), term!(self.owl_restriction));
        self.push(node, term!(self.owl_on_property), property);
        node
    }

    fn range(&mut self, id: RangeId) -> Term {
        let range = self.ontology.range(id).clone();
        let structure = |w: &mut Self| {
            let node = w.make.blank();
            w.push(node, term!(w.rdf_type), term!(w.rdfs_datatype));
            node
        };
        match range {
            DataRange::Datatype(t) => t,
            DataRange::And(xs) | DataRange::Or(xs) => {
                let union = matches!(self.ontology.range(id), DataRange::Or(_));
                let members: Vec<Term> = xs.iter().map(|&x| self.range(x)).collect();
                let list = self.list(&members);
                let node = structure(self);
                let operator = if union {
                    term!(self.owl_union_of)
                } else {
                    term!(self.owl_intersection_of)
                };
                self.push(node, operator, list);
                node
            }
            DataRange::Not(x) => {
                let operand = self.range(x);
                let node = structure(self);
                self.push(node, term!(self.owl_datatype_complement_of), operand);
                node
            }
            DataRange::OneOf(literals) => {
                let list = self.list(&literals);
                let node = structure(self);
                self.push(node, term!(self.owl_one_of), list);
                node
            }
            DataRange::Restriction(datatype, facets) => {
                let mut restrictions = Vec::new();
                for (facet, value) in facets {
                    let node = self.make.blank();
                    self.push(node, facet, value);
                    restrictions.push(node);
                }
                let list = self.list(&restrictions);
                let node = structure(self);
                self.push(node, term!(self.owl_on_datatype), datatype);
                self.push(node, term!(self.owl_with_restrictions), list);
                node
            }
        }
    }

    fn axiom(&mut self, axiom: &Axiom) {
        match axiom {
            Axiom::Declaration(kind, entity) => {
                let class = match kind {
                    EntityKind::Class => term!(self.owl_class),
                    EntityKind::ObjectProperty => term!(self.owl_object_property),
                    EntityKind::DataProperty => term!(self.owl_datatype_property),
                    EntityKind::AnnotationProperty => term!(self.owl_annotation_property),
                    EntityKind::Datatype => term!(self.rdfs_datatype),
                    EntityKind::NamedIndividual => term!(self.owl_named_individual),
                };
                self.push(*entity, term!(self.rdf_type), class);
            }
            Axiom::SubClassOf(a, b) => {
                let (a, b) = (self.class(*a), self.class(*b));
                self.push(a, term!(self.rdfs_sub_class_of), b);
            }
            Axiom::EquivalentClasses(xs) => {
                let first = self.class(xs[0]);
                if xs.len() == 1 {
                    // An expression equivalent to itself (OWL 1 states expressions so).
                    self.push(first, term!(self.owl_equivalent_class), first);
                }
                for &x in &xs[1..] {
                    let other = self.class(x);
                    self.push(first, term!(self.owl_equivalent_class), other);
                }
            }
            Axiom::DisjointClasses(xs) => {
                let members: Vec<Term> = xs.iter().map(|&x| self.class(x)).collect();
                if let [a, b] = members[..] {
                    self.push(a, term!(self.owl_disjoint_with), b);
                } else {
                    let list = self.list(&members);
                    let node = self.make.blank();
                    self.push(
                        node,
                        term!(self.rdf_type),
                        term!(self.owl_all_disjoint_classes),
                    );
                    self.push(node, term!(self.owl_members), list);
                }
            }
            Axiom::DisjointUnion(class, xs) => {
                let members: Vec<Term> = xs.iter().map(|&x| self.class(x)).collect();
                let list = self.list(&members);
                self.push(*class, term!(self.owl_disjoint_union_of), list);
            }
            Axiom::SubObjectPropertyOf(chain, sup) => {
                let sup = self.property(*sup);
                if let [sub] = chain[..] {
                    let sub = self.property(sub);
                    self.push(sub, term!(self.rdfs_sub_property_of), sup);
                } else {
                    let members: Vec<Term> = chain.iter().map(|&p| self.property(p)).collect();
                    let list = self.list(&members);
                    self.push(sup, term!(self.owl_property_chain_axiom), list);
                }
            }
            Axiom::EquivalentObjectProperties(ps) => {
                let first = self.property(ps[0]);
                for &p in &ps[1..] {
                    let other = self.property(p);
                    self.push(first, term!(self.owl_equivalent_property), other);
                }
            }
            Axiom::DisjointObjectProperties(ps) => {
                let members: Vec<Term> = ps.iter().map(|&p| self.property(p)).collect();
                self.disjoint_properties(&members);
            }
            Axiom::InverseObjectProperties(a, b) => {
                // A pair of inverses is the pair of the properties (p⁻ = q⁻⁻ iff p = q⁻),
                // the form an IRI subject can carry.
                let (a, b) = match (a, b) {
                    (ObjProp::Inverse(x), ObjProp::Inverse(y)) => {
                        (ObjProp::Named(*x), ObjProp::Named(*y))
                    }
                    (ObjProp::Named(_), _) => (*a, *b),
                    _ => (*b, *a),
                };
                let (a, b) = (self.property(a), self.property(b));
                self.push(a, term!(self.owl_inverse_of), b);
            }
            Axiom::ObjectPropertyDomain(p, x) | Axiom::ObjectPropertyRange(p, x) => {
                let domain = matches!(axiom, Axiom::ObjectPropertyDomain(..));
                let (p, x) = (self.property(*p), self.class(*x));
                let predicate = if domain {
                    term!(self.rdfs_domain)
                } else {
                    term!(self.rdfs_range)
                };
                self.push(p, predicate, x);
            }
            Axiom::ObjectCharacteristic(kind, p) => {
                let class = match kind {
                    Characteristic::Functional => term!(self.owl_functional),
                    Characteristic::InverseFunctional => term!(self.owl_inverse_functional),
                    Characteristic::Reflexive => term!(self.owl_reflexive),
                    Characteristic::Irreflexive => term!(self.owl_irreflexive),
                    Characteristic::Symmetric => term!(self.owl_symmetric),
                    Characteristic::Asymmetric => term!(self.owl_asymmetric),
                    Characteristic::Transitive => term!(self.owl_transitive),
                };
                let p = self.property(*p);
                self.push(p, term!(self.rdf_type), class);
            }
            Axiom::SubDataPropertyOf(a, b) => self.push(*a, term!(self.rdfs_sub_property_of), *b),
            Axiom::EquivalentDataProperties(ps) => {
                for &p in &ps[1..] {
                    self.push(ps[0], term!(self.owl_equivalent_property), p);
                }
            }
            Axiom::DisjointDataProperties(ps) => self.disjoint_properties(ps),
            Axiom::DataPropertyDomain(p, x) => {
                let x = self.class(*x);
                self.push(*p, term!(self.rdfs_domain), x);
            }
            Axiom::DataPropertyRange(p, r) => {
                let r = self.range(*r);
                self.push(*p, term!(self.rdfs_range), r);
            }
            Axiom::FunctionalDataProperty(p) => {
                self.push(*p, term!(self.rdf_type), term!(self.owl_functional));
            }
            Axiom::DatatypeDefinition(d, r) => {
                let r = self.range(*r);
                self.push(*d, term!(self.owl_equivalent_class), r);
            }
            Axiom::HasKey(class, objects, datas) => {
                let class = self.class(*class);
                let mut members: Vec<Term> = objects.iter().map(|&p| self.property(p)).collect();
                members.extend(datas);
                let list = self.list(&members);
                self.push(class, term!(self.owl_has_key), list);
            }
            Axiom::ClassAssertion(class, individual) => {
                let class = self.class(*class);
                self.push(*individual, term!(self.rdf_type), class);
            }
            Axiom::ObjectPropertyAssertion(p, a, b) | Axiom::DataPropertyAssertion(p, a, b) => {
                self.push(*a, *p, *b);
            }
            Axiom::NegativeObjectPropertyAssertion(p, a, b)
            | Axiom::NegativeDataPropertyAssertion(p, a, b) => {
                let data = matches!(axiom, Axiom::NegativeDataPropertyAssertion(..));
                let node = self.make.blank();
                self.push(
                    node,
                    term!(self.rdf_type),
                    term!(self.owl_negative_property_assertion),
                );
                self.push(node, term!(self.owl_source_individual), *a);
                self.push(node, term!(self.owl_assertion_property), *p);
                let target = if data {
                    term!(self.owl_target_value)
                } else {
                    term!(self.owl_target_individual)
                };
                self.push(node, target, *b);
            }
            Axiom::SameIndividual(xs) => {
                for &x in &xs[1..] {
                    self.push(xs[0], term!(self.owl_same_as), x);
                }
            }
            Axiom::DifferentIndividuals(xs) => {
                if let [a, b] = xs[..] {
                    self.push(a, term!(self.owl_different_from), b);
                } else {
                    let list = self.list(xs);
                    let node = self.make.blank();
                    self.push(node, term!(self.rdf_type), term!(self.owl_all_different));
                    self.push(node, term!(self.owl_members), list);
                }
            }
        }
    }

    fn disjoint_properties(&mut self, members: &[Term]) {
        if let [a, b] = members[..] {
            self.push(a, term!(self.owl_property_disjoint_with), b);
        } else {
            let list = self.list(members);
            let node = self.make.blank();
            self.push(
                node,
                term!(self.rdf_type),
                term!(self.owl_all_disjoint_properties),
            );
            self.push(node, term!(self.owl_members), list);
        }
    }
}
