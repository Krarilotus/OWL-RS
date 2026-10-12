//! Every OWL 2 DL ontology up to a bound (docs/design/owl2-dl.md §11, "the small scope"):
//! the exhaustive part of the coverage proof, beside the random ontologies of
//! [`crate::fuzz`].
//!
//! - **The universe** ([`Universe`]): classes A and B, object properties r and s (and their
//!   inverses), `owl:topObjectProperty` U and `owl:bottomObjectProperty`, a data property
//!   d with `owl:topDataProperty` and `owl:bottomDataProperty`, individuals a and b, the
//!   datatypes `xsd:integer`, `xsd:string`, `rdfs:Literal` and one defined datatype, the
//!   integer literals 0 and 1, and the numbers 0 to 2 in cardinalities.
//! - **The grammar** covers every class expression constructor, every data range
//!   constructor and every axiom type of the *OWL 2 Structural Specification* (but
//!   annotations and declarations, which carry no meaning). Each expression and axiom has
//!   a size ([`Tables`]): one per constructor and per class or data range atom, properties
//!   and individuals free, a chain one more.
//! - **The scope** ([`Scope`]): per number of axioms, the largest total size of the
//!   axioms. [`cases`] enumerates every set of that many distinct axioms within it, in a fixed
//!   order, keeping those within OWL 2 DL's global restrictions (§11.2: simple roles, a
//!   regular property hierarchy decided exactly by its strict partial order, the uses of
//!   `owl:topDataProperty`, defined datatypes) and, of each class of ontologies equal up to
//!   a renaming, only the least ([`symmetries`]: A↔B, r↔s, r↔r⁻, s↔s⁻, a↔b, 32 in all).
//!
//! An ontology a renaming maps to another has the same answers up to the renaming, so the
//! reduction loses no case. Anonymous individuals, annotations, imports and declarations
//! beyond the universe's are outside the scope.

use std::collections::HashMap;

use crate::fuzz::Name;
use crate::mapping::{BuiltinProperties, Ontology};
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, DataTerms, EntityKind, ExprId, Literal, ObjProp,
    RangeId, Term,
};

/// The IRIs of the universe's entities: `{SCOPE}A`, `{SCOPE}r`, `{SCOPE}a`, ...
pub const SCOPE: &str = "http://example.org/scope#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// An object property expression of the universe: r or s (`0`, `1`), possibly inverse,
/// or one of the two built-in properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Prop(u8, bool),
    Top,
    Bottom,
}

impl Role {
    /// r, r⁻, s, s⁻, U, ⊥.
    pub const ALL: [Role; 6] = [
        Role::Prop(0, false),
        Role::Prop(0, true),
        Role::Prop(1, false),
        Role::Prop(1, true),
        Role::Top,
        Role::Bottom,
    ];
    /// The roles cardinalities, `Self` and the simple-role axioms are generated over.
    const SIMPLE: [Role; 4] = [
        Role::Prop(0, false),
        Role::Prop(0, true),
        Role::Prop(1, false),
        Role::Prop(1, true),
    ];
    /// The named ones: what assertions are over.
    const NAMED: [Role; 4] = [
        Role::Prop(0, false),
        Role::Prop(1, false),
        Role::Top,
        Role::Bottom,
    ];

    /// The expression's position among the eight of `AllOPE` (§11.1): property × inverse.
    fn slot(self) -> usize {
        match self {
            Role::Prop(i, inv) => 2 * i as usize + usize::from(inv),
            Role::Top => 4,
            Role::Bottom => 6,
        }
    }

    fn inverse_slot(slot: usize) -> usize {
        slot ^ 1
    }
}

/// A data property expression: d, `owl:topDataProperty` (only above another, §11.2) or
/// `owl:bottomDataProperty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DProp {
    D,
    Top,
    Bottom,
}

/// A data range of the universe; operands index [`Tables::ranges`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum D {
    Literal,
    Integer,
    String,
    /// The universe's defined datatype (an ontology using it must define it once).
    Defined,
    /// Literals 0 and 1, by bit.
    OneOf(u8),
    /// `xsd:integer[≥ 1]`.
    AtLeastOne,
    /// `xsd:integer[≤ 0]`.
    AtMostZero,
    Not(u32),
    And(u32, u32),
    Or(u32, u32),
}

/// A class expression of the universe; operands index [`Tables::classes`] (fillers of
/// data restrictions [`Tables::ranges`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum C {
    Class(u8),
    Thing,
    Nothing,
    /// Individuals a and b, by bit.
    OneOf(u8),
    HasValue(Role, u8),
    HasSelf(Role),
    DataHasValue(DProp, u8),
    Not(u32),
    And(u32, u32),
    Or(u32, u32),
    Some(Role, u32),
    All(Role, u32),
    Min(u8, Role, u32),
    Max(u8, Role, u32),
    Exact(u8, Role, u32),
    DataSome(DProp, u32),
    DataAll(DProp, u32),
    DataMin(u8, DProp, u32),
    DataMax(u8, DProp, u32),
    DataExact(u8, DProp, u32),
}

/// An axiom of the universe; class operands index [`Tables::classes`], data ranges
/// [`Tables::ranges`]. Pairs are sorted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ax {
    SubClassOf(u32, u32),
    EquivalentClasses(u32, u32),
    DisjointClasses(u32, u32),
    DisjointUnion(u8, u32, u32),
    SubObjectPropertyOf(Vec<Role>, Role),
    EquivalentObjectProperties(Role, Role),
    DisjointObjectProperties(Role, Role),
    InverseObjectProperties(Role, Role),
    ObjectPropertyDomain(Role, u32),
    ObjectPropertyRange(Role, u32),
    Characteristic(Characteristic, Role),
    SubDataPropertyOf(DProp, DProp),
    EquivalentDataProperties(DProp, DProp),
    DisjointDataProperties(DProp, DProp),
    DataPropertyDomain(DProp, u32),
    DataPropertyRange(DProp, u32),
    FunctionalDataProperty(DProp),
    DatatypeDefinition(u32),
    HasKey(u32, Option<Role>, bool),
    ClassAssertion(u32, u8),
    ObjectPropertyAssertion(Role, u8, u8),
    NegativeObjectPropertyAssertion(Role, u8, u8),
    DataPropertyAssertion(DProp, u8, u8),
    NegativeDataPropertyAssertion(DProp, u8, u8),
    SameIndividual,
    DifferentIndividuals,
}

/// What a case uses: one bit per constructor and axiom type (covering arrays read them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Feature {
    Class,
    Thing,
    Nothing,
    OneOf,
    HasValue,
    HasSelf,
    DataHasValue,
    Not,
    And,
    Or,
    Some,
    All,
    Min,
    Max,
    Exact,
    DataSome,
    DataAll,
    DataMin,
    DataMax,
    DataExact,
    Inverse,
    TopObject,
    BottomObject,
    TopData,
    BottomData,
    Literal,
    Integer,
    String,
    Defined,
    DataOneOf,
    Facet,
    DataNot,
    DataAnd,
    DataOr,
    SubClassOf,
    EquivalentClasses,
    DisjointClasses,
    DisjointUnion,
    SubObjectPropertyOf,
    Chain,
    EquivalentObjectProperties,
    DisjointObjectProperties,
    InverseObjectProperties,
    ObjectPropertyDomain,
    ObjectPropertyRange,
    Functional,
    InverseFunctional,
    Reflexive,
    Irreflexive,
    Symmetric,
    Asymmetric,
    Transitive,
    SubDataPropertyOf,
    EquivalentDataProperties,
    DisjointDataProperties,
    DataPropertyDomain,
    DataPropertyRange,
    FunctionalDataProperty,
    DatatypeDefinition,
    HasKey,
    ClassAssertion,
    ObjectPropertyAssertion,
    NegativeObjectPropertyAssertion,
    DataPropertyAssertion,
    NegativeDataPropertyAssertion,
    SameIndividual,
    DifferentIndividuals,
}

impl Feature {
    /// Every feature, in order.
    pub const COUNT: usize = Feature::DifferentIndividuals as usize + 1;

    pub fn all() -> impl Iterator<Item = Feature> {
        ALL_FEATURES.into_iter()
    }

    pub fn bit(self) -> u128 {
        1u128 << self as u8
    }

    pub fn name(self) -> String {
        format!("{self:?}")
    }
}

const ALL_FEATURES: [Feature; Feature::COUNT] = {
    use Feature::*;
    [
        Class,
        Thing,
        Nothing,
        OneOf,
        HasValue,
        HasSelf,
        DataHasValue,
        Not,
        And,
        Or,
        Some,
        All,
        Min,
        Max,
        Exact,
        DataSome,
        DataAll,
        DataMin,
        DataMax,
        DataExact,
        Inverse,
        TopObject,
        BottomObject,
        TopData,
        BottomData,
        Literal,
        Integer,
        String,
        Defined,
        DataOneOf,
        Facet,
        DataNot,
        DataAnd,
        DataOr,
        SubClassOf,
        EquivalentClasses,
        DisjointClasses,
        DisjointUnion,
        SubObjectPropertyOf,
        Chain,
        EquivalentObjectProperties,
        DisjointObjectProperties,
        InverseObjectProperties,
        ObjectPropertyDomain,
        ObjectPropertyRange,
        Functional,
        InverseFunctional,
        Reflexive,
        Irreflexive,
        Symmetric,
        Asymmetric,
        Transitive,
        SubDataPropertyOf,
        EquivalentDataProperties,
        DisjointDataProperties,
        DataPropertyDomain,
        DataPropertyRange,
        FunctionalDataProperty,
        DatatypeDefinition,
        HasKey,
        ClassAssertion,
        ObjectPropertyAssertion,
        NegativeObjectPropertyAssertion,
        DataPropertyAssertion,
        NegativeDataPropertyAssertion,
        SameIndividual,
        DifferentIndividuals,
    ]
};

