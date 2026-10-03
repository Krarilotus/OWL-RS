//! Random OWL 2 DL ontologies and meaning-preserving transformations of them: the
//! SROIQ(D) fuzzer and metamorphic harness of docs/design/owl2-dl.md §11 (work package
//! 2.5), for every engine's tests.
//!
//! - **The generator** ([`ontology`]) builds ontologies in the structural model over a
//!   small [`Signature`], within the global restrictions of OWL 2 DL: the object
//!   properties are ordered, the first [`Sizes::simple`] are simple and the rest may be
//!   transitive or the superproperty of a chain; cardinalities, `Self`, functionality,
//!   irreflexivity, asymmetry and disjointness use simple properties only, and every chain
//!   is regular (its properties precede its superproperty, which may stand at either end).
//!   [`Profile`]s choose the constructors: EL (what the EL classifier reads), or all of
//!   SROIQ(D) with data, nominals and numbers on or off.
//! - **The transformations** ([`rename`], [`shuffle`], [`add_redundant`], [`define_fresh`])
//!   leave the entailments over the original signature as they are (renaming: up to the
//!   renaming), so an engine's answers must not change (§11's metamorphic tests).
//!
//! Small signatures on purpose: the model enumerators of the tests check such ontologies
//! exhaustively.

use crate::mapping::Ontology;
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, EntityKind, ExprId, ObjProp, RangeId, Term,
    canonical,
};

/// The IRIs of generated entities: `{FUZZ}C0`, `{FUZZ}p0`, `{FUZZ}d0`, `{FUZZ}a0`.
pub const FUZZ: &str = "http://example.org/fuzz#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// A deterministic random source (SplitMix64).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A number below `n` (0 for `n` = 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n.max(1)
    }

    /// True one time in `n`.
    pub fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }
}

/// What the caller interns for a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Name {
    Iri(String),
    /// An `xsd:integer` literal.
    Integer(i64),
}

/// How many entities of each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizes {
    pub classes: u32,
    pub object_properties: u32,
    /// The first this many object properties are simple.
    pub simple: u32,
    pub data_properties: u32,
    pub individuals: u32,
    pub literals: u32,
}

impl Default for Sizes {
    fn default() -> Self {
        Self {
            classes: 4,
            object_properties: 4,
            simple: 2,
            data_properties: 1,
            individuals: 3,
            literals: 3,
        }
    }
}

/// The terms a generated ontology is over.
#[derive(Debug, Clone)]
pub struct Signature {
    pub classes: Vec<Term>,
    /// Ordered: the first `simple` are simple, chains go up the order.
    pub object_properties: Vec<Term>,
    pub simple: usize,
    pub data_properties: Vec<Term>,
    pub individuals: Vec<Term>,
    pub integer: Term,
    /// `xsd:minInclusive`, `xsd:maxInclusive`.
    pub facets: [Term; 2],
    /// `xsd:integer` literals 0, 1, ...
    pub literals: Vec<Term>,
}

impl Signature {
    /// The signature of `sizes`, each term interned by `intern`.
    pub fn new(sizes: Sizes, intern: &mut dyn FnMut(&Name) -> Term) -> Self {
        let mut iris = |prefix: &str, n: u32| -> Vec<Term> {
            (0..n)
                .map(|i| intern(&Name::Iri(format!("{FUZZ}{prefix}{i}"))))
                .collect()
        };
        let classes = iris("C", sizes.classes);
        let object_properties = iris("p", sizes.object_properties);
        let data_properties = iris("d", sizes.data_properties);
        let individuals = iris("a", sizes.individuals);
        let integer = intern(&Name::Iri(format!("{XSD}integer")));
        let facets = [
            intern(&Name::Iri(format!("{XSD}minInclusive"))),
            intern(&Name::Iri(format!("{XSD}maxInclusive"))),
        ];
        let literals = (0..sizes.literals)
            .map(|i| intern(&Name::Integer(i64::from(i))))
            .collect();
        Self {
            classes,
            object_properties,
            simple: (sizes.simple.min(sizes.object_properties)) as usize,
            data_properties,
            individuals,
            integer,
            facets,
            literals,
        }
    }

