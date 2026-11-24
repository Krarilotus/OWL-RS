//! OWL 2 DL's typing constraints (*Structural Specification* §5.8.1) over the axioms read: a
//! name used as an object, a data or an annotation property is used as no other of the
//! three, and a class is no datatype. Each kind a name has comes from its declarations (and
//! the built-in ones of Table 5) and from every place an axiom, a class expression, a data
//! range or a rule atom puts it, each named below by an exhaustive match; an annotation
//! property's from the statements read past as annotations ([`Ontology::annotation_uses`]),
//! by an axiom where an annotation axiom names it ([`Ontology::annotation_axioms`]).
//! Individuals take part in no constraint: a name may be an individual and anything else
//! (punning).
//!
//! The readers report each name found as a [`crate::Diagnostic::TwoKinds`]; what it does to
//! reasoning is its [`crate::Treatment`].

use std::collections::{BTreeMap, HashSet};

use crate::mapping::Ontology;
use crate::model::{Axiom, ClassExpr, DataRange, EntityKind, ExprId, ObjProp, RangeId, Term};
use crate::rules::Atom;

/// A name used as two kinds OWL 2 DL keeps apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TwoKinds {
    pub term: Term,
    /// As in [`PAIRS`].
    pub kinds: [EntityKind; 2],
    /// Whether something other than an assertion or a statement read past names it as one
    /// of the two: a declaration, a property axiom (a characteristic, a domain or range, a
    /// sub-, equivalent, inverse or disjoint property, a chain, a key), a class expression or
    /// data range (a restriction's property included), an annotation axiom, a rule. Where one
    /// does, the OWL 2 RL rules can apply it to the other kind's statements.
    pub by_axiom: bool,
}

/// The kinds §5.8.1 keeps apart, pairwise.
pub const PAIRS: [[EntityKind; 2]; 4] = [
    [EntityKind::ObjectProperty, EntityKind::DataProperty],
    [EntityKind::ObjectProperty, EntityKind::AnnotationProperty],
    [EntityKind::DataProperty, EntityKind::AnnotationProperty],
    [EntityKind::Class, EntityKind::Datatype],
];

/// Every name of `ontology` used as two kinds of a pair of [`PAIRS`], by term, then pair.
pub fn two_kinds(ontology: &Ontology) -> Vec<TwoKinds> {
    let mut check = Check {
        o: ontology,
        uses: BTreeMap::new(),
        classes: HashSet::new(),
        ranges: HashSet::new(),
    };
    for axiom in &ontology.axioms {
        check.axiom(axiom);
    }
    for rule in &ontology.rules {
        for atom in rule.body.iter().chain(&rule.head) {
            check.atom(atom);
        }
    }
    for &t in &ontology.annotation_uses {
        check.mark(t, EntityKind::AnnotationProperty, false);
    }
    for &t in &ontology.annotation_axioms {
        check.mark(t, EntityKind::AnnotationProperty, true);
    }
    // The built-in properties are declared by OWL 2 itself (Table 5), where they are used.
    let builtin = ontology.builtin;
    for (property, kind) in [
        (builtin.top_object, EntityKind::ObjectProperty),
        (builtin.bottom_object, EntityKind::ObjectProperty),
        (builtin.top_data, EntityKind::DataProperty),
        (builtin.bottom_data, EntityKind::DataProperty),
    ] {
        if let Some(t) = property
            && check.uses.contains_key(&t)
        {
            check.mark(t, kind, true);
        }
    }
    let mut out = Vec::new();
    for (&term, uses) in &check.uses {
        for kinds in PAIRS {
            let [a, b] = kinds.map(bit);
            if uses.any & a != 0 && uses.any & b != 0 {
                let by_axiom = uses.by_axiom & (a | b) != 0;
                out.push(TwoKinds {
                    term,
                    kinds,
                    by_axiom,
                });
            }
        }
    }
    out
}