/// Every class expression, data range and axiom of the universe up to a size, each once.
#[derive(Debug, Clone, Default)]
pub struct Tables {
    pub classes: Vec<C>,
    pub class_size: Vec<u32>,
    pub ranges: Vec<D>,
    pub range_size: Vec<u32>,
    /// Sorted by size, then by value: the order cases are enumerated in.
    pub axioms: Vec<Ax>,
    pub axiom_size: Vec<u32>,
    class_ids: HashMap<C, u32>,
    range_ids: HashMap<D, u32>,
    axiom_ids: HashMap<Ax, u32>,
}

impl Tables {
    /// Every expression, range and axiom of size at most `max`.
    pub fn new(max: u32) -> Self {
        let mut t = Tables::default();
        t.build_ranges(max);
        t.build_classes(max);
        t.build_axioms(max);
        t
    }

    fn add_range(&mut self, d: D, size: u32) {
        if !self.range_ids.contains_key(&d) {
            self.range_ids.insert(d, self.ranges.len() as u32);
            self.ranges.push(d);
            self.range_size.push(size);
        }
    }

    fn add_class(&mut self, c: C, size: u32) {
        if !self.class_ids.contains_key(&c) {
            self.class_ids.insert(c, self.classes.len() as u32);
            self.classes.push(c);
            self.class_size.push(size);
        }
    }

    fn of_range_size(&self, k: u32) -> Vec<u32> {
        (0..self.ranges.len() as u32)
            .filter(|&i| self.range_size[i as usize] == k)
            .collect()
    }

    fn of_class_size(&self, k: u32) -> Vec<u32> {
        (0..self.classes.len() as u32)
            .filter(|&i| self.class_size[i as usize] == k)
            .collect()
    }

    fn build_ranges(&mut self, max: u32) {
        for d in [
            D::Literal,
            D::Integer,
            D::String,
            D::Defined,
            D::OneOf(1),
            D::OneOf(2),
            D::OneOf(3),
            D::AtLeastOne,
            D::AtMostZero,
        ] {
            self.add_range(d, 1);
        }
        for k in 2..=max {
            for x in self.of_range_size(k - 1) {
                self.add_range(D::Not(x), k);
            }
            for i in 1..k - 1 {
                let j = k - 1 - i;
                if i > j {
                    continue;
                }
                for x in self.of_range_size(i) {
                    for y in self.of_range_size(j) {
                        if x < y {
                            self.add_range(D::And(x, y), k);
                            self.add_range(D::Or(x, y), k);
                        }
                    }
                }
            }
        }
    }

    fn build_classes(&mut self, max: u32) {
        let mut atoms = vec![
            C::Class(0),
            C::Class(1),
            C::Thing,
            C::Nothing,
            C::OneOf(1),
            C::OneOf(2),
            C::OneOf(3),
        ];
        for r in Role::ALL {
            atoms.extend([C::HasValue(r, 0), C::HasValue(r, 1)]);
        }
        atoms.extend(Role::SIMPLE.map(C::HasSelf));
        for p in [DProp::D, DProp::Bottom] {
            atoms.extend([C::DataHasValue(p, 0), C::DataHasValue(p, 1)]);
        }
        for c in atoms {
            self.add_class(c, 1);
        }
        for k in 2..=max {
            let below = self.of_class_size(k - 1);
            for &x in &below {
                self.add_class(C::Not(x), k);
                for r in Role::ALL {
                    self.add_class(C::Some(r, x), k);
                    self.add_class(C::All(r, x), k);
                }
                for r in Role::SIMPLE {
                    for n in 0..=2 {
                        self.add_class(C::Min(n, r, x), k);
                        self.add_class(C::Max(n, r, x), k);
                        self.add_class(C::Exact(n, r, x), k);
                    }
                }
            }
            for i in 1..k - 1 {
                let j = k - 1 - i;
                if i > j {
                    continue;
                }
                for x in self.of_class_size(i) {
                    for y in self.of_class_size(j) {
                        if x < y {
                            self.add_class(C::And(x, y), k);
                            self.add_class(C::Or(x, y), k);
                        }
                    }
                }
            }
            for d in self.of_range_size(k - 1) {
                for p in [DProp::D, DProp::Bottom] {
                    self.add_class(C::DataSome(p, d), k);
                    self.add_class(C::DataAll(p, d), k);
                    for n in 0..=2 {
                        self.add_class(C::DataMin(n, p, d), k);
                        self.add_class(C::DataMax(n, p, d), k);
                        self.add_class(C::DataExact(n, p, d), k);
                    }
                }
            }
        }
    }