    /// Declarations of every entity (OWL 2 DL wants them; the reader then has no guesses).
    pub fn declarations(&self) -> Vec<Axiom> {
        let mut out = Vec::new();
        out.extend(
            self.classes
                .iter()
                .map(|&c| Axiom::Declaration(EntityKind::Class, c)),
        );
        out.extend(
            self.object_properties
                .iter()
                .map(|&p| Axiom::Declaration(EntityKind::ObjectProperty, p)),
        );
        out.extend(
            self.data_properties
                .iter()
                .map(|&p| Axiom::Declaration(EntityKind::DataProperty, p)),
        );
        out.extend(
            self.individuals
                .iter()
                .map(|&a| Axiom::Declaration(EntityKind::NamedIndividual, a)),
        );
        out
    }
}

/// Which constructors and axioms the generator uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// Logical axioms per ontology (before deduplication).
    pub axioms: usize,
    /// The nesting depth of class expressions.
    pub depth: u32,
    /// EL only: conjunction, existential restriction, `owl:Thing`, `owl:Nothing`; class,
    /// property and chain axioms the EL classifier reads; no ABox.
    pub el: bool,
    pub data: bool,
    pub nominals: bool,
    /// Cardinality restrictions and `Self`.
    pub numbers: bool,
    /// Transitivity and chains.
    pub chains: bool,
    pub abox: bool,
}

impl Profile {
    /// All of SROIQ(D).
    pub fn sroiq() -> Self {
        Self {
            axioms: 10,
            depth: 2,
            el: false,
            data: true,
            nominals: true,
            numbers: true,
            chains: true,
            abox: true,
        }
    }

    /// The EL classifier's fragment.
    pub fn el() -> Self {
        Self {
            axioms: 10,
            depth: 2,
            el: true,
            data: false,
            nominals: false,
            numbers: false,
            chains: true,
            abox: false,
        }
    }
}

struct Gen<'a> {
    rng: &'a mut Rng,
    sig: &'a Signature,
    profile: Profile,
    o: Ontology,
}

