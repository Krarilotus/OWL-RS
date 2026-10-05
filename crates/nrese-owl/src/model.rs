//! The OWL 2 structural model (W3C *OWL 2 Structural Specification*), over the term ids
//! of the store the ontology was read from: entities, literals and individuals are those
//! ids ([`Term`]), class expressions and data ranges are interned ([`ExprId`],
//! [`RangeId`]) so that equal expressions are one, in a canonical form (n-ary operands
//! sorted and without repeats, inverses of inverses removed).

use std::collections::HashMap;
use std::hash::Hash;

/// A term of the source: an IRI, a blank node or a literal, by its id there.
pub type Term = u64;

/// An interned class expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExprId(pub u32);

/// An interned data range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RangeId(pub u32);

/// An object property expression: a named property or its inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjProp {
    Named(Term),
    Inverse(Term),
}

impl ObjProp {
    /// The inverse expression (an inverse's inverse is the property).
    pub fn inverse(self) -> Self {
        match self {
            Self::Named(p) => Self::Inverse(p),
            Self::Inverse(p) => Self::Named(p),
        }
    }

    /// The named property it is over.
    pub fn named(self) -> Term {
        match self {
            Self::Named(p) | Self::Inverse(p) => p,
        }
    }
}

/// A class expression; operands are interned.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClassExpr {
    /// A named class (`owl:Thing` and `owl:Nothing` are [`Self::Thing`], [`Self::Nothing`]).
    Class(Term),
    Thing,
    Nothing,
    And(Vec<ExprId>),
    Or(Vec<ExprId>),
    Not(ExprId),
    OneOf(Vec<Term>),
    Some(ObjProp, ExprId),
    All(ObjProp, ExprId),
    HasValue(ObjProp, Term),
    HasSelf(ObjProp),
    Min(u32, ObjProp, ExprId),
    Max(u32, ObjProp, ExprId),
    Exact(u32, ObjProp, ExprId),
    DataSome(Term, RangeId),
    DataAll(Term, RangeId),
    DataHasValue(Term, Term),
    DataMin(u32, Term, RangeId),
    DataMax(u32, Term, RangeId),
    DataExact(u32, Term, RangeId),
}

/// A data range; operands are interned.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DataRange {
    /// `rdfs:Literal`, every data value: the IRI read so, and the filler of a data
    /// cardinality without one (also where the source has no term for the IRI).
    Literal,
    /// A datatype other than `rdfs:Literal`.
    Datatype(Term),
    And(Vec<RangeId>),
    Or(Vec<RangeId>),
    Not(RangeId),
    OneOf(Vec<Term>),
    /// A datatype restricted by facets: (facet IRI, literal), sorted.
    Restriction(Term, Vec<(Term, Term)>),
}

/// A literal as its source writes it: the lexical form, the datatype's IRI and the
/// language tag (`None` where the source doesn't give them).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Literal {
    pub lexical: String,
    pub datatype: Option<String>,
    pub language: Option<String>,
}

/// The literals and the datatype and facet IRIs the axioms use, by term: what a datatype
/// theory reads values from (the model itself has only term ids). A literal or IRI
/// missing here is one the source didn't give; the reasoners say so rather than guess.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataTerms {
    pub literals: HashMap<Term, Literal>,
    pub iris: HashMap<Term, String>,
}

impl DataTerms {
    /// Adds `other`'s terms (an imported axiom's, say).
    pub fn extend(&mut self, other: &DataTerms) {
        for (t, l) in &other.literals {
            self.literals.entry(*t).or_insert_with(|| l.clone());
        }
        for (t, i) in &other.iris {
            self.iris.entry(*t).or_insert_with(|| i.clone());
        }
    }
}

/// What an entity is declared as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntityKind {
    Class,
    ObjectProperty,
    DataProperty,
    AnnotationProperty,
    Datatype,
    NamedIndividual,
}

/// An object property characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Characteristic {
    Functional,
    InverseFunctional,
    Reflexive,
    Irreflexive,
    Symmetric,
    Asymmetric,
    Transitive,
}

/// A logical axiom, or a declaration. n-ary operands are sorted and without repeats.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Axiom {
    Declaration(EntityKind, Term),
    SubClassOf(ExprId, ExprId),
    EquivalentClasses(Vec<ExprId>),
    DisjointClasses(Vec<ExprId>),
    /// The class is the disjoint union of the expressions.
    DisjointUnion(Term, Vec<ExprId>),
    /// The chain (one property: a plain subproperty axiom) is a subproperty of the last.
    SubObjectPropertyOf(Vec<ObjProp>, ObjProp),
    EquivalentObjectProperties(Vec<ObjProp>),
    DisjointObjectProperties(Vec<ObjProp>),
    InverseObjectProperties(ObjProp, ObjProp),
    ObjectPropertyDomain(ObjProp, ExprId),
    ObjectPropertyRange(ObjProp, ExprId),
    ObjectCharacteristic(Characteristic, ObjProp),
    SubDataPropertyOf(Term, Term),
    EquivalentDataProperties(Vec<Term>),
    DisjointDataProperties(Vec<Term>),
    DataPropertyDomain(Term, ExprId),
    DataPropertyRange(Term, RangeId),
    FunctionalDataProperty(Term),
    DatatypeDefinition(Term, RangeId),
    HasKey(ExprId, Vec<ObjProp>, Vec<Term>),
    ClassAssertion(ExprId, Term),
    /// Always over a named property: an assertion of an inverse is stored swapped.
    ObjectPropertyAssertion(Term, Term, Term),
    NegativeObjectPropertyAssertion(Term, Term, Term),
    DataPropertyAssertion(Term, Term, Term),
    NegativeDataPropertyAssertion(Term, Term, Term),
    SameIndividual(Vec<Term>),
    DifferentIndividuals(Vec<Term>),
}

impl Axiom {
    /// Whether it is about individuals (an ABox axiom).
    pub fn is_assertion(&self) -> bool {
        matches!(
            self,
            Self::ClassAssertion(..)
                | Self::ObjectPropertyAssertion(..)
                | Self::NegativeObjectPropertyAssertion(..)
                | Self::DataPropertyAssertion(..)
                | Self::NegativeDataPropertyAssertion(..)
                | Self::SameIndividual(..)
                | Self::DifferentIndividuals(..)
        )
    }
}

/// Values, each stored once, by a dense id.
#[derive(Debug, Clone)]
pub struct Interner<T> {
    values: Vec<T>,
    ids: HashMap<T, u32>,
}

impl<T> Default for Interner<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            ids: HashMap::new(),
        }
    }
}

impl<T: Clone + Eq + Hash> Interner<T> {
    /// The id of `value`, added if new.
    pub fn intern(&mut self, value: T) -> u32 {
        if let Some(&id) = self.ids.get(&value) {
            return id;
        }
        let id = self.values.len() as u32;
        self.values.push(value.clone());
        self.ids.insert(value, id);
        id
    }

    pub fn get(&self, id: u32) -> &T {
        &self.values[id as usize]
    }

    /// The id of `value`, if it is interned.
    pub fn find(&self, value: &T) -> Option<u32> {
        self.ids.get(value).copied()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Sorts and removes repeats: the canonical form of n-ary operands.
pub(crate) fn canonical<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort();
    items.dedup();
    items
}