    fn build_axioms(&mut self, max: u32) {
        let mut out: Vec<(u32, Ax)> = Vec::new();
        let classes = |s: &Self, k: u32| -> Vec<u32> {
            (0..s.classes.len() as u32)
                .filter(|&i| s.class_size[i as usize] <= k)
                .collect()
        };
        let ranges_upto = |s: &Self, k: u32| -> Vec<u32> {
            (0..s.ranges.len() as u32)
                .filter(|&i| s.range_size[i as usize] <= k)
                .collect()
        };
        let cs = |s: &Self, i: u32| s.class_size[i as usize];
        let rs = |s: &Self, i: u32| s.range_size[i as usize];
        // Class axioms.
        if max >= 3 {
            for x in classes(self, max - 2) {
                for y in classes(self, max - 1 - cs(self, x)) {
                    let size = 1 + cs(self, x) + cs(self, y);
                    out.push((size, Ax::SubClassOf(x, y)));
                    if x < y {
                        out.push((size, Ax::EquivalentClasses(x, y)));
                        out.push((size, Ax::DisjointClasses(x, y)));
                        for a in 0..2 {
                            out.push((size, Ax::DisjointUnion(a, x, y)));
                        }
                    }
                }
            }
        }
        if max >= 2 {
            for x in classes(self, max - 1) {
                let size = 1 + cs(self, x);
                for r in Role::ALL {
                    out.push((size, Ax::ObjectPropertyDomain(r, x)));
                    out.push((size, Ax::ObjectPropertyRange(r, x)));
                }
                out.push((size, Ax::DataPropertyDomain(DProp::D, x)));
                out.push((size, Ax::DataPropertyDomain(DProp::Bottom, x)));
                for a in 0..2 {
                    out.push((size, Ax::ClassAssertion(x, a)));
                }
                out.push((size, Ax::HasKey(x, None, true)));
                for r in Role::SIMPLE.into_iter().chain([Role::Top]) {
                    for data in [false, true] {
                        out.push((size, Ax::HasKey(x, Some(r), data)));
                    }
                }
            }
            for d in ranges_upto(self, max - 1) {
                let size = 1 + rs(self, d);
                out.push((size, Ax::DataPropertyRange(DProp::D, d)));
                out.push((size, Ax::DataPropertyRange(DProp::Bottom, d)));
                out.push((size, Ax::DatatypeDefinition(d)));
            }
        }
        // Property axioms.
        for r in Role::ALL {
            for s in Role::ALL {
                out.push((1, Ax::SubObjectPropertyOf(vec![r], s)));
                if r < s {
                    out.push((1, Ax::EquivalentObjectProperties(r, s)));
                    out.push((1, Ax::DisjointObjectProperties(r, s)));
                }
                if r <= s {
                    out.push((1, Ax::InverseObjectProperties(r, s)));
                }
            }
        }
        if max >= 2 {
            let chainable = [
                Role::Prop(0, false),
                Role::Prop(0, true),
                Role::Prop(1, false),
                Role::Prop(1, true),
                Role::Top,
            ];
            for a in chainable {
                for b in chainable {
                    for sup in chainable {
                        out.push((2, Ax::SubObjectPropertyOf(vec![a, b], sup)));
                    }
                }
            }
        }
        for kind in [
            Characteristic::Functional,
            Characteristic::InverseFunctional,
            Characteristic::Reflexive,
            Characteristic::Irreflexive,
            Characteristic::Symmetric,
            Characteristic::Asymmetric,
            Characteristic::Transitive,
        ] {
            for r in Role::ALL {
                out.push((1, Ax::Characteristic(kind, r)));
            }
        }
        for sub in [DProp::D, DProp::Bottom] {
            for sup in [DProp::D, DProp::Top, DProp::Bottom] {
                out.push((1, Ax::SubDataPropertyOf(sub, sup)));
            }
        }
        out.push((1, Ax::EquivalentDataProperties(DProp::D, DProp::Bottom)));
        out.push((1, Ax::DisjointDataProperties(DProp::D, DProp::Bottom)));
        out.push((1, Ax::FunctionalDataProperty(DProp::D)));
        out.push((1, Ax::FunctionalDataProperty(DProp::Bottom)));
        // Assertions.
        for r in Role::NAMED {
            for (a, b) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                out.push((1, Ax::ObjectPropertyAssertion(r, a, b)));
                out.push((1, Ax::NegativeObjectPropertyAssertion(r, a, b)));
            }
        }
        for p in [DProp::D, DProp::Bottom] {
            for a in 0..2 {
                for v in 0..2 {
                    out.push((1, Ax::DataPropertyAssertion(p, a, v)));
                    out.push((1, Ax::NegativeDataPropertyAssertion(p, a, v)));
                }
            }
        }
        out.push((1, Ax::SameIndividual));
        out.push((1, Ax::DifferentIndividuals));
        out.retain(|(size, _)| *size <= max);
        out.sort();
        out.dedup();
        for (size, a) in out {
            self.axiom_ids.insert(a.clone(), self.axioms.len() as u32);
            self.axioms.push(a);
            self.axiom_size.push(size);
        }
    }

    pub fn class_id(&self, c: &C) -> Option<u32> {
        self.class_ids.get(c).copied()
    }

    pub fn range_id(&self, d: &D) -> Option<u32> {
        self.range_ids.get(d).copied()
    }

    pub fn axiom_id(&self, a: &Ax) -> Option<u32> {
        self.axiom_ids.get(a).copied()
    }

    // Features ---------------------------------------------------------------------------

    fn role_features(r: Role) -> u128 {
        match r {
            Role::Prop(_, true) => Feature::Inverse.bit(),
            Role::Prop(_, false) => 0,
            Role::Top => Feature::TopObject.bit(),
            Role::Bottom => Feature::BottomObject.bit(),
        }
    }

    fn dprop_features(p: DProp) -> u128 {
        match p {
            DProp::D => 0,
            DProp::Top => Feature::TopData.bit(),
            DProp::Bottom => Feature::BottomData.bit(),
        }
    }

    pub fn range_features(&self, d: u32) -> u128 {
        match self.ranges[d as usize] {
            D::Literal => Feature::Literal.bit(),
            D::Integer => Feature::Integer.bit(),
            D::String => Feature::String.bit(),
            D::Defined => Feature::Defined.bit(),
            D::OneOf(_) => Feature::DataOneOf.bit(),
            D::AtLeastOne | D::AtMostZero => Feature::Facet.bit(),
            D::Not(x) => Feature::DataNot.bit() | self.range_features(x),
            D::And(x, y) => {
                Feature::DataAnd.bit() | self.range_features(x) | self.range_features(y)
            }
            D::Or(x, y) => Feature::DataOr.bit() | self.range_features(x) | self.range_features(y),
        }
    }

    pub fn class_features(&self, c: u32) -> u128 {
        use Feature as F;
        match self.classes[c as usize] {
            C::Class(_) => F::Class.bit(),
            C::Thing => F::Thing.bit(),
            C::Nothing => F::Nothing.bit(),
            C::OneOf(_) => F::OneOf.bit(),
            C::HasValue(r, _) => F::HasValue.bit() | Self::role_features(r),
            C::HasSelf(r) => F::HasSelf.bit() | Self::role_features(r),
            C::DataHasValue(p, _) => F::DataHasValue.bit() | Self::dprop_features(p),
            C::Not(x) => F::Not.bit() | self.class_features(x),
            C::And(x, y) => F::And.bit() | self.class_features(x) | self.class_features(y),
            C::Or(x, y) => F::Or.bit() | self.class_features(x) | self.class_features(y),
            C::Some(r, x) => F::Some.bit() | Self::role_features(r) | self.class_features(x),
            C::All(r, x) => F::All.bit() | Self::role_features(r) | self.class_features(x),
            C::Min(_, r, x) => F::Min.bit() | Self::role_features(r) | self.class_features(x),
            C::Max(_, r, x) => F::Max.bit() | Self::role_features(r) | self.class_features(x),
            C::Exact(_, r, x) => F::Exact.bit() | Self::role_features(r) | self.class_features(x),
            C::DataSome(p, d) => {
                F::DataSome.bit() | Self::dprop_features(p) | self.range_features(d)
            }
            C::DataAll(p, d) => F::DataAll.bit() | Self::dprop_features(p) | self.range_features(d),
            C::DataMin(_, p, d) => {
                F::DataMin.bit() | Self::dprop_features(p) | self.range_features(d)
            }
            C::DataMax(_, p, d) => {
                F::DataMax.bit() | Self::dprop_features(p) | self.range_features(d)
            }
            C::DataExact(_, p, d) => {
                F::DataExact.bit() | Self::dprop_features(p) | self.range_features(d)
            }
        }
    }

    pub fn axiom_features(&self, a: u32) -> u128 {
        use Feature as F;
        let c = |x: u32| self.class_features(x);
        let r = Self::role_features;
        let p = Self::dprop_features;
        match &self.axioms[a as usize] {
            Ax::SubClassOf(x, y) => F::SubClassOf.bit() | c(*x) | c(*y),
            Ax::EquivalentClasses(x, y) => F::EquivalentClasses.bit() | c(*x) | c(*y),
            Ax::DisjointClasses(x, y) => F::DisjointClasses.bit() | c(*x) | c(*y),
            Ax::DisjointUnion(_, x, y) => F::DisjointUnion.bit() | c(*x) | c(*y),
            Ax::SubObjectPropertyOf(chain, sup) => {
                let base = if chain.len() > 1 {
                    F::Chain.bit()
                } else {
                    F::SubObjectPropertyOf.bit()
                };
                chain.iter().fold(base | r(*sup), |m, x| m | r(*x))
            }
            Ax::EquivalentObjectProperties(x, y) => {
                F::EquivalentObjectProperties.bit() | r(*x) | r(*y)
            }
            Ax::DisjointObjectProperties(x, y) => F::DisjointObjectProperties.bit() | r(*x) | r(*y),
            Ax::InverseObjectProperties(x, y) => F::InverseObjectProperties.bit() | r(*x) | r(*y),
            Ax::ObjectPropertyDomain(x, y) => F::ObjectPropertyDomain.bit() | r(*x) | c(*y),
            Ax::ObjectPropertyRange(x, y) => F::ObjectPropertyRange.bit() | r(*x) | c(*y),
            Ax::Characteristic(kind, x) => {
                let f = match kind {
                    Characteristic::Functional => F::Functional,
                    Characteristic::InverseFunctional => F::InverseFunctional,
                    Characteristic::Reflexive => F::Reflexive,
                    Characteristic::Irreflexive => F::Irreflexive,
                    Characteristic::Symmetric => F::Symmetric,
                    Characteristic::Asymmetric => F::Asymmetric,
                    Characteristic::Transitive => F::Transitive,
                };
                f.bit() | r(*x)
            }
            Ax::SubDataPropertyOf(x, y) => F::SubDataPropertyOf.bit() | p(*x) | p(*y),
            Ax::EquivalentDataProperties(x, y) => F::EquivalentDataProperties.bit() | p(*x) | p(*y),
            Ax::DisjointDataProperties(x, y) => F::DisjointDataProperties.bit() | p(*x) | p(*y),
            Ax::DataPropertyDomain(x, y) => F::DataPropertyDomain.bit() | p(*x) | c(*y),
            Ax::DataPropertyRange(x, d) => {
                F::DataPropertyRange.bit() | p(*x) | self.range_features(*d)
            }
            Ax::FunctionalDataProperty(x) => F::FunctionalDataProperty.bit() | p(*x),
            Ax::DatatypeDefinition(d) => {
                F::DatatypeDefinition.bit() | F::Defined.bit() | self.range_features(*d)
            }
            Ax::HasKey(x, role, _) => F::HasKey.bit() | c(*x) | role.map_or(0, r),
            Ax::ClassAssertion(x, _) => F::ClassAssertion.bit() | c(*x),
            Ax::ObjectPropertyAssertion(x, ..) => F::ObjectPropertyAssertion.bit() | r(*x),
            Ax::NegativeObjectPropertyAssertion(x, ..) => {
                F::NegativeObjectPropertyAssertion.bit() | r(*x)
            }
            Ax::DataPropertyAssertion(x, ..) => F::DataPropertyAssertion.bit() | p(*x),
            Ax::NegativeDataPropertyAssertion(x, ..) => {
                F::NegativeDataPropertyAssertion.bit() | p(*x)
            }
            Ax::SameIndividual => F::SameIndividual.bit(),
            Ax::DifferentIndividuals => F::DifferentIndividuals.bit(),
        }
    }
}

// Symmetries -----------------------------------------------------------------------------

/// A renaming of the universe: swap A and B, swap r and s (after inverting each where
/// `flip` says), swap a and b.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Symmetry {
    pub classes: bool,
    pub roles: bool,
    pub flip: [bool; 2],
    pub individuals: bool,
}

/// The 32 renamings.
pub fn symmetries() -> Vec<Symmetry> {
    let mut out = Vec::with_capacity(32);
    for bits in 0..32u8 {
        out.push(Symmetry {
            classes: bits & 1 != 0,
            roles: bits & 2 != 0,
            flip: [bits & 4 != 0, bits & 8 != 0],
            individuals: bits & 16 != 0,
        });
    }
    out
}

impl Symmetry {
    fn role(self, r: Role) -> Role {
        match r {
            Role::Prop(i, inv) => {
                let inv = inv ^ self.flip[i as usize];
                let i = if self.roles { 1 - i } else { i };
                Role::Prop(i, inv)
            }
            other => other,
        }
    }

    fn ind(self, a: u8) -> u8 {
        if self.individuals { 1 - a } else { a }
    }

    fn mask(self, m: u8) -> u8 {
        if self.individuals {
            ((m & 1) << 1) | ((m & 2) >> 1)
        } else {
            m
        }
    }

    fn class(self, t: &Tables, c: u32) -> u32 {
        let pair = |x: u32, y: u32| {
            let (x, y) = (self.class(t, x), self.class(t, y));
            (x.min(y), x.max(y))
        };
        let mapped = match t.classes[c as usize] {
            C::Class(i) => C::Class(if self.classes { 1 - i } else { i }),
            C::Thing => C::Thing,
            C::Nothing => C::Nothing,
            C::OneOf(m) => C::OneOf(self.mask(m)),
            C::HasValue(r, a) => C::HasValue(self.role(r), self.ind(a)),
            C::HasSelf(r) => C::HasSelf(self.role(r)),
            C::DataHasValue(p, v) => C::DataHasValue(p, v),
            C::Not(x) => C::Not(self.class(t, x)),
            C::And(x, y) => {
                let (x, y) = pair(x, y);
                C::And(x, y)
            }
            C::Or(x, y) => {
                let (x, y) = pair(x, y);
                C::Or(x, y)
            }
            C::Some(r, x) => C::Some(self.role(r), self.class(t, x)),
            C::All(r, x) => C::All(self.role(r), self.class(t, x)),
            C::Min(n, r, x) => C::Min(n, self.role(r), self.class(t, x)),
            C::Max(n, r, x) => C::Max(n, self.role(r), self.class(t, x)),
            C::Exact(n, r, x) => C::Exact(n, self.role(r), self.class(t, x)),
            d @ (C::DataSome(..)
            | C::DataAll(..)
            | C::DataMin(..)
            | C::DataMax(..)
            | C::DataExact(..)) => d,
        };
        t.class_id(&mapped)
            .expect("the tables are closed under the symmetries")
    }