impl Gen<'_> {
    fn e(&mut self, expr: ClassExpr) -> ExprId {
        ExprId(self.o.classes.intern(expr))
    }

    fn r(&mut self, range: DataRange) -> RangeId {
        RangeId(self.o.ranges.intern(range))
    }

    /// Any object property expression (inverses outside EL).
    fn any_role(&mut self) -> ObjProp {
        let p = self.rng.pick(&self.sig.object_properties);
        if !self.profile.el && self.rng.one_in(4) {
            ObjProp::Inverse(p)
        } else {
            ObjProp::Named(p)
        }
    }

    /// A simple object property expression.
    fn simple_role(&mut self) -> ObjProp {
        let p = self
            .rng
            .pick(&self.sig.object_properties[..self.sig.simple.max(1)]);
        if !self.profile.el && self.rng.one_in(4) {
            ObjProp::Inverse(p)
        } else {
            ObjProp::Named(p)
        }
    }

    fn range(&mut self) -> RangeId {
        let (integer, lits) = (self.sig.integer, self.sig.literals.clone());
        let r = match self.rng.below(4) {
            0 => DataRange::Datatype(integer),
            1 => {
                let facet = self.rng.pick(&self.sig.facets);
                DataRange::Restriction(integer, vec![(facet, self.rng.pick(&lits))])
            }
            2 => {
                let mut v = vec![self.rng.pick(&lits), self.rng.pick(&lits)];
                v.sort_unstable();
                v.dedup();
                DataRange::OneOf(v)
            }
            _ => {
                let inner = self.range();
                DataRange::Not(inner)
            }
        };
        self.r(r)
    }

    fn pair(&mut self, depth: u32) -> Vec<ExprId> {
        let v = vec![self.class(depth), self.class(depth)];
        canonical(v)
    }

    fn class(&mut self, depth: u32) -> ExprId {
        let named = |g: &mut Self| {
            let c = g.rng.pick(&g.sig.classes);
            g.e(ClassExpr::Class(c))
        };
        if depth == 0 || self.rng.one_in(3) {
            return match self.rng.below(12) {
                0 => self.e(ClassExpr::Thing),
                1 if !self.profile.el || self.rng.one_in(2) => self.e(ClassExpr::Nothing),
                _ => named(self),
            };
        }
        let d = depth - 1;
        if self.profile.el {
            return match self.rng.below(3) {
                0 => {
                    let v = self.pair(d);
                    self.and_or_single(v, true)
                }
                _ => {
                    let role = ObjProp::Named(self.rng.pick(&self.sig.object_properties));
                    let filler = self.class(d);
                    self.e(ClassExpr::Some(role, filler))
                }
            };
        }
        let lits = self.sig.literals.clone();
        let inds = self.sig.individuals.clone();
        loop {
            let expr = match self.rng.below(17) {
                0 => {
                    let v = self.pair(d);
                    return self.and_or_single(v, true);
                }
                1 => {
                    let v = self.pair(d);
                    return self.and_or_single(v, false);
                }
                2 => ClassExpr::Not(self.class(d)),
                3 | 4 => ClassExpr::Some(self.any_role(), self.class(d)),
                5 => ClassExpr::All(self.any_role(), self.class(d)),
                6 if self.profile.numbers => {
                    ClassExpr::Min(self.rng.below(3) as u32, self.simple_role(), self.class(d))
                }
                7 if self.profile.numbers => {
                    ClassExpr::Max(self.rng.below(3) as u32, self.simple_role(), self.class(d))
                }
                8 if self.profile.numbers => ClassExpr::Exact(
                    1 + self.rng.below(2) as u32,
                    self.simple_role(),
                    self.class(d),
                ),
                9 if self.profile.numbers => ClassExpr::HasSelf(self.simple_role()),
                10 if self.profile.nominals => {
                    ClassExpr::HasValue(self.any_role(), self.rng.pick(&inds))
                }
                11 if self.profile.nominals => {
                    let mut v = vec![self.rng.pick(&inds), self.rng.pick(&inds)];
                    v.sort_unstable();
                    v.dedup();
                    ClassExpr::OneOf(v)
                }
                12 if self.profile.data && !self.sig.data_properties.is_empty() => {
                    ClassExpr::DataSome(self.rng.pick(&self.sig.data_properties), self.range())
                }
                13 if self.profile.data && !self.sig.data_properties.is_empty() => {
                    ClassExpr::DataAll(self.rng.pick(&self.sig.data_properties), self.range())
                }
                14 if self.profile.data && !self.sig.data_properties.is_empty() => {
                    ClassExpr::DataHasValue(
                        self.rng.pick(&self.sig.data_properties),
                        self.rng.pick(&lits),
                    )
                }
                15 if self.profile.data
                    && self.profile.numbers
                    && !self.sig.data_properties.is_empty() =>
                {
                    ClassExpr::DataMax(
                        1 + self.rng.below(2) as u32,
                        self.rng.pick(&self.sig.data_properties),
                        self.range(),
                    )
                }
                16 => return named(self),
                _ => continue,
            };
            return self.e(expr);
        }
    }

    /// An intersection or union of `v`, or its single operand.
    fn and_or_single(&mut self, v: Vec<ExprId>, and: bool) -> ExprId {
        if v.len() == 1 {
            return v[0];
        }
        self.e(if and {
            ClassExpr::And(v)
        } else {
            ClassExpr::Or(v)
        })
    }

    /// A regular role inclusion: a chain into a non-simple property, or a plain inclusion
    /// up the order (simple into simple, anything lower into a non-simple one).
    fn role_inclusion(&mut self) -> Option<Axiom> {
        let props = self.sig.object_properties.clone();
        let simple = self.sig.simple;
        let el = self.profile.el;
        let maybe_inverse = |g: &mut Self, p: Term| {
            if !el && g.rng.one_in(4) {
                ObjProp::Inverse(p)
            } else {
                ObjProp::Named(p)
            }
        };
        if self.profile.chains && simple < props.len() && self.rng.one_in(2) {
            let top = simple + self.rng.below((props.len() - simple) as u64) as usize;
            let sup = props[top];
            let mut chain: Vec<ObjProp> = (0..1 + self.rng.below(2))
                .map(|_| {
                    let p = props[self.rng.below(top as u64 + 1) as usize];
                    // The superproperty itself only named, at an end.
                    if p == sup {
                        ObjProp::Named(p)
                    } else {
                        maybe_inverse(self, p)
                    }
                })
                .collect();
            // The superproperty at an end at most: `sup ∘ w`, `w ∘ sup` or `w` below it.
            let inner = chain.len();
            for (i, r) in chain.iter_mut().enumerate() {
                if r.named() == sup && i != 0 && i + 1 != inner {
                    *r = ObjProp::Named(props[self.rng.below(top as u64) as usize]);
                }
            }
            match self.rng.below(3) {
                0 => chain.insert(0, ObjProp::Named(sup)),
                1 => chain.push(ObjProp::Named(sup)),
                _ => {}
            }
            if chain.len() < 2 {
                chain.push(ObjProp::Named(props[self.rng.below(top as u64) as usize]));
            }
            // Only the ends may be the superproperty.
            let n = chain.len();
            if chain[1..n - 1].iter().any(|r| r.named() == sup) {
                return None;
            }
            return Some(Axiom::SubObjectPropertyOf(chain, ObjProp::Named(sup)));
        }
        // A plain inclusion: up the order.
        let (i, j) = (
            self.rng.below(props.len() as u64) as usize,
            self.rng.below(props.len() as u64) as usize,
        );
        let (low, high) = (i.min(j), i.max(j));
        if low == high {
            return None;
        }
        // Into a simple property only from a simple one (low < high < simple then).
        let sub = maybe_inverse(self, props[low]);
        Some(Axiom::SubObjectPropertyOf(
            vec![sub],
            ObjProp::Named(props[high]),
        ))
    }

    fn axiom(&mut self) -> Option<Axiom> {
        let depth = self.profile.depth;
        let props = self.sig.object_properties.clone();
        let simple = self.sig.simple;
        if self.profile.el {
            return Some(match self.rng.below(12) {
                0..=4 => Axiom::SubClassOf(self.class(depth), self.class(depth)),
                5 => Axiom::EquivalentClasses(self.pair(depth)).nary()?,
                6 => Axiom::DisjointClasses(self.pair(1)).nary()?,
                7 => Axiom::ObjectPropertyDomain(
                    ObjProp::Named(self.rng.pick(&props)),
                    self.class(1),
                ),
                8 => {
                    Axiom::ObjectPropertyRange(ObjProp::Named(self.rng.pick(&props)), self.class(1))
                }
                9 if self.profile.chains && simple < props.len() => {
                    let p = props[simple + self.rng.below((props.len() - simple) as u64) as usize];
                    Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(p))
                }
                _ => self.role_inclusion()?,
            });
        }
        let inds = self.sig.individuals.clone();
        let lits = self.sig.literals.clone();
        let data = self.sig.data_properties.clone();
        Some(match self.rng.below(24) {
            0..=4 => Axiom::SubClassOf(self.class(depth), self.class(depth)),
            5 => Axiom::EquivalentClasses(self.pair(depth)).nary()?,
            6 => Axiom::DisjointClasses(self.pair(1)).nary()?,
            7 => {
                let class = self.rng.pick(&self.sig.classes);
                let parts = self.pair(1);
                if parts.len() < 2 {
                    return None;
                }
                Axiom::DisjointUnion(class, parts)
            }
            8 => Axiom::ObjectPropertyDomain(self.any_role(), self.class(1)),
            9 => Axiom::ObjectPropertyRange(self.any_role(), self.class(1)),
            10 => {
                let kind = self.rng.pick(&[
                    Characteristic::Functional,
                    Characteristic::InverseFunctional,
                    Characteristic::Reflexive,
                    Characteristic::Irreflexive,
                    Characteristic::Symmetric,
                    Characteristic::Asymmetric,
                    Characteristic::Transitive,
                ]);
                let role = match kind {
                    Characteristic::Transitive if simple < props.len() => ObjProp::Named(
                        props[simple + self.rng.below((props.len() - simple) as u64) as usize],
                    ),
                    Characteristic::Transitive => return None,
                    Characteristic::Reflexive | Characteristic::Symmetric => self.any_role(),
                    _ => self.simple_role(),
                };
                Axiom::ObjectCharacteristic(kind, role)
            }
            11 => {
                let (a, b) = (self.simple_role(), self.simple_role());
                if a == b {
                    return None;
                }
                let mut v = vec![a, b];
                v.sort_unstable();
                Axiom::DisjointObjectProperties(v)
            }
            12 => {
                // Within the simple ones: a non-simple property's inverse stays non-simple.
                let (a, b) = (self.simple_role(), self.simple_role());
                // Two inverses are the pair of the properties (the canonical form).
                let (a, b) = match (a, b) {
                    (ObjProp::Inverse(x), ObjProp::Inverse(y)) => {
                        (ObjProp::Named(x), ObjProp::Named(y))
                    }
                    pair => pair,
                };
                Axiom::InverseObjectProperties(a.min(b), a.max(b))
            }
            13 | 14 => self.role_inclusion()?,
            15 if self.profile.data && !data.is_empty() => {
                Axiom::DataPropertyDomain(self.rng.pick(&data), self.class(1))
            }
            16 if self.profile.data && !data.is_empty() => {
                Axiom::DataPropertyRange(self.rng.pick(&data), self.range())
            }
            17 if self.profile.data && !data.is_empty() => {
                Axiom::FunctionalDataProperty(self.rng.pick(&data))
            }
            18 if self.profile.abox => {
                Axiom::ClassAssertion(self.class(depth), self.rng.pick(&inds))
            }
            19 if self.profile.abox => Axiom::ObjectPropertyAssertion(
                self.rng.pick(&props),
                self.rng.pick(&inds),
                self.rng.pick(&inds),
            ),
            20 if self.profile.abox => Axiom::NegativeObjectPropertyAssertion(
                self.rng.pick(&props),
                self.rng.pick(&inds),
                self.rng.pick(&inds),
            ),
            21 if self.profile.abox && self.profile.data && !data.is_empty() => {
                Axiom::DataPropertyAssertion(
                    self.rng.pick(&data),
                    self.rng.pick(&inds),
                    self.rng.pick(&lits),
                )
            }
            22 if self.profile.abox && self.profile.nominals => {
                let mut v = vec![self.rng.pick(&inds), self.rng.pick(&inds)];
                v.sort_unstable();
                v.dedup();
                if v.len() < 2 {
                    return None;
                }
                if self.rng.one_in(2) {
                    Axiom::SameIndividual(v)
                } else {
                    Axiom::DifferentIndividuals(v)
                }
            }
            _ => Axiom::SubClassOf(self.class(depth), self.class(depth)),
        })
    }
}