/// A name's kinds, a bit each ([`bit`]): all, and those something other than an assertion
/// gives it.
#[derive(Debug, Clone, Copy, Default)]
struct Uses {
    any: u8,
    by_axiom: u8,
}

#[deny(clippy::wildcard_enum_match_arm)]
fn bit(kind: EntityKind) -> u8 {
    match kind {
        EntityKind::Class => 1,
        EntityKind::ObjectProperty => 2,
        EntityKind::DataProperty => 4,
        EntityKind::AnnotationProperty => 8,
        EntityKind::Datatype => 16,
        EntityKind::NamedIndividual => 32,
    }
}

struct Check<'o> {
    o: &'o Ontology,
    uses: BTreeMap<Term, Uses>,
    /// The class expressions and data ranges walked from an axiom ([`Self::expression`]).
    classes: HashSet<ExprId>,
    ranges: HashSet<RangeId>,
}

impl Check<'_> {
    fn mark(&mut self, term: Term, kind: EntityKind, by_axiom: bool) {
        let uses = self.uses.entry(term).or_default();
        uses.any |= bit(kind);
        if by_axiom {
            uses.by_axiom |= bit(kind);
        }
    }

    fn object(&mut self, p: ObjProp, by_axiom: bool) {
        self.mark(p.named(), EntityKind::ObjectProperty, by_axiom);
    }

    fn data(&mut self, p: Term, by_axiom: bool) {
        self.mark(p, EntityKind::DataProperty, by_axiom);
    }

    #[deny(clippy::wildcard_enum_match_arm)]
    fn axiom(&mut self, axiom: &Axiom) {
        match axiom {
            Axiom::Declaration(kind, t) => self.mark(*t, *kind, true),
            Axiom::SubClassOf(a, b) => {
                self.class(*a);
                self.class(*b);
            }
            Axiom::EquivalentClasses(xs) | Axiom::DisjointClasses(xs) => {
                xs.iter().for_each(|&x| self.class(x));
            }
            Axiom::DisjointUnion(c, xs) => {
                self.class(*c);
                xs.iter().for_each(|&x| self.class(x));
            }
            Axiom::SubObjectPropertyOf(chain, sup) => {
                chain.iter().for_each(|&p| self.object(p, true));
                self.object(*sup, true);
            }
            Axiom::EquivalentObjectProperties(ps) | Axiom::DisjointObjectProperties(ps) => {
                ps.iter().for_each(|&p| self.object(p, true));
            }
            Axiom::InverseObjectProperties(a, b) => {
                self.object(*a, true);
                self.object(*b, true);
            }
            Axiom::ObjectPropertyDomain(p, x) | Axiom::ObjectPropertyRange(p, x) => {
                self.object(*p, true);
                self.class(*x);
            }
            Axiom::ObjectCharacteristic(_, p) => self.object(*p, true),
            Axiom::SubDataPropertyOf(a, b) => {
                self.data(*a, true);
                self.data(*b, true);
            }
            Axiom::EquivalentDataProperties(ps) | Axiom::DisjointDataProperties(ps) => {
                ps.iter().for_each(|&p| self.data(p, true));
            }
            Axiom::DataPropertyDomain(p, x) => {
                self.data(*p, true);
                self.class(*x);
            }
            Axiom::DataPropertyRange(p, r) => {
                self.data(*p, true);
                self.range(*r);
            }
            Axiom::FunctionalDataProperty(p) => self.data(*p, true),
            Axiom::DatatypeDefinition(t, r) => {
                self.mark(*t, EntityKind::Datatype, true);
                self.range(*r);
            }
            Axiom::HasKey(x, objects, data) => {
                self.class(*x);
                objects.iter().for_each(|&p| self.object(p, true));
                data.iter().for_each(|&p| self.data(p, true));
            }
            Axiom::ClassAssertion(x, _) => self.expression(*x, false),
            Axiom::ObjectPropertyAssertion(p, _, _)
            | Axiom::NegativeObjectPropertyAssertion(p, _, _) => {
                self.object(ObjProp::Named(*p), false);
            }
            Axiom::DataPropertyAssertion(p, _, _)
            | Axiom::NegativeDataPropertyAssertion(p, _, _) => self.data(*p, false),
            // Individuals: no typing constraint.
            Axiom::SameIndividual(_) | Axiom::DifferentIndividuals(_) => {}
        }
    }

    /// A class expression an axiom (or a rule) names, and every expression and range in it.
    fn class(&mut self, id: ExprId) {
        self.expression(id, true);
    }

    /// A class expression and every expression and range in it; `by_axiom`: whether a named
    /// class here is named by an axiom, not by an assertion's own place. Everything within
    /// is: the rules read a restriction or a set operator in an assertion's class as they
    /// read one in an axiom.
    /// Walked with its own stack, operands in order, so that the nesting depth of an
    /// expression is no call depth.
    #[deny(clippy::wildcard_enum_match_arm)]
    fn expression(&mut self, id: ExprId, by_axiom: bool) {
        let mut stack = vec![(id, by_axiom)];
        while let Some((id, by_axiom)) = stack.pop() {
            // Only the walks from an axiom are remembered: one from an assertion marks less.
            if by_axiom && !self.classes.insert(id) {
                continue;
            }
            match self.o.class(id) {
                ClassExpr::Class(t) => self.mark(*t, EntityKind::Class, by_axiom),
                ClassExpr::Thing | ClassExpr::Nothing | ClassExpr::OneOf(_) => {}
                ClassExpr::And(xs) | ClassExpr::Or(xs) => {
                    stack.extend(xs.iter().rev().map(|&x| (x, true)));
                }
                ClassExpr::Not(x) => stack.push((*x, true)),
                ClassExpr::Some(p, x)
                | ClassExpr::All(p, x)
                | ClassExpr::Min(_, p, x)
                | ClassExpr::Max(_, p, x)
                | ClassExpr::Exact(_, p, x) => {
                    self.object(*p, true);
                    stack.push((*x, true));
                }
                ClassExpr::HasValue(p, _) | ClassExpr::HasSelf(p) => self.object(*p, true),
                ClassExpr::DataSome(p, r)
                | ClassExpr::DataAll(p, r)
                | ClassExpr::DataMin(_, p, r)
                | ClassExpr::DataMax(_, p, r)
                | ClassExpr::DataExact(_, p, r) => {
                    self.data(*p, true);
                    self.range(*r);
                }
                ClassExpr::DataHasValue(p, _) => self.data(*p, true),
            }
        }
    }

    /// A data range an axiom (or a rule) names, and every range in it.
    #[deny(clippy::wildcard_enum_match_arm)]
    fn range(&mut self, id: RangeId) {
        if !self.ranges.insert(id) {
            return;
        }
        match self.o.range(id) {
            DataRange::Literal | DataRange::OneOf(_) => {}
            DataRange::Datatype(t) | DataRange::Restriction(t, _) => {
                self.mark(*t, EntityKind::Datatype, true);
            }
            DataRange::And(rs) | DataRange::Or(rs) => rs.iter().for_each(|&r| self.range(r)),
            DataRange::Not(r) => self.range(*r),
        }
    }

    /// A rule's atom: a rule is no assertion.
    #[deny(clippy::wildcard_enum_match_arm)]
    fn atom(&mut self, atom: &Atom) {
        match atom {
            Atom::Class(x, _) => self.class(*x),
            Atom::Range(r, _) => self.range(*r),
            Atom::Object(p, _, _) => self.object(*p, true),
            Atom::Data(p, _, _) => self.data(*p, true),
            // Individuals, and a built-in's IRI (no entity).
            Atom::Same(..) | Atom::Different(..) | Atom::BuiltIn(..) => {}
        }
    }
}