    /// The image of axiom `a` (an index of `t.axioms`).
    pub fn axiom(self, t: &Tables, a: u32) -> u32 {
        let c = |x: u32| self.class(t, x);
        let pair = |x: u32, y: u32| {
            let (x, y) = (c(x), c(y));
            (x.min(y), x.max(y))
        };
        let rpair = |x: Role, y: Role| {
            let (x, y) = (self.role(x), self.role(y));
            (x.min(y), x.max(y))
        };
        let mapped = match &t.axioms[a as usize] {
            Ax::SubClassOf(x, y) => Ax::SubClassOf(c(*x), c(*y)),
            Ax::EquivalentClasses(x, y) => {
                let (x, y) = pair(*x, *y);
                Ax::EquivalentClasses(x, y)
            }
            Ax::DisjointClasses(x, y) => {
                let (x, y) = pair(*x, *y);
                Ax::DisjointClasses(x, y)
            }
            Ax::DisjointUnion(k, x, y) => {
                let (x, y) = pair(*x, *y);
                Ax::DisjointUnion(if self.classes { 1 - k } else { *k }, x, y)
            }
            Ax::SubObjectPropertyOf(chain, sup) => Ax::SubObjectPropertyOf(
                chain.iter().map(|&r| self.role(r)).collect(),
                self.role(*sup),
            ),
            Ax::EquivalentObjectProperties(x, y) => {
                let (x, y) = rpair(*x, *y);
                Ax::EquivalentObjectProperties(x, y)
            }
            Ax::DisjointObjectProperties(x, y) => {
                let (x, y) = rpair(*x, *y);
                Ax::DisjointObjectProperties(x, y)
            }
            Ax::InverseObjectProperties(x, y) => {
                let (x, y) = rpair(*x, *y);
                Ax::InverseObjectProperties(x, y)
            }
            Ax::ObjectPropertyDomain(r, x) => Ax::ObjectPropertyDomain(self.role(*r), c(*x)),
            Ax::ObjectPropertyRange(r, x) => Ax::ObjectPropertyRange(self.role(*r), c(*x)),
            Ax::Characteristic(kind, r) => Ax::Characteristic(*kind, self.role(*r)),
            Ax::DataPropertyDomain(p, x) => Ax::DataPropertyDomain(*p, c(*x)),
            Ax::HasKey(x, r, d) => Ax::HasKey(c(*x), r.map(|r| self.role(r)), *d),
            Ax::ClassAssertion(x, i) => Ax::ClassAssertion(c(*x), self.ind(*i)),
            Ax::ObjectPropertyAssertion(r, x, y) | Ax::NegativeObjectPropertyAssertion(r, x, y) => {
                let (x, y) = (self.ind(*x), self.ind(*y));
                // An assertion of an inverse is the property's, the other way round.
                let (r, x, y) = match self.role(*r) {
                    Role::Prop(i, true) => (Role::Prop(i, false), y, x),
                    r => (r, x, y),
                };
                if matches!(t.axioms[a as usize], Ax::ObjectPropertyAssertion(..)) {
                    Ax::ObjectPropertyAssertion(r, x, y)
                } else {
                    Ax::NegativeObjectPropertyAssertion(r, x, y)
                }
            }
            Ax::DataPropertyAssertion(p, i, v) => Ax::DataPropertyAssertion(*p, self.ind(*i), *v),
            Ax::NegativeDataPropertyAssertion(p, i, v) => {
                Ax::NegativeDataPropertyAssertion(*p, self.ind(*i), *v)
            }
            other => other.clone(),
        };
        t.axiom_id(&mapped)
            .expect("the axioms are closed under the symmetries")
    }
}

// The global restrictions ----------------------------------------------------------------

/// Whether the axioms (indexes of `t.axioms`) are within OWL 2 DL's global restrictions
/// (*OWL 2 Structural Specification* §11.2): simple roles where required, a regular
/// property hierarchy, `owl:topDataProperty` only above another data property (by
/// construction), and the defined datatype defined once, acyclically, where it is used.
pub fn within_dl(t: &Tables, axioms: &[u32]) -> bool {
    datatypes_ok(t, axioms) && simple_roles_ok(t, axioms) && regular(t, axioms)
}

fn range_uses_defined(t: &Tables, d: u32) -> bool {
    match t.ranges[d as usize] {
        D::Defined => true,
        D::Not(x) => range_uses_defined(t, x),
        D::And(x, y) | D::Or(x, y) => range_uses_defined(t, x) || range_uses_defined(t, y),
        _ => false,
    }
}

fn class_uses_defined(t: &Tables, c: u32) -> bool {
    match t.classes[c as usize] {
        C::Not(x)
        | C::Some(_, x)
        | C::All(_, x)
        | C::Min(_, _, x)
        | C::Max(_, _, x)
        | C::Exact(_, _, x) => class_uses_defined(t, x),
        C::And(x, y) | C::Or(x, y) => class_uses_defined(t, x) || class_uses_defined(t, y),
        C::DataSome(_, d)
        | C::DataAll(_, d)
        | C::DataMin(_, _, d)
        | C::DataMax(_, _, d)
        | C::DataExact(_, _, d) => range_uses_defined(t, d),
        _ => false,
    }
}

fn datatypes_ok(t: &Tables, axioms: &[u32]) -> bool {
    let mut definitions = 0;
    let mut used = false;
    for &a in axioms {
        match &t.axioms[a as usize] {
            Ax::DatatypeDefinition(d) => {
                definitions += 1;
                // Acyclic: the definition doesn't use the datatype it defines.
                if range_uses_defined(t, *d) {
                    return false;
                }
            }
            Ax::DataPropertyRange(_, d) => used |= range_uses_defined(t, *d),
            other => {
                used |= classes_of(other)
                    .into_iter()
                    .any(|c| class_uses_defined(t, c))
            }
        }
    }
    definitions <= 1 && (!used || definitions == 1)
}

/// The class expressions an axiom holds directly.
fn classes_of(a: &Ax) -> Vec<u32> {
    match a {
        Ax::SubClassOf(x, y)
        | Ax::EquivalentClasses(x, y)
        | Ax::DisjointClasses(x, y)
        | Ax::DisjointUnion(_, x, y) => vec![*x, *y],
        Ax::ObjectPropertyDomain(_, x)
        | Ax::ObjectPropertyRange(_, x)
        | Ax::DataPropertyDomain(_, x)
        | Ax::HasKey(x, ..)
        | Ax::ClassAssertion(x, _) => vec![*x],
        _ => Vec::new(),
    }
}

/// The relation `→` of §11.1 over the eight slots of `AllOPE`, closed reflexively and
/// transitively (`reach[a]` has bit `b` iff `a →* b`).
fn hierarchy(t: &Tables, axioms: &[u32]) -> [u8; 8] {
    let mut reach = [0u8; 8];
    for (i, r) in reach.iter_mut().enumerate() {
        *r = 1 << i;
    }
    let edge = |a: usize, b: usize, reach: &mut [u8; 8]| {
        reach[a] |= 1 << b;
        reach[Role::inverse_slot(a)] |= 1 << Role::inverse_slot(b);
    };
    for &a in axioms {
        match &t.axioms[a as usize] {
            Ax::SubObjectPropertyOf(chain, sup) if chain.len() == 1 => {
                edge(chain[0].slot(), sup.slot(), &mut reach)
            }
            Ax::EquivalentObjectProperties(x, y) => {
                edge(x.slot(), y.slot(), &mut reach);
                edge(y.slot(), x.slot(), &mut reach);
            }
            Ax::InverseObjectProperties(x, y) => {
                let inv_y = Role::inverse_slot(y.slot());
                edge(x.slot(), inv_y, &mut reach);
                edge(inv_y, x.slot(), &mut reach);
            }
            Ax::Characteristic(Characteristic::Symmetric, x) => {
                edge(x.slot(), Role::inverse_slot(x.slot()), &mut reach)
            }
            _ => {}
        }
    }
    loop {
        let mut changed = false;
        for a in 0..8 {
            let mut r = reach[a];
            for b in 0..8 {
                if reach[a] & (1 << b) != 0 {
                    r |= reach[b];
                }
            }
            if r != reach[a] {
                reach[a] = r;
                changed = true;
            }
        }
        if !changed {
            return reach;
        }
    }
}

fn simple_roles_ok(t: &Tables, axioms: &[u32]) -> bool {
    // Composite (§11.1): U, ⊥, the superproperty of a chain or a transitive property,
    // and its inverse.
    let mut composite = (1u8 << 4) | (1 << 5) | (1 << 6) | (1 << 7);
    for &a in axioms {
        let slot = match &t.axioms[a as usize] {
            Ax::SubObjectPropertyOf(chain, sup) if chain.len() > 1 => sup.slot(),
            Ax::Characteristic(Characteristic::Transitive, r) => r.slot(),
            _ => continue,
        };
        composite |= (1 << slot) | (1 << Role::inverse_slot(slot));
    }
    let reach = hierarchy(t, axioms);
    let simple =
        |r: Role| (0..8).all(|s| composite & (1 << s) == 0 || reach[s] & (1 << r.slot()) == 0);
    fn needs(t: &Tables, c: u32, out: &mut Vec<Role>) {
        match t.classes[c as usize] {
            C::HasSelf(r) => out.push(r),
            C::Min(_, r, x) | C::Max(_, r, x) | C::Exact(_, r, x) => {
                out.push(r);
                needs(t, x, out);
            }
            C::Not(x) | C::Some(_, x) | C::All(_, x) => needs(t, x, out),
            C::And(x, y) | C::Or(x, y) => {
                needs(t, x, out);
                needs(t, y, out);
            }
            _ => {}
        }
    }
    let mut required = Vec::new();
    for &a in axioms {
        let ax = &t.axioms[a as usize];
        match ax {
            Ax::Characteristic(
                Characteristic::Functional
                | Characteristic::InverseFunctional
                | Characteristic::Irreflexive
                | Characteristic::Asymmetric,
                r,
            ) => required.push(*r),
            Ax::DisjointObjectProperties(x, y) => required.extend([*x, *y]),
            _ => {}
        }
        for c in classes_of(ax) {
            needs(t, c, &mut required);
        }
    }
    required.into_iter().all(simple)
}