trait Nary {
    fn nary(self) -> Option<Axiom>;
}

impl Nary for Axiom {
    /// `None` for an n-ary axiom with fewer than two operands.
    fn nary(self) -> Option<Axiom> {
        match &self {
            Axiom::EquivalentClasses(v) | Axiom::DisjointClasses(v) if v.len() < 2 => None,
            _ => Some(self),
        }
    }
}

/// A random ontology over `sig` within `profile`: the signature's declarations and the
/// axioms, sorted and without repeats, each with no source.
pub fn ontology(rng: &mut Rng, sig: &Signature, profile: Profile) -> Ontology {
    let mut g = Gen {
        rng,
        sig,
        profile,
        o: Ontology::default(),
    };
    let mut axioms = sig.declarations();
    let mut tries = 0;
    while axioms.len() < sig.declarations().len() + profile.axioms && tries < profile.axioms * 4 {
        tries += 1;
        if let Some(axiom) = g.axiom() {
            axioms.push(axiom);
        }
    }
    axioms.sort();
    axioms.dedup();
    let mut o = g.o;
    o.sources = vec![Vec::new(); axioms.len()];
    o.axioms = axioms;
    o
}

// Transformations ------------------------------------------------------------------------

/// Rebuilds `o` with every term mapped by `f` (an injective renaming keeps the meaning, up
/// to the renaming). n-ary operands are sorted again.
pub fn rename(o: &Ontology, f: &dyn Fn(Term) -> Term) -> Ontology {
    let mut out = Ontology::default();
    let mut classes = std::collections::HashMap::new();
    let mut ranges = std::collections::HashMap::new();
    let axioms: Vec<Axiom> = o
        .axioms
        .iter()
        .map(|a| map_axiom(o, &mut out, a, f, &mut classes, &mut ranges))
        .collect();
    let mut axioms = axioms;
    axioms.sort();
    axioms.dedup();
    out.sources = vec![Vec::new(); axioms.len()];
    out.axioms = axioms;
    out
}

type Memo<K> = std::collections::HashMap<K, K>;

fn map_prop(p: ObjProp, f: &dyn Fn(Term) -> Term) -> ObjProp {
    match p {
        ObjProp::Named(t) => ObjProp::Named(f(t)),
        ObjProp::Inverse(t) => ObjProp::Inverse(f(t)),
    }
}

fn map_range(
    o: &Ontology,
    out: &mut Ontology,
    r: RangeId,
    f: &dyn Fn(Term) -> Term,
    memo: &mut Memo<RangeId>,
) -> RangeId {
    if let Some(&done) = memo.get(&r) {
        return done;
    }
    let mapped = match o.ranges.get(r.0).clone() {
        DataRange::Datatype(t) => DataRange::Datatype(f(t)),
        DataRange::And(v) => DataRange::And(canonical(
            v.into_iter()
                .map(|x| map_range(o, out, x, f, memo))
                .collect(),
        )),
        DataRange::Or(v) => DataRange::Or(canonical(
            v.into_iter()
                .map(|x| map_range(o, out, x, f, memo))
                .collect(),
        )),
        DataRange::Not(x) => DataRange::Not(map_range(o, out, x, f, memo)),
        DataRange::OneOf(v) => DataRange::OneOf(canonical(v.into_iter().map(f).collect())),
        DataRange::Restriction(t, facets) => {
            let mut facets: Vec<(Term, Term)> =
                facets.into_iter().map(|(a, b)| (f(a), f(b))).collect();
            facets.sort_unstable();
            DataRange::Restriction(f(t), facets)
        }
    };
    let id = RangeId(out.ranges.intern(mapped));
    memo.insert(r, id);
    id
}