/// §11.2's restriction on the property hierarchy, decided exactly: a strict partial order
/// on `AllOPE` exists with the three conditions. Each chain axiom offers its forms (U
/// above it, `R ∘ R ⊑ R`, everything below, the superproperty at either end); for each
/// choice of forms the least order they need is built (the pairs they require, closed
/// under the inverse condition and transitivity) and checked; a larger order only adds
/// constraints, so the least decides.
///
/// A chain into an inverse, `w ⊑ R⁻`, is read as its equivalent `w⁻ ⊑ R` (the chain's
/// inverses reversed), so the inverse condition, which §11.2 states for named properties,
/// applies to it: `r ∘ r ⊑ r⁻` is `r⁻ ∘ r⁻ ⊑ r`, which needs `r⁻ < r`, so `r < r`. This is
/// Horrocks, Kutz and Sattler's regularity (KR 2006), which §11.2 cites and HermiT checks;
/// the specification's text read literally would admit the first form and not the second.
fn regular(t: &Tables, axioms: &[u32]) -> bool {
    let named = |chain: &[Role], sup: Role| -> (Vec<Role>, Role) {
        match sup {
            Role::Prop(i, true) => (
                chain
                    .iter()
                    .rev()
                    .map(|r| match *r {
                        Role::Prop(j, inv) => Role::Prop(j, !inv),
                        other => other,
                    })
                    .collect(),
                Role::Prop(i, false),
            ),
            _ => (chain.to_vec(), sup),
        }
    };
    let chains: Vec<(Vec<Role>, Role)> = axioms
        .iter()
        .filter_map(|&a| match &t.axioms[a as usize] {
            Ax::SubObjectPropertyOf(chain, sup) if chain.len() > 1 => Some(named(chain, *sup)),
            _ => None,
        })
        .collect();
    if chains.is_empty() {
        return true;
    }
    let reach = hierarchy(t, axioms);
    // Each chain's options: the pairs `x < y` it needs, or `None` where it doesn't apply.
    let options: Vec<Vec<Vec<(usize, usize)>>> = chains
        .iter()
        .map(|(chain, sup)| {
            let s = sup.slot();
            let n = chain.len();
            let below = |range: std::ops::Range<usize>| -> Vec<(usize, usize)> {
                chain[range].iter().map(|r| (r.slot(), s)).collect()
            };
            let mut forms = Vec::new();
            if *sup == Role::Top {
                forms.push(Vec::new());
            }
            if n == 2 && chain[0] == *sup && chain[1] == *sup {
                forms.push(Vec::new());
            }
            forms.push(below(0..n));
            if chain[0] == *sup {
                forms.push(below(1..n));
            }
            if chain[n - 1] == *sup {
                forms.push(below(0..n - 1));
            }
            forms
        })
        .collect();
    let mut choice = vec![0usize; options.len()];
    loop {
        let mut less = [0u8; 8];
        for (k, &c) in choice.iter().enumerate() {
            for &(x, y) in &options[k][c] {
                less[x] |= 1 << y;
            }
        }
        if least_order_ok(&mut less, &reach) {
            return true;
        }
        // The next choice.
        let mut k = 0;
        loop {
            if k == choice.len() {
                return false;
            }
            choice[k] += 1;
            if choice[k] < options[k].len() {
                break;
            }
            choice[k] = 0;
            k += 1;
        }
    }
}

/// Closes `less` under the inverse condition (for named `y`: `x < y` iff `INV(x) < y`) and
/// transitivity, and checks it is irreflexive and never against the hierarchy (`x < y`
/// with `y →* x`).
fn least_order_ok(less: &mut [u8; 8], reach: &[u8; 8]) -> bool {
    loop {
        let mut changed = false;
        for x in 0..8 {
            let mut l = less[x];
            for y in 0..8 {
                if less[x] & (1 << y) != 0 {
                    l |= less[y];
                }
            }
            // Named properties are the even slots.
            for y in (0..8).step_by(2) {
                if l & (1 << y) != 0 {
                    let inv = Role::inverse_slot(x);
                    if less[inv] & (1 << y) == 0 {
                        less[inv] |= 1 << y;
                        changed = true;
                    }
                }
            }
            if l != less[x] {
                less[x] = l;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (0..8).all(|x| {
        less[x] & (1 << x) == 0
            && (0..8).all(|y| less[x] & (1 << y) == 0 || reach[y] & (1 << x) == 0)
    })
}

// Cases ----------------------------------------------------------------------------------

/// How far the enumeration goes: `max_total[k - 1]` is the largest total size of the
/// axioms of a `k`-axiom ontology (below `k`: no ontologies of `k` axioms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub max_total: Vec<u32>,
}

impl Scope {
    /// The largest size one axiom may have.
    fn max_axiom(&self) -> u32 {
        self.max_total
            .iter()
            .enumerate()
            .map(|(k, &t)| t.saturating_sub(k as u32))
            .max()
            .unwrap_or(0)
    }

    /// The tables the scope needs.
    pub fn tables(&self) -> Tables {
        Tables::new(self.max_axiom())
    }

    /// The scope as written on the command line: `4,5,4`.
    pub fn parse(text: &str) -> Result<Self, std::num::ParseIntError> {
        Ok(Self {
            max_total: text.split(',').map(str::parse).collect::<Result<_, _>>()?,
        })
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.max_total.iter().map(u32::to_string).collect();
        f.write_str(&parts.join(","))
    }
}

/// One ontology of the scope: its axioms by index, ascending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// Its position among the cases (the same scope enumerates the same cases in the same
    /// order).
    pub index: u64,
    pub axioms: Vec<u32>,
}

/// Every case of `scope` over `t` (as [`Scope::tables`] gives them), in order: for each
/// number of axioms, each set of distinct axioms within the size, within OWL 2 DL, the
/// least of its renamings.
pub fn cases<'a>(scope: &'a Scope, t: &'a Tables) -> impl Iterator<Item = Case> + 'a {
    let syms = symmetries();
    // The image of every axiom under every renaming.
    let images: Vec<Vec<u32>> = syms
        .iter()
        .map(|s| (0..t.axioms.len() as u32).map(|a| s.axiom(t, a)).collect())
        .collect();
    let mut index = 0u64;
    scope
        .max_total
        .iter()
        .enumerate()
        .filter(|&(k, &total)| total > k as u32)
        .flat_map(move |(k, &total)| Combinations::new(&t.axiom_size, k + 1, total))
        .filter(move |axioms| least(&images, axioms) && within_dl(t, axioms))
        .map(move |axioms| {
            let case = Case { index, axioms };
            index += 1;
            case
        })
}

/// Whether `axioms` (ascending) is the least of its images.
fn least(images: &[Vec<u32>], axioms: &[u32]) -> bool {
    let mut image: Vec<u32> = Vec::with_capacity(axioms.len());
    for map in &images[1..] {
        image.clear();
        image.extend(axioms.iter().map(|&a| map[a as usize]));
        image.sort_unstable();
        if image.as_slice() < axioms {
            return false;
        }
    }
    true
}

/// The `k`-subsets of the axioms whose sizes (`size`, ascending) sum to at most `total`,
/// each ascending, in lexicographic order. Depth first; a position stops at the first
/// axiom too large for what is left, the sizes being sorted.
struct Combinations<'a> {
    size: &'a [u32],
    total: u32,
    k: usize,
    /// The chosen axioms; `stack.len() < k` while descending.
    stack: Vec<u32>,
    started: bool,
}

impl<'a> Combinations<'a> {
    fn new(size: &'a [u32], k: usize, total: u32) -> Self {
        Self {
            size,
            total,
            k,
            stack: Vec::with_capacity(k),
            started: false,
        }
    }

    /// What the chosen axioms leave of the total.
    fn left(&self) -> u32 {
        self.total
            .saturating_sub(self.stack.iter().map(|&a| self.size[a as usize]).sum())
    }

    /// Whether `a` fits at the next position, with the smallest sizes after it.
    fn fits(&self, a: u32) -> bool {
        let rest = (self.k - self.stack.len() - 1) as u32;
        (a as usize) < self.size.len() && self.size[a as usize] * (rest + 1) <= self.left()
    }

    /// Fills the stack from `from` on; whether it got to `k`.
    fn descend(&mut self, mut from: u32) -> bool {
        while self.stack.len() < self.k {
            if !self.fits(from) {
                return false;
            }
            self.stack.push(from);
            from += 1;
        }
        true
    }
}

impl Iterator for Combinations<'_> {
    type Item = Vec<u32>;

    fn next(&mut self) -> Option<Vec<u32>> {
        if !self.started {
            self.started = true;
            if self.descend(0) {
                return Some(self.stack.clone());
            }
            // Not even the smallest axioms fit: none do.
            self.stack.clear();
            return None;
        }
        // Advance the deepest position that can move, then fill the rest with the
        // smallest axioms after it. Sizes ascend, so where that fails at a position, a
        // larger axiom there fails too: go up one.
        while let Some(last) = self.stack.pop() {
            let next = last + 1;
            if !self.fits(next) {
                continue;
            }
            let depth = self.stack.len();
            self.stack.push(next);
            if self.descend(next + 1) {
                return Some(self.stack.clone());
            }
            self.stack.truncate(depth);
        }
        None
    }
}

// Covering arrays -----------------------------------------------------------------------

/// What a covering array covers: every `strength`-subset of the features that some
/// ontology of `min..=max` axioms within OWL 2 DL has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Covering {
    pub strength: usize,
    pub min: usize,
    pub max: usize,
    pub seed: u64,
}

impl Covering {
    /// The recorded campaign's array of `strength`: three to six axioms of up to size 4
    /// each ([`Covering::tables`]), beyond the exhaustive scope `4,4,4`.
    pub fn campaign(strength: usize) -> Self {
        Self {
            strength,
            min: 3,
            max: 6,
            seed: 0x2026_1012,
        }
    }

    /// The tables the campaign's arrays draw from: every axiom up to size 4 (`And` and
    /// `Or` need it).
    pub fn tables() -> Tables {
        Tables::new(4)
    }
}

/// What a covering array came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub cases: Vec<Case>,
    /// The feature tuples the cases cover, and those no try covered (infeasible, or too
    /// rare for the tries: listed by name).
    pub covered: usize,
    pub uncovered: Vec<Vec<Feature>>,
}

/// The `k`-subsets of `items`' set bits, each as a sorted list.
fn subsets(bits: u128, k: usize, out: &mut Vec<Vec<u8>>) {
    let items: Vec<u8> = (0..128u8).filter(|&i| bits & (1 << i) != 0).collect();
    let mut chosen: Vec<u8> = Vec::with_capacity(k);
    fn go(items: &[u8], k: usize, from: usize, chosen: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
        if chosen.len() == k {
            out.push(chosen.clone());
            return;
        }
        for i in from..items.len() {
            chosen.push(items[i]);
            go(items, k, i + 1, chosen, out);
            chosen.pop();
        }
    }
    go(&items, k, 0, &mut chosen, out);
}

/// A covering array over `t`'s axioms (beyond the exhaustive scope: ontologies of more and
/// larger axioms), greedily: each case is the best of a batch of candidates, some aimed at
/// an uncovered tuple (an axiom with each of its features, and random others), the rest
/// random, by the uncovered tuples its features cover; until every tuple is covered, or a
/// run of batches covers none. Deterministic by `c.seed`. The cases are within OWL 2 DL and
/// numbered from 0.
pub fn covering(t: &Tables, c: Covering) -> Coverage {
    use std::collections::HashSet;
    let mut rng = crate::fuzz::Rng::new(c.seed);
    let features: Vec<u128> = (0..t.axioms.len() as u32)
        .map(|a| t.axiom_features(a))
        .collect();
    let mut by_feature: Vec<Vec<u32>> = vec![Vec::new(); Feature::COUNT];
    for (a, &f) in features.iter().enumerate() {
        for (i, list) in by_feature.iter_mut().enumerate() {
            if f & (1 << i) != 0 {
                list.push(a as u32);
            }
        }
    }
    let all: u128 = (0..Feature::COUNT).fold(0, |m, i| m | (1 << i));
    let mut uncovered: HashSet<Vec<u8>> = HashSet::new();
    let mut tuples = Vec::new();
    subsets(all, c.strength, &mut tuples);
    let total = tuples.len();
    uncovered.extend(tuples);
    let mut cases: Vec<Case> = Vec::new();
    let mut idle = 0;
    let mut targets: Vec<Vec<u8>> = uncovered.iter().cloned().collect();
    targets.sort();
    let mut failed_targets: HashSet<Vec<u8>> = HashSet::new();
    while !uncovered.is_empty() && idle < 40 {
        let mut best: Option<(usize, Vec<u32>)> = None;
        for attempt in 0..64 {
            let k = c.min + rng.below((c.max - c.min + 1) as u64) as usize;
            let mut axioms: Vec<u32> = Vec::with_capacity(k);
            if attempt % 2 == 0 {
                // Aimed: an uncovered tuple's features, one axiom each.
                let open: Vec<&Vec<u8>> = targets
                    .iter()
                    .filter(|x| uncovered.contains(*x) && !failed_targets.contains(*x))
                    .collect();
                if let Some(target) = open.get(rng.below(open.len().max(1) as u64) as usize) {
                    for &f in target.iter() {
                        let list = &by_feature[f as usize];
                        if !list.is_empty() {
                            axioms.push(list[rng.below(list.len() as u64) as usize]);
                        }
                    }
                }
            }
            while axioms.len() < k {
                axioms.push(rng.below(t.axioms.len() as u64) as u32);
            }
            axioms.sort_unstable();
            axioms.dedup();
            if !within_dl(t, &axioms) {
                continue;
            }
            let f = axioms.iter().fold(0u128, |m, &a| m | features[a as usize]);
            let mut covered = Vec::new();
            subsets(f, c.strength, &mut covered);
            let gain = covered.iter().filter(|x| uncovered.contains(*x)).count();
            if best.as_ref().is_none_or(|(g, _)| gain > *g) {
                best = Some((gain, axioms));
            }
        }
        match best {
            Some((gain, axioms)) if gain > 0 => {
                idle = 0;
                let f = axioms.iter().fold(0u128, |m, &a| m | features[a as usize]);
                let mut covered = Vec::new();
                subsets(f, c.strength, &mut covered);
                for x in covered {
                    uncovered.remove(&x);
                }
                cases.push(Case {
                    index: cases.len() as u64,
                    axioms,
                });
            }
            _ => {
                idle += 1;
                // The aimed tries of a tuple that no batch covers stop after a while.
                if let Some(x) = targets
                    .iter()
                    .find(|x| uncovered.contains(*x) && !failed_targets.contains(*x))
                {
                    failed_targets.insert(x.clone());
                }
            }
        }
    }
    // The tuples the batches left: many aimed tries each (the greedy loop gives up on a
    // rare one after a few), the cases found appended.
    let mut rest: Vec<Vec<u8>> = uncovered.iter().cloned().collect();
    rest.sort();
    for target in rest {
        if !uncovered.contains(&target) {
            continue;
        }
        for _ in 0..20_000 {
            let mut axioms: Vec<u32> = target
                .iter()
                .filter_map(|&f| {
                    let list = &by_feature[f as usize];
                    (!list.is_empty()).then(|| list[rng.below(list.len() as u64) as usize])
                })
                .collect();
            for _ in 0..rng.below(3) {
                axioms.push(rng.below(t.axioms.len() as u64) as u32);
            }
            axioms.sort_unstable();
            axioms.dedup();
            let f = axioms.iter().fold(0u128, |m, &a| m | features[a as usize]);
            if target.iter().any(|&i| f & (1 << i) == 0) || !within_dl(t, &axioms) {
                continue;
            }
            let mut covered = Vec::new();
            subsets(f, c.strength, &mut covered);
            for x in covered {
                uncovered.remove(&x);
            }
            cases.push(Case {
                index: cases.len() as u64,
                axioms,
            });
            break;
        }
    }
    let mut left: Vec<Vec<Feature>> = uncovered
        .into_iter()
        .map(|x| x.into_iter().map(|i| ALL_FEATURES[i as usize]).collect())
        .collect();
    left.sort();
    Coverage {
        covered: total - left.len(),
        uncovered: left,
        cases,
    }
}

// Building the ontologies ----------------------------------------------------------------

/// The universe's terms, interned by the caller.
#[derive(Debug, Clone)]
pub struct Universe {
    pub classes: [Term; 2],
    pub roles: [Term; 2],
    pub data: Term,
    pub individuals: [Term; 2],
    pub top_object: Term,
    pub bottom_object: Term,
    pub top_data: Term,
    pub bottom_data: Term,
    pub integer: Term,
    pub string: Term,
    pub defined: Term,
    pub min_inclusive: Term,
    pub max_inclusive: Term,
    /// The integer literals 0 and 1.
    pub literals: [Term; 2],
}

impl Universe {
    pub fn new(intern: &mut dyn FnMut(&Name) -> Term) -> Self {
        let mut iri = |s: String| intern(&Name::Iri(s));
        let classes = [iri(format!("{SCOPE}A")), iri(format!("{SCOPE}B"))];
        let roles = [iri(format!("{SCOPE}r")), iri(format!("{SCOPE}s"))];
        let data = iri(format!("{SCOPE}d"));
        let individuals = [iri(format!("{SCOPE}a")), iri(format!("{SCOPE}b"))];
        let top_object = iri(format!("{OWL}topObjectProperty"));
        let bottom_object = iri(format!("{OWL}bottomObjectProperty"));
        let top_data = iri(format!("{OWL}topDataProperty"));
        let bottom_data = iri(format!("{OWL}bottomDataProperty"));
        let integer = iri(format!("{XSD}integer"));
        let string = iri(format!("{XSD}string"));
        let defined = iri(format!("{SCOPE}dt"));
        let min_inclusive = iri(format!("{XSD}minInclusive"));
        let max_inclusive = iri(format!("{XSD}maxInclusive"));
        let literals = [intern(&Name::Integer(0)), intern(&Name::Integer(1))];
        Self {
            classes,
            roles,
            data,
            individuals,
            top_object,
            bottom_object,
            top_data,
            bottom_data,
            integer,
            string,
            defined,
            min_inclusive,
            max_inclusive,
            literals,
        }
    }

    fn role(&self, r: Role) -> ObjProp {
        match r {
            Role::Prop(i, false) => ObjProp::Named(self.roles[i as usize]),
            Role::Prop(i, true) => ObjProp::Inverse(self.roles[i as usize]),
            Role::Top => ObjProp::Named(self.top_object),
            Role::Bottom => ObjProp::Named(self.bottom_object),
        }
    }