fn map_class(
    o: &Ontology,
    out: &mut Ontology,
    e: ExprId,
    f: &dyn Fn(Term) -> Term,
    memo: &mut Memo<ExprId>,
    ranges: &mut Memo<RangeId>,
) -> ExprId {
    if let Some(&done) = memo.get(&e) {
        return done;
    }
    let c = |x: ExprId, out: &mut Ontology, memo: &mut Memo<ExprId>, ranges: &mut Memo<RangeId>| {
        map_class(o, out, x, f, memo, ranges)
    };
    let mapped = match o.classes.get(e.0).clone() {
        ClassExpr::Class(t) => ClassExpr::Class(f(t)),
        ClassExpr::Thing => ClassExpr::Thing,
        ClassExpr::Nothing => ClassExpr::Nothing,
        ClassExpr::And(v) => ClassExpr::And(canonical(
            v.into_iter().map(|x| c(x, out, memo, ranges)).collect(),
        )),
        ClassExpr::Or(v) => ClassExpr::Or(canonical(
            v.into_iter().map(|x| c(x, out, memo, ranges)).collect(),
        )),
        ClassExpr::Not(x) => ClassExpr::Not(c(x, out, memo, ranges)),
        ClassExpr::OneOf(v) => ClassExpr::OneOf(canonical(v.into_iter().map(f).collect())),
        ClassExpr::Some(p, x) => ClassExpr::Some(map_prop(p, f), c(x, out, memo, ranges)),
        ClassExpr::All(p, x) => ClassExpr::All(map_prop(p, f), c(x, out, memo, ranges)),
        ClassExpr::HasValue(p, a) => ClassExpr::HasValue(map_prop(p, f), f(a)),
        ClassExpr::HasSelf(p) => ClassExpr::HasSelf(map_prop(p, f)),
        ClassExpr::Min(n, p, x) => ClassExpr::Min(n, map_prop(p, f), c(x, out, memo, ranges)),
        ClassExpr::Max(n, p, x) => ClassExpr::Max(n, map_prop(p, f), c(x, out, memo, ranges)),
        ClassExpr::Exact(n, p, x) => ClassExpr::Exact(n, map_prop(p, f), c(x, out, memo, ranges)),
        ClassExpr::DataSome(d, r) => ClassExpr::DataSome(f(d), map_range(o, out, r, f, ranges)),
        ClassExpr::DataAll(d, r) => ClassExpr::DataAll(f(d), map_range(o, out, r, f, ranges)),
        ClassExpr::DataHasValue(d, v) => ClassExpr::DataHasValue(f(d), f(v)),
        ClassExpr::DataMin(n, d, r) => ClassExpr::DataMin(n, f(d), map_range(o, out, r, f, ranges)),
        ClassExpr::DataMax(n, d, r) => ClassExpr::DataMax(n, f(d), map_range(o, out, r, f, ranges)),
        ClassExpr::DataExact(n, d, r) => {
            ClassExpr::DataExact(n, f(d), map_range(o, out, r, f, ranges))
        }
    };
    let id = ExprId(out.classes.intern(mapped));
    memo.insert(e, id);
    id
}