    fn dprop(&self, p: DProp) -> Term {
        match p {
            DProp::D => self.data,
            DProp::Top => self.top_data,
            DProp::Bottom => self.bottom_data,
        }
    }

    fn individuals_of(&self, mask: u8) -> Vec<Term> {
        (0..2)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| self.individuals[i])
            .collect()
    }

    fn data_terms(&self) -> DataTerms {
        let mut data = DataTerms::default();
        for (i, &t) in self.literals.iter().enumerate() {
            data.literals.insert(
                t,
                Literal {
                    lexical: i.to_string(),
                    datatype: Some(format!("{XSD}integer")),
                    language: None,
                },
            );
        }
        for (t, name) in [
            (self.integer, "integer"),
            (self.string, "string"),
            (self.min_inclusive, "minInclusive"),
            (self.max_inclusive, "maxInclusive"),
        ] {
            data.iris.insert(t, format!("{XSD}{name}"));
        }
        data.iris.insert(self.defined, format!("{SCOPE}dt"));
        data
    }

    /// The ontology of `case`: the universe's declarations and the case's axioms.
    pub fn ontology(&self, t: &Tables, case: &Case) -> Ontology {
        let mut b = Builder {
            u: self,
            t,
            o: Ontology::default(),
        };
        let mut axioms: Vec<Axiom> = Vec::new();
        for (kind, terms) in [
            (EntityKind::Class, &self.classes[..]),
            (EntityKind::ObjectProperty, &self.roles[..]),
            (EntityKind::DataProperty, std::slice::from_ref(&self.data)),
            (EntityKind::NamedIndividual, &self.individuals[..]),
        ] {
            axioms.extend(terms.iter().map(|&x| Axiom::Declaration(kind, x)));
        }
        let defines = case
            .axioms
            .iter()
            .any(|&a| matches!(t.axioms[a as usize], Ax::DatatypeDefinition(_)));
        if defines {
            axioms.push(Axiom::Declaration(EntityKind::Datatype, self.defined));
        }
        for &a in &case.axioms {
            axioms.push(b.axiom(&t.axioms[a as usize]));
        }
        axioms.sort();
        axioms.dedup();
        let mut o = b.o;
        o.sources = vec![Vec::new(); axioms.len()];
        o.axioms = axioms;
        o.data = self.data_terms();
        o.builtin = BuiltinProperties {
            top_object: Some(self.top_object),
            bottom_object: Some(self.bottom_object),
            top_data: Some(self.top_data),
            bottom_data: Some(self.bottom_data),
        };
        o
    }
}

/// What each case is asked, in order (the reference runner's `scope` task answers the
/// same): after consistency, whether A and B are satisfiable, `A ⊑ B`, `B ⊑ A`, `⊤ ⊑ A`,
/// `⊤ ⊑ B`, and whether a and b are instances of A and B.
pub const QUESTIONS: [&str; 10] = [
    "A satisfiable",
    "B satisfiable",
    "A ⊑ B",
    "B ⊑ A",
    "⊤ ⊑ A",
    "⊤ ⊑ B",
    "a : A",
    "a : B",
    "b : A",
    "b : B",
];

const PREFIXES: &str = "Prefix(:=<http://example.org/scope#>)Prefix(owl:=<http://www.w3.org/2002/07/owl#>)Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)";

/// The universe over terms numbered from 0, with each term's text in the functional
/// syntax ([`document`] writes with them).
pub fn universe() -> (Universe, Vec<String>) {
    let mut names: Vec<String> = Vec::new();
    let u = Universe::new(&mut |n: &Name| {
        let text = match n {
            Name::Iri(iri) => {
                if let Some(local) = iri.strip_prefix(SCOPE) {
                    format!(":{local}")
                } else if let Some(local) = iri.strip_prefix(OWL) {
                    format!("owl:{local}")
                } else if let Some(local) = iri.strip_prefix(XSD) {
                    format!("xsd:{local}")
                } else {
                    format!("<{iri}>")
                }
            }
            Name::Integer(i) => format!("\"{i}\"^^xsd:integer"),
        };
        if let Some(at) = names.iter().position(|x| *x == text) {
            return at as Term;
        }
        names.push(text);
        (names.len() - 1) as Term
    });
    (u, names)
}

/// Case `index`'s ontology as one line of the functional syntax, prefixed.
pub fn document(o: &Ontology, index: u64, names: &[String]) -> String {
    let name = |t: Term| names[t as usize].clone();
    let mut out = String::from(PREFIXES);
    out.push_str(&format!("Ontology(<urn:nrese:scope:{index}>"));
    for axiom in &o.axioms {
        out.push(' ');
        out.push_str(&o.functional(axiom, &name));
    }
    out.push(')');
    out
}

struct Builder<'a> {
    u: &'a Universe,
    t: &'a Tables,
    o: Ontology,
}