fn map_axiom(
    o: &Ontology,
    out: &mut Ontology,
    a: &Axiom,
    f: &dyn Fn(Term) -> Term,
    classes: &mut Memo<ExprId>,
    ranges: &mut Memo<RangeId>,
) -> Axiom {
    let mut c = |x: ExprId, out: &mut Ontology| map_class(o, out, x, f, classes, ranges);
    let props = |v: &[ObjProp]| -> Vec<ObjProp> { v.iter().map(|&p| map_prop(p, f)).collect() };
    let sorted = |mut v: Vec<ObjProp>| {
        v.sort_unstable();
        v.dedup();
        v
    };
    let terms = |v: &[Term]| canonical(v.iter().map(|&t| f(t)).collect());
    match a {
        Axiom::Declaration(k, t) => Axiom::Declaration(*k, f(*t)),
        Axiom::SubClassOf(x, y) => {
            let x = c(*x, out);
            Axiom::SubClassOf(x, c(*y, out))
        }
        Axiom::EquivalentClasses(v) => {
            Axiom::EquivalentClasses(canonical(v.iter().map(|&x| c(x, out)).collect()))
        }
        Axiom::DisjointClasses(v) => {
            Axiom::DisjointClasses(canonical(v.iter().map(|&x| c(x, out)).collect()))
        }
        Axiom::DisjointUnion(t, v) => {
            Axiom::DisjointUnion(f(*t), canonical(v.iter().map(|&x| c(x, out)).collect()))
        }
        Axiom::SubObjectPropertyOf(chain, sup) => {
            Axiom::SubObjectPropertyOf(props(chain), map_prop(*sup, f))
        }
        Axiom::EquivalentObjectProperties(v) => Axiom::EquivalentObjectProperties(sorted(props(v))),
        Axiom::DisjointObjectProperties(v) => Axiom::DisjointObjectProperties(sorted(props(v))),
        Axiom::InverseObjectProperties(x, y) => {
            let (x, y) = (map_prop(*x, f), map_prop(*y, f));
            Axiom::InverseObjectProperties(x.min(y), x.max(y))
        }
        Axiom::ObjectPropertyDomain(p, x) => {
            Axiom::ObjectPropertyDomain(map_prop(*p, f), c(*x, out))
        }
        Axiom::ObjectPropertyRange(p, x) => Axiom::ObjectPropertyRange(map_prop(*p, f), c(*x, out)),
        Axiom::ObjectCharacteristic(k, p) => Axiom::ObjectCharacteristic(*k, map_prop(*p, f)),
        Axiom::SubDataPropertyOf(x, y) => Axiom::SubDataPropertyOf(f(*x), f(*y)),
        Axiom::EquivalentDataProperties(v) => Axiom::EquivalentDataProperties(terms(v)),
        Axiom::DisjointDataProperties(v) => Axiom::DisjointDataProperties(terms(v)),
        Axiom::DataPropertyDomain(d, x) => Axiom::DataPropertyDomain(f(*d), c(*x, out)),
        Axiom::DataPropertyRange(d, r) => {
            Axiom::DataPropertyRange(f(*d), map_range(o, out, *r, f, ranges))
        }
        Axiom::FunctionalDataProperty(d) => Axiom::FunctionalDataProperty(f(*d)),
        Axiom::DatatypeDefinition(t, r) => {
            Axiom::DatatypeDefinition(f(*t), map_range(o, out, *r, f, ranges))
        }
        Axiom::HasKey(x, ps, ds) => {
            let x = c(*x, out);
            Axiom::HasKey(x, sorted(props(ps)), terms(ds))
        }
        Axiom::ClassAssertion(x, i) => Axiom::ClassAssertion(c(*x, out), f(*i)),
        Axiom::ObjectPropertyAssertion(p, s, t) => {
            Axiom::ObjectPropertyAssertion(f(*p), f(*s), f(*t))
        }
        Axiom::NegativeObjectPropertyAssertion(p, s, t) => {
            Axiom::NegativeObjectPropertyAssertion(f(*p), f(*s), f(*t))
        }
        Axiom::DataPropertyAssertion(p, s, t) => Axiom::DataPropertyAssertion(f(*p), f(*s), f(*t)),
        Axiom::NegativeDataPropertyAssertion(p, s, t) => {
            Axiom::NegativeDataPropertyAssertion(f(*p), f(*s), f(*t))
        }
        Axiom::SameIndividual(v) => Axiom::SameIndividual(terms(v)),
        Axiom::DifferentIndividuals(v) => Axiom::DifferentIndividuals(terms(v)),
    }
}

/// `o` with its axioms in a random order (and their sources with them).
pub fn shuffle(o: &Ontology, rng: &mut Rng) -> Ontology {
    let mut order: Vec<usize> = (0..o.axioms.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i as u64 + 1) as usize);
    }
    let mut out = o.clone();
    out.axioms = order.iter().map(|&i| o.axioms[i].clone()).collect();
    out.sources = order
        .iter()
        .map(|&i| o.sources.get(i).cloned().unwrap_or_default())
        .collect();
    out
}

/// `o` with up to `count` axioms that follow from it in every interpretation (tautologies
/// and weakenings of its own axioms): `X ⊑ X`, `X ⊓ Y ⊑ X`, `A ⊑ B ⊔ C` beside `A ⊑ B`,
/// `A ⊑ ∃r.B` beside `A ⊑ ∃r.(B ⊓ C)`. With `el`, only EL forms.
pub fn add_redundant(
    o: &Ontology,
    rng: &mut Rng,
    sig: &Signature,
    count: usize,
    el: bool,
) -> Ontology {
    let mut out = o.clone();
    let mut added = Vec::new();
    let subs: Vec<(ExprId, ExprId)> = o
        .axioms
        .iter()
        .filter_map(|a| match a {
            Axiom::SubClassOf(x, y) => Some((*x, *y)),
            _ => None,
        })
        .collect();
    for _ in 0..count {
        let x = ExprId(out.classes.intern(ClassExpr::Class(rng.pick(&sig.classes))));
        let y = ExprId(out.classes.intern(ClassExpr::Class(rng.pick(&sig.classes))));
        let axiom = match rng.below(4) {
            0 => Axiom::SubClassOf(x, x),
            1 => {
                let and = canonical(vec![x, y]);
                let and = if and.len() == 1 {
                    and[0]
                } else {
                    ExprId(out.classes.intern(ClassExpr::And(and)))
                };
                Axiom::SubClassOf(and, x)
            }
            2 if !el && !subs.is_empty() => {
                let (a, b) = rng.pick(&subs);
                let or = canonical(vec![b, y]);
                let or = if or.len() == 1 {
                    or[0]
                } else {
                    ExprId(out.classes.intern(ClassExpr::Or(or)))
                };
                Axiom::SubClassOf(a, or)
            }
            _ => {
                // A ⊑ ∃r.(B ⊓ C) gives A ⊑ ∃r.B.
                let Some((a, filler, role)) =
                    subs.iter().find_map(|&(a, b)| match out.classes.get(b.0) {
                        ClassExpr::Some(r, f) => Some((a, *f, *r)),
                        _ => None,
                    })
                else {
                    continue;
                };
                let weaker = match out.classes.get(filler.0).clone() {
                    ClassExpr::And(v) => v[rng.below(v.len() as u64) as usize],
                    _ => filler,
                };
                let some = ExprId(out.classes.intern(ClassExpr::Some(role, weaker)));
                Axiom::SubClassOf(a, some)
            }
        };
        added.push(axiom);
    }
    out.axioms.extend(added);
    out.sources.resize(out.axioms.len(), Vec::new());
    out
}

/// `o` with up to `count` complex superclasses `E` of `A ⊑ E` given a fresh name: `F ≡ E`
/// and `A ⊑ F`. The fresh names come from `fresh` (each a new class IRI, declared). A
/// conservative extension: what follows over the original signature doesn't change.
pub fn define_fresh(o: &Ontology, count: usize, fresh: &mut dyn FnMut() -> Term) -> Ontology {
    let mut out = o.clone();
    let mut done = 0;
    for i in 0..out.axioms.len() {
        if done == count {
            break;
        }
        let Axiom::SubClassOf(a, e) = out.axioms[i].clone() else {
            continue;
        };
        if matches!(
            out.classes.get(e.0),
            ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing
        ) {
            continue;
        }
        let name = fresh();
        let f = ExprId(out.classes.intern(ClassExpr::Class(name)));
        out.axioms[i] = Axiom::SubClassOf(a, f);
        out.axioms.push(Axiom::Declaration(EntityKind::Class, name));
        out.axioms
            .push(Axiom::EquivalentClasses(canonical(vec![f, e])));
        done += 1;
    }
    out.sources.resize(out.axioms.len(), Vec::new());
    out
}