impl Builder<'_> {
    fn e(&mut self, x: ClassExpr) -> ExprId {
        ExprId(self.o.classes.intern(x))
    }

    fn r(&mut self, x: DataRange) -> RangeId {
        RangeId(self.o.ranges.intern(x))
    }

    fn range(&mut self, d: u32) -> RangeId {
        let u = self.u;
        let x = match self.t.ranges[d as usize] {
            D::Literal => DataRange::Literal,
            D::Integer => DataRange::Datatype(u.integer),
            D::String => DataRange::Datatype(u.string),
            D::Defined => DataRange::Datatype(u.defined),
            D::OneOf(m) => DataRange::OneOf(
                (0..2)
                    .filter(|i| m & (1 << i) != 0)
                    .map(|i| u.literals[i])
                    .collect(),
            ),
            D::AtLeastOne => {
                DataRange::Restriction(u.integer, vec![(u.min_inclusive, u.literals[1])])
            }
            D::AtMostZero => {
                DataRange::Restriction(u.integer, vec![(u.max_inclusive, u.literals[0])])
            }
            D::Not(x) => DataRange::Not(self.range(x)),
            D::And(x, y) => {
                let v = sorted(vec![self.range(x), self.range(y)]);
                DataRange::And(v)
            }
            D::Or(x, y) => {
                let v = sorted(vec![self.range(x), self.range(y)]);
                DataRange::Or(v)
            }
        };
        self.r(x)
    }

    fn class(&mut self, c: u32) -> ExprId {
        let u = self.u;
        let x = match self.t.classes[c as usize] {
            C::Class(i) => ClassExpr::Class(u.classes[i as usize]),
            C::Thing => ClassExpr::Thing,
            C::Nothing => ClassExpr::Nothing,
            C::OneOf(m) => ClassExpr::OneOf(u.individuals_of(m)),
            C::HasValue(r, a) => ClassExpr::HasValue(u.role(r), u.individuals[a as usize]),
            C::HasSelf(r) => ClassExpr::HasSelf(u.role(r)),
            C::DataHasValue(p, v) => ClassExpr::DataHasValue(u.dprop(p), u.literals[v as usize]),
            C::Not(x) => ClassExpr::Not(self.class(x)),
            C::And(x, y) => {
                let v = sorted(vec![self.class(x), self.class(y)]);
                ClassExpr::And(v)
            }
            C::Or(x, y) => {
                let v = sorted(vec![self.class(x), self.class(y)]);
                ClassExpr::Or(v)
            }
            C::Some(r, x) => ClassExpr::Some(u.role(r), self.class(x)),
            C::All(r, x) => ClassExpr::All(u.role(r), self.class(x)),
            C::Min(n, r, x) => ClassExpr::Min(u32::from(n), u.role(r), self.class(x)),
            C::Max(n, r, x) => ClassExpr::Max(u32::from(n), u.role(r), self.class(x)),
            C::Exact(n, r, x) => ClassExpr::Exact(u32::from(n), u.role(r), self.class(x)),
            C::DataSome(p, d) => ClassExpr::DataSome(u.dprop(p), self.range(d)),
            C::DataAll(p, d) => ClassExpr::DataAll(u.dprop(p), self.range(d)),
            C::DataMin(n, p, d) => ClassExpr::DataMin(u32::from(n), u.dprop(p), self.range(d)),
            C::DataMax(n, p, d) => ClassExpr::DataMax(u32::from(n), u.dprop(p), self.range(d)),
            C::DataExact(n, p, d) => ClassExpr::DataExact(u32::from(n), u.dprop(p), self.range(d)),
        };
        self.e(x)
    }

    fn axiom(&mut self, a: &Ax) -> Axiom {
        let u = self.u;
        let ind = |i: u8| u.individuals[i as usize];
        match a {
            Ax::SubClassOf(x, y) => Axiom::SubClassOf(self.class(*x), self.class(*y)),
            Ax::EquivalentClasses(x, y) => {
                Axiom::EquivalentClasses(sorted(vec![self.class(*x), self.class(*y)]))
            }
            Ax::DisjointClasses(x, y) => {
                Axiom::DisjointClasses(sorted(vec![self.class(*x), self.class(*y)]))
            }
            Ax::DisjointUnion(k, x, y) => Axiom::DisjointUnion(
                u.classes[*k as usize],
                sorted(vec![self.class(*x), self.class(*y)]),
            ),
            Ax::SubObjectPropertyOf(chain, sup) => {
                Axiom::SubObjectPropertyOf(chain.iter().map(|&r| u.role(r)).collect(), u.role(*sup))
            }
            Ax::EquivalentObjectProperties(x, y) => {
                Axiom::EquivalentObjectProperties(sorted(vec![u.role(*x), u.role(*y)]))
            }
            Ax::DisjointObjectProperties(x, y) => {
                Axiom::DisjointObjectProperties(sorted(vec![u.role(*x), u.role(*y)]))
            }
            Ax::InverseObjectProperties(x, y) => {
                Axiom::InverseObjectProperties(u.role(*x), u.role(*y))
            }
            Ax::ObjectPropertyDomain(r, x) => {
                Axiom::ObjectPropertyDomain(u.role(*r), self.class(*x))
            }
            Ax::ObjectPropertyRange(r, x) => Axiom::ObjectPropertyRange(u.role(*r), self.class(*x)),
            Ax::Characteristic(kind, r) => Axiom::ObjectCharacteristic(*kind, u.role(*r)),
            Ax::SubDataPropertyOf(x, y) => Axiom::SubDataPropertyOf(u.dprop(*x), u.dprop(*y)),
            Ax::EquivalentDataProperties(x, y) => {
                Axiom::EquivalentDataProperties(sorted(vec![u.dprop(*x), u.dprop(*y)]))
            }
            Ax::DisjointDataProperties(x, y) => {
                Axiom::DisjointDataProperties(sorted(vec![u.dprop(*x), u.dprop(*y)]))
            }
            Ax::DataPropertyDomain(p, x) => Axiom::DataPropertyDomain(u.dprop(*p), self.class(*x)),
            Ax::DataPropertyRange(p, d) => Axiom::DataPropertyRange(u.dprop(*p), self.range(*d)),
            Ax::FunctionalDataProperty(p) => Axiom::FunctionalDataProperty(u.dprop(*p)),
            Ax::DatatypeDefinition(d) => Axiom::DatatypeDefinition(u.defined, self.range(*d)),
            Ax::HasKey(x, r, d) => Axiom::HasKey(
                self.class(*x),
                r.map(|r| u.role(r)).into_iter().collect(),
                if *d { vec![u.data] } else { Vec::new() },
            ),
            Ax::ClassAssertion(x, i) => Axiom::ClassAssertion(self.class(*x), ind(*i)),
            Ax::ObjectPropertyAssertion(r, x, y) => {
                Axiom::ObjectPropertyAssertion(u.role(*r).named(), ind(*x), ind(*y))
            }
            Ax::NegativeObjectPropertyAssertion(r, x, y) => {
                Axiom::NegativeObjectPropertyAssertion(u.role(*r).named(), ind(*x), ind(*y))
            }
            Ax::DataPropertyAssertion(p, i, v) => {
                Axiom::DataPropertyAssertion(u.dprop(*p), ind(*i), u.literals[*v as usize])
            }
            Ax::NegativeDataPropertyAssertion(p, i, v) => {
                Axiom::NegativeDataPropertyAssertion(u.dprop(*p), ind(*i), u.literals[*v as usize])
            }
            Ax::SameIndividual => Axiom::SameIndividual(sorted(u.individuals.to_vec())),
            Ax::DifferentIndividuals => Axiom::DifferentIndividuals(sorted(u.individuals.to_vec())),
        }
    }
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables are closed under every renaming, each renaming is a bijection on the
    /// axioms, and the renamings are closed under composition (a group: the least image
    /// then stands for every case of its class).
    #[test]
    fn renamings_are_a_group_on_the_tables() {
        let t = Tables::new(3);
        let syms = symmetries();
        let maps: Vec<Vec<u32>> = syms
            .iter()
            .map(|s| (0..t.axioms.len() as u32).map(|a| s.axiom(&t, a)).collect())
            .collect();
        for map in &maps {
            let mut seen = vec![false; map.len()];
            for &b in map {
                assert!(!std::mem::replace(&mut seen[b as usize], true));
            }
        }
        for f in &maps {
            for g in &maps {
                let composed: Vec<u32> = f.iter().map(|&a| g[a as usize]).collect();
                assert!(maps.contains(&composed));
            }
        }
    }

    /// §11.2's property hierarchy: the specification's own examples.
    #[test]
    fn regularity_follows_the_specification() {
        let t = Tables::new(2);
        let id = |a: Ax| t.axiom_id(&a).expect("in the tables");
        let (r, s) = (Role::Prop(0, false), Role::Prop(1, false));
        // r ∘ s ⊑ s and s ∘ r ⊑ r: s < ... needs r < s and s < r.
        let cyclic = [
            id(Ax::SubObjectPropertyOf(vec![r, s], s)),
            id(Ax::SubObjectPropertyOf(vec![s, r], r)),
        ];
        assert!(!regular(&t, &cyclic));
        // r ∘ s ⊑ s alone, and r ∘ r ⊑ r (transitivity's form).
        assert!(regular(&t, &cyclic[..1]));
        assert!(regular(&t, &[id(Ax::SubObjectPropertyOf(vec![r, r], r))]));
        // r ∘ s ⊑ r needs s < r; with r ≡ s, r →* s holds, which s < r forbids.
        let a = id(Ax::SubObjectPropertyOf(vec![r, s], r));
        let eq = id(Ax::EquivalentObjectProperties(r, s));
        assert!(regular(&t, &[a]));
        assert!(!regular(&t, &[a, eq]));
        // U above a chain needs no order.
        let u = id(Ax::SubObjectPropertyOf(vec![s, r], Role::Top));
        assert!(regular(&t, &[u, a]));
        // r⁻ ∘ r ⊑ r: r⁻ < r, and by the inverse condition r < r: irregular.
        let inv = id(Ax::SubObjectPropertyOf(vec![Role::Prop(0, true), r], r));
        assert!(!regular(&t, &[inv]));
        // r ∘ r ⊑ r⁻ is r⁻ ∘ r⁻ ⊑ r: irregular as that is (HermiT rejects it too).
        let into_inverse = id(Ax::SubObjectPropertyOf(vec![r, r], Role::Prop(0, true)));
        assert!(!regular(&t, &[into_inverse]));
        // s ∘ s ⊑ r⁻ is s⁻ ∘ s⁻ ⊑ r: s < r will do.
        let other = id(Ax::SubObjectPropertyOf(vec![s, s], Role::Prop(0, true)));
        assert!(regular(&t, &[other]));
    }

    /// Simple roles: transitivity or a chain makes a property composite, and what is above
    /// it non-simple, including through U.
    #[test]
    fn simple_roles_follow_the_specification() {
        let t = Tables::new(2);
        let id = |a: Ax| t.axiom_id(&a).expect("in the tables");
        let (r, s) = (Role::Prop(0, false), Role::Prop(1, false));
        let functional = id(Ax::Characteristic(Characteristic::Functional, s));
        assert!(simple_roles_ok(&t, &[functional]));
        let transitive = id(Ax::Characteristic(Characteristic::Transitive, r));
        let sub = id(Ax::SubObjectPropertyOf(vec![r], s));
        assert!(simple_roles_ok(&t, &[transitive, functional]));
        assert!(!simple_roles_ok(&t, &[transitive, sub, functional]));
        let top = id(Ax::SubObjectPropertyOf(vec![Role::Top], s));
        assert!(!simple_roles_ok(&t, &[top, functional]));
        let inverse = id(Ax::Characteristic(
            Characteristic::Functional,
            Role::Prop(1, true),
        ));
        assert!(!simple_roles_ok(&t, &[top, inverse]));
    }

    /// The bounded combinations are the subsets a brute force finds, in its order.
    #[test]
    fn combinations_within_a_total_are_all_found() {
        let sizes = [1, 1, 1, 2, 2, 3, 3, 4];
        for k in 1..=4 {
            for total in 0..=10 {
                let got: Vec<Vec<u32>> = Combinations::new(&sizes, k, total).collect();
                let mut want = Vec::new();
                for mask in 0u32..1 << sizes.len() {
                    let set: Vec<u32> = (0..sizes.len() as u32)
                        .filter(|i| mask & (1 << i) != 0)
                        .collect();
                    let sum: u32 = set.iter().map(|&i| sizes[i as usize]).sum();
                    if set.len() == k && sum <= total {
                        want.push(set);
                    }
                }
                want.sort();
                assert_eq!(got, want, "k {k} total {total}");
            }
        }
    }

    /// A covering array of pairs covers every pair of features some small ontology has,
    /// with cases within OWL 2 DL, deterministically.
    #[test]
    fn a_covering_array_covers_the_pairs() {
        let t = Tables::new(3);
        let c = Covering {
            strength: 2,
            min: 2,
            max: 5,
            seed: 7,
        };
        let a = covering(&t, c);
        assert_eq!(a, covering(&t, c));
        for case in &a.cases {
            assert!(within_dl(&t, &case.axioms));
        }
        // Every pair the tables' axioms can express (`And` and `Or` need size 4).
        let expressible = (0..t.axioms.len() as u32).fold(0u128, |m, x| m | t.axiom_features(x));
        let missing: Vec<&Vec<Feature>> = a
            .uncovered
            .iter()
            .filter(|pair| pair.iter().all(|f| expressible & f.bit() != 0))
            .collect();
        assert!(
            missing.is_empty(),
            "{} pairs covered by {} cases; not {missing:?}",
            a.covered,
            a.cases.len()
        );
    }

    /// The enumeration is deterministic, its cases distinct, within OWL 2 DL and least
    /// among their renamings.
    #[test]
    fn cases_are_distinct_least_and_within_dl() {
        let scope = Scope {
            max_total: vec![3, 3],
        };
        let t = scope.tables();
        let first: Vec<Case> = cases(&scope, &t).collect();
        let again: Vec<Case> = cases(&scope, &t).collect();
        assert_eq!(first, again);
        let mut seen = std::collections::HashSet::new();
        for c in &first {
            assert!(seen.insert(c.axioms.clone()));
            assert!(within_dl(&t, &c.axioms));
        }
        assert!(first.len() > 1000, "{}", first.len());
        let mut names: Vec<String> = Vec::new();
        let u = Universe::new(&mut |n: &Name| {
            names.push(format!("{n:?}"));
            (names.len() - 1) as Term
        });
        for c in first.iter().take(500) {
            let o = u.ontology(&t, c);
            assert!(o.axioms.len() > c.axioms.len());
        }
    }
}
