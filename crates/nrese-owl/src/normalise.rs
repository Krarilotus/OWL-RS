//! Normalisation into DL-clauses (docs/design/owl2-dl.md §3), in the order of Motik,
//! Shearer and Horrocks (JAIR 2009):
//!
//! 1. **Axioms to general concept inclusions** `⊤ ⊑ L₁ ⊔ … ⊔ Lₙ` in negation normal form
//!    (an exact cardinality as a minimum and a maximum, a value restriction as an
//!    existential to a nominal), with extra body atoms where an axiom is about a role
//!    (domains, disjoint and asymmetric properties).
//! 2. **Structural transformation:** a disjunct that isn't a literal, and a filler that
//!    isn't atomic, gets a fresh name by its polarity: `Q ⊑ E` for a positive occurrence,
//!    `E ⊑ Q` for a negative one (only what is needed, so Horn structure survives).
//! 3. **Clausification** of the literals: `¬A` into the body, `A` into the head, `∀R.A`
//!    as a body edge and a head atom on the successor, `≥ n R.A` as an at-least atom,
//!    `≤ n R.A` as `n + 1` successors whose head says two are equal, nominals as nominal
//!    atoms. Negative literals land in the body: what binary and role absorption do.
//! 4. **Non-simple roles** (transitivity, chains) under a universal restriction become an
//!    automaton (Horrocks and Sattler, AIJ 2004): a fresh name per state, a clause per
//!    transition along explicit edges. The chains themselves are left out, so that every
//!    edge an engine sees is explicit (equisatisfiable; exact on interpretations closed
//!    under the role inclusions).
//! 5. **The ABox** as facts, a complex class assertion through a fresh name.
//! 6. **Built-in properties:** the bottom properties relate nothing (`⊤ ⊑ ∀R.⊥`, through
//!    R's automaton where chains reach it); the universal object property goes through a
//!    hub individual (`universal.rs`); an axiom that mentions the universal data property
//!    is reported unsupported, never read as one over an ordinary property.
//!
//! Every clause keeps the axioms it came from.

use std::collections::{HashMap, HashSet};

use crate::clauses::{
    BodyAtom, Clause, Concept, Filler, FreshOf, HeadAtom, Normalised, SafeRule, Var,
};
use crate::mapping::Ontology;
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, ExprId, Interner, ObjProp, RangeId, Term,
};

/// How the normalisation encodes what has more than one encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// `≤ n R.C` up to this `n` is spelled out as `n + 1` successors whose head says two
    /// are equal (quadratic in `n`, but plain resolution for the engines); above it, an
    /// at-most atom. At most [`Options::MAX_EXPANSION`].
    pub expand_at_most_up_to: u32,
}

impl Options {
    pub const MAX_EXPANSION: u32 = 8;
}

impl Default for Options {
    fn default() -> Self {
        Self {
            expand_at_most_up_to: 2,
        }
    }
}

/// The clauses and facts of `ontology`, with the default [`Options`].
pub fn normalise(ontology: &Ontology) -> Normalised {
    normalise_with(ontology, Options::default())
}

/// The clauses and facts of `ontology`.
pub fn normalise_with(ontology: &Ontology, options: Options) -> Normalised {
    // The universal object property through a hub (`universal.rs`), axiom for axiom.
    let rewritten = ontology.builtin.top_object.and_then(|top| {
        let used = ontology
            .axioms
            .iter()
            .any(|a| crate::properties::mentions(ontology, a, &|t| t == top));
        used.then(|| {
            let mut r = crate::universal::rewrite(ontology, top);
            r.ontology.builtin.top_object = None;
            r
        })
    });
    let ontology = rewritten.as_ref().map_or(ontology, |r| &r.ontology);
    let mut n = Normaliser::new(ontology);
    n.expand_up_to = options.expand_at_most_up_to.min(Options::MAX_EXPANSION);
    if let Some(r) = &rewritten {
        for &index in &r.unsupported {
            n.out
                .unsupported
                .push((index, crate::universal::UNSUPPORTED));
        }
    }
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        if n.tautology(axiom) {
            continue;
        }
        if n.uses_top(axiom) {
            n.out.unsupported.push((index, UNIVERSAL));
            continue;
        }
        n.axiom(index, axiom);
    }
    if let Some(r) = &rewritten {
        // ⊤ ⊑ ∃u.{h}, from every axiom that uses the universal property.
        let hub = n.e(ClassExpr::OneOf(vec![r.hub]));
        let to_hub = n.e(ClassExpr::Some(ObjProp::Named(r.hub), hub));
        for &index in &r.uses {
            n.gci(Vec::new(), vec![Item::Expr(to_hub)], index);
        }
    }
    n.bottom_properties();
    n.drain();
    n.finish()
}

/// Why a datatype definition is in [`Normalised::unsupported`]: the clauses don't cover
/// it; a datatype theory reads it from [`Normalised::definitions`].
pub const UNSUPPORTED_DATATYPE_DEFINITIONS: &str = "datatype definitions (the datatype theory)";

/// Why a key is in [`Normalised::unsupported`]: the clauses don't cover it; an engine with
/// DL-safe rules reads it from [`Normalised::rules`].
pub const UNSUPPORTED_KEYS: &str = "keys (a DL-safe rule over named individuals)";

/// The most GCIs one GCI is distributed into over its conjunctions.
const MAX_DISTRIBUTED: usize = 16;

/// Why an axiom over a universal property is left out.
const UNIVERSAL: &str = "the universal data property (owl:topDataProperty)";

/// The roles equivalent to one, each with the inclusions that make it a subrole of it.
type Members = Vec<(ObjProp, Vec<usize>)>;

/// A disjunct of a GCI being clausified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Item {
    Expr(ExprId),
    /// A fresh name (positive) or its complement.
    Fresh(u32),
    NotFresh(u32),
}

/// A GCI waiting to be clausified: extra body atoms and its disjuncts.
struct Gci {
    body: Vec<BodyAtom>,
    items: Vec<Item>,
    source: usize,
}

/// An automaton over roles: states, the start, the end, transitions (`None`: ε), each
/// with the role-inclusion axioms it comes from (none for the role's own edge).
#[derive(Debug, Clone)]
struct Nfa {
    states: u32,
    start: u32,
    end: u32,
    edges: Vec<(u32, Option<ObjProp>, u32, Vec<usize>)>,
}

impl Nfa {
    /// The automaton of the inverse role: every path reversed.
    fn inverse(&self) -> Nfa {
        Nfa {
            states: self.states,
            start: self.end,
            end: self.start,
            edges: self
                .edges
                .iter()
                .map(|(a, label, b, axioms)| (*b, label.map(ObjProp::inverse), *a, axioms.clone()))
                .collect(),
        }
    }
}

struct Normaliser<'a> {
    ontology: &'a Ontology,
    classes: Interner<ClassExpr>,
    ranges: Interner<DataRange>,
    out: Normalised,
    queue: Vec<Gci>,
    positive: HashMap<ExprId, u32>,
    negative: HashMap<ExprId, u32>,
    /// Role inclusions `chain ⊑ named role`, normalised to a named superrole, with the
    /// axiom each comes from.
    rias: Vec<(Vec<ObjProp>, Term, usize)>,
    non_simple: HashSet<Term>,
    automata: HashMap<(ObjProp, Item), u32>,
    /// The single-role inclusions as edges between role expressions, both ways (`R ⊑ S`
    /// gives `R → S` and `R⁻ → S⁻`), with the axiom of each.
    up: HashMap<ObjProp, Vec<(ObjProp, usize)>>,
    down: HashMap<ObjProp, Vec<(ObjProp, usize)>>,
    /// The roles equivalent to a role, as [`Normaliser::equivalents`] computes them.
    equivalent: std::cell::RefCell<HashMap<ObjProp, Members>>,
    clause_index: HashMap<(Vec<BodyAtom>, Vec<HeadAtom>), usize>,
    expand_up_to: u32,
}

impl<'a> Normaliser<'a> {
    fn new(ontology: &'a Ontology) -> Self {
        let mut n = Self {
            ontology,
            classes: ontology.classes.clone(),
            ranges: ontology.ranges.clone(),
            out: Normalised::default(),
            queue: Vec::new(),
            positive: HashMap::new(),
            negative: HashMap::new(),
            rias: Vec::new(),
            non_simple: HashSet::new(),
            automata: HashMap::new(),
            up: HashMap::new(),
            down: HashMap::new(),
            equivalent: std::cell::RefCell::new(HashMap::new()),
            clause_index: HashMap::new(),
            expand_up_to: Options::default().expand_at_most_up_to,
        };
        n.role_inclusions();
        n
    }

    // Expressions ----------------------------------------------------------------------

    fn e(&mut self, expr: ClassExpr) -> ExprId {
        ExprId(self.classes.intern(expr))
    }

    fn get(&self, id: ExprId) -> ClassExpr {
        self.classes.get(id.0).clone()
    }

    fn range_not(&mut self, r: RangeId) -> RangeId {
        RangeId(self.ranges.intern(DataRange::Not(r)))
    }

    /// The negation normal form of `id`.
    fn nnf(&mut self, id: ExprId) -> ExprId {
        let expr = self.get(id);
        let out = match expr {
            ClassExpr::Not(x) => return self.nnf_not(x),
            ClassExpr::And(xs) => {
                let xs: Vec<ExprId> = xs.iter().map(|&x| self.nnf(x)).collect();
                ClassExpr::And(crate::model::canonical(xs))
            }
            ClassExpr::Or(xs) => {
                let xs: Vec<ExprId> = xs.iter().map(|&x| self.nnf(x)).collect();
                ClassExpr::Or(crate::model::canonical(xs))
            }
            ClassExpr::Some(r, x) => ClassExpr::Some(r, self.nnf(x)),
            ClassExpr::All(r, x) => ClassExpr::All(r, self.nnf(x)),
            ClassExpr::Min(n, r, x) => ClassExpr::Min(n, r, self.nnf(x)),
            ClassExpr::Max(n, r, x) => ClassExpr::Max(n, r, self.nnf(x)),
            ClassExpr::Exact(n, r, x) => {
                let x = self.nnf(x);
                let min = self.e(ClassExpr::Min(n, r, x));
                let max = self.e(ClassExpr::Max(n, r, x));
                ClassExpr::And(crate::model::canonical(vec![min, max]))
            }
            ClassExpr::HasValue(r, a) => {
                let nominal = self.e(ClassExpr::OneOf(vec![a]));
                ClassExpr::Some(r, nominal)
            }
            ClassExpr::DataExact(n, d, r) => {
                let min = self.e(ClassExpr::DataMin(n, d, r));
                let max = self.e(ClassExpr::DataMax(n, d, r));
                ClassExpr::And(crate::model::canonical(vec![min, max]))
            }
            ClassExpr::DataHasValue(d, v) => {
                let r = RangeId(self.ranges.intern(DataRange::OneOf(vec![v])));
                ClassExpr::DataSome(d, r)
            }
            other => other,
        };
        self.e(out)
    }

    /// The negation normal form of `¬id`.
    fn nnf_not(&mut self, id: ExprId) -> ExprId {
        let expr = self.get(id);
        let out = match expr {
            ClassExpr::Class(_) | ClassExpr::HasSelf(_) => ClassExpr::Not(id),
            ClassExpr::OneOf(xs) => {
                // ¬{a, b} = ¬{a} ⊓ ¬{b}: each a literal of its own.
                let parts: Vec<ExprId> = xs
                    .iter()
                    .map(|&a| {
                        let one = self.e(ClassExpr::OneOf(vec![a]));
                        self.e(ClassExpr::Not(one))
                    })
                    .collect();
                if parts.len() == 1 {
                    return parts[0];
                }
                ClassExpr::And(crate::model::canonical(parts))
            }
            ClassExpr::Thing => ClassExpr::Nothing,
            ClassExpr::Nothing => ClassExpr::Thing,
            ClassExpr::Not(x) => return self.nnf(x),
            ClassExpr::And(xs) => {
                let xs: Vec<ExprId> = xs.iter().map(|&x| self.nnf_not(x)).collect();
                ClassExpr::Or(crate::model::canonical(xs))
            }
            ClassExpr::Or(xs) => {
                let xs: Vec<ExprId> = xs.iter().map(|&x| self.nnf_not(x)).collect();
                ClassExpr::And(crate::model::canonical(xs))
            }
            ClassExpr::Some(r, x) => ClassExpr::All(r, self.nnf_not(x)),
            ClassExpr::All(r, x) => ClassExpr::Some(r, self.nnf_not(x)),
            ClassExpr::Min(0, _, _) => ClassExpr::Nothing,
            ClassExpr::Min(n, r, x) => ClassExpr::Max(n - 1, r, self.nnf(x)),
            ClassExpr::Max(n, r, x) => ClassExpr::Min(n + 1, r, self.nnf(x)),
            ClassExpr::Exact(n, r, x) => {
                let x = self.nnf(x);
                let more = self.e(ClassExpr::Min(n + 1, r, x));
                if n == 0 {
                    return more;
                }
                let fewer = self.e(ClassExpr::Max(n - 1, r, x));
                ClassExpr::Or(crate::model::canonical(vec![fewer, more]))
            }
            ClassExpr::HasValue(r, a) => {
                let nominal = self.e(ClassExpr::OneOf(vec![a]));
                let not = self.nnf_not(nominal);
                ClassExpr::All(r, not)
            }
            ClassExpr::DataSome(d, r) => ClassExpr::DataAll(d, self.range_not(r)),
            ClassExpr::DataAll(d, r) => ClassExpr::DataSome(d, self.range_not(r)),
            ClassExpr::DataMin(0, _, _) => ClassExpr::Nothing,
            ClassExpr::DataMin(n, d, r) => ClassExpr::DataMax(n - 1, d, r),
            ClassExpr::DataMax(n, d, r) => ClassExpr::DataMin(n + 1, d, r),
            ClassExpr::DataExact(n, d, r) => {
                let more = self.e(ClassExpr::DataMin(n + 1, d, r));
                if n == 0 {
                    return more;
                }
                let fewer = self.e(ClassExpr::DataMax(n - 1, d, r));
                ClassExpr::Or(crate::model::canonical(vec![fewer, more]))
            }
            ClassExpr::DataHasValue(d, v) => {
                let one = RangeId(self.ranges.intern(DataRange::OneOf(vec![v])));
                ClassExpr::DataAll(d, self.range_not(one))
            }
        };
        self.e(out)
    }

    // Axioms ---------------------------------------------------------------------------

    fn gci(&mut self, body: Vec<BodyAtom>, items: Vec<Item>, source: usize) {
        self.queue.push(Gci {
            body,
            items,
            source,
        });
    }

    fn sub(&mut self, sub: ExprId, sup: ExprId, source: usize) {
        let (a, b) = (self.nnf_not(sub), self.nnf(sup));
        self.gci(Vec::new(), vec![Item::Expr(a), Item::Expr(b)], source);
    }

    /// `role(x, y)` as a body atom.
    fn edge(role: ObjProp, from: Var, to: Var) -> BodyAtom {
        match role {
            ObjProp::Named(p) => BodyAtom::Role(p, from, to),
            ObjProp::Inverse(p) => BodyAtom::Role(p, to, from),
        }
    }

    fn head_edge(role: ObjProp, from: Var, to: Var) -> HeadAtom {
        match role {
            ObjProp::Named(p) => HeadAtom::Role(p, from, to),
            ObjProp::Inverse(p) => HeadAtom::Role(p, to, from),
        }
    }

    /// Whether `axiom` holds in every interpretation by the meaning of the universal data
    /// property: `SubDataPropertyOf(p, owl:topDataProperty)`. Nothing to encode, so it is
    /// neither a clause nor unsupported (ontologies declare their data properties so).
    fn tautology(&self, axiom: &Axiom) -> bool {
        matches!(axiom, Axiom::SubDataPropertyOf(_, sup)
            if Some(*sup) == self.ontology.builtin.top_data)
    }

    /// Whether `axiom` mentions the universal data property (the object one is encoded
    /// before: `universal.rs`).
    fn uses_top(&self, axiom: &Axiom) -> bool {
        let Some(top) = self.ontology.builtin.top_data else {
            return false;
        };
        crate::properties::mentions(self.ontology, axiom, &|t| t == top)
    }

    /// The bottom properties relate nothing: `⊤ ⊑ ∀R.⊥` for the object property (a GCI,
    /// so that a chain reaching it passes R's automaton), `R(x, v) → ⊥` for the data
    /// property; from each axiom that mentions them.
    fn bottom_properties(&mut self) {
        let builtin = self.ontology.builtin;
        let ontology = self.ontology;
        for (index, axiom) in ontology.axioms.iter().enumerate() {
            if let Some(bottom) = builtin.bottom_object
                && crate::properties::mentions(ontology, axiom, &|t| t == bottom)
            {
                let nothing = self.e(ClassExpr::Nothing);
                let none = self.e(ClassExpr::All(ObjProp::Named(bottom), nothing));
                self.gci(Vec::new(), vec![Item::Expr(none)], index);
            }
            if let Some(bottom) = builtin.bottom_data
                && crate::properties::mentions(ontology, axiom, &|t| t == bottom)
            {
                self.add(
                    vec![BodyAtom::Data(bottom, Var::X, Var::V(0))],
                    Vec::new(),
                    index,
                );
            }
        }
    }

    fn axiom(&mut self, index: usize, axiom: &Axiom) {
        let (x, y) = (Var::X, Var::Y(0));
        match axiom {
            Axiom::Declaration(..) => {}
            Axiom::SubClassOf(a, b) => self.sub(*a, *b, index),
            Axiom::EquivalentClasses(xs) => {
                for pair in xs.windows(2) {
                    self.sub(pair[0], pair[1], index);
                    self.sub(pair[1], pair[0], index);
                }
            }
            Axiom::DisjointClasses(xs) => {
                for (i, &a) in xs.iter().enumerate() {
                    for &b in &xs[i + 1..] {
                        let (na, nb) = (self.nnf_not(a), self.nnf_not(b));
                        self.gci(Vec::new(), vec![Item::Expr(na), Item::Expr(nb)], index);
                    }
                }
            }
            Axiom::DisjointUnion(class, xs) => {
                let named = self.e(ClassExpr::Class(*class));
                let union = self.e(ClassExpr::Or(xs.clone()));
                self.sub(named, union, index);
                self.sub(union, named, index);
                for (i, &a) in xs.iter().enumerate() {
                    for &b in &xs[i + 1..] {
                        let (na, nb) = (self.nnf_not(a), self.nnf_not(b));
                        self.gci(Vec::new(), vec![Item::Expr(na), Item::Expr(nb)], index);
                    }
                }
            }
            Axiom::ObjectPropertyDomain(r, c) => {
                // The range of the inverse, `⊤ ⊑ ∀R⁻.C`, as a range goes: a non-simple R then
                // passes its (inverse) automaton, so an edge a chain or transitivity implies
                // fires the domain too, and the clauses stay Horn. A raw `R(x, y) → C(x)`
                // saw asserted edges only (found by the context core's EL gate); the form
                // `⊤ ⊑ C ⊔ ∀R.⊥` made every such domain disjunctive (found by the U1 bound,
                // whose split turned the disjunction into typing everything C: LUBM's
                // transitive subOrganizationOf; 3 October 2026).
                let c = self.nnf(*c);
                let all = self.e(ClassExpr::All(r.inverse(), c));
                self.gci(Vec::new(), vec![Item::Expr(all)], index);
            }
            Axiom::ObjectPropertyRange(r, c) => {
                let c = self.nnf(*c);
                let all = self.e(ClassExpr::All(*r, c));
                self.gci(Vec::new(), vec![Item::Expr(all)], index);
            }
            Axiom::ObjectCharacteristic(kind, r) => match kind {
                Characteristic::Functional | Characteristic::InverseFunctional => {
                    let role = match kind {
                        Characteristic::Functional => *r,
                        _ => r.inverse(),
                    };
                    let thing = self.e(ClassExpr::Thing);
                    let max = self.e(ClassExpr::Max(1, role, thing));
                    self.gci(Vec::new(), vec![Item::Expr(max)], index);
                }
                Characteristic::Reflexive => {
                    self.add(Vec::new(), vec![Self::head_edge(*r, x, x)], index);
                }
                Characteristic::Irreflexive => {
                    self.add(vec![Self::edge(*r, x, x)], Vec::new(), index);
                }
                Characteristic::Asymmetric => {
                    self.add(
                        vec![Self::edge(*r, x, y), Self::edge(*r, y, x)],
                        Vec::new(),
                        index,
                    );
                }
                Characteristic::Symmetric => {
                    self.add(
                        vec![Self::edge(*r, x, y)],
                        vec![Self::head_edge(*r, y, x)],
                        index,
                    );
                }
                // Through the automata.
                Characteristic::Transitive => {}
            },
            Axiom::SubObjectPropertyOf(chain, sup) => {
                if let [sub] = chain[..] {
                    self.add(
                        vec![Self::edge(sub, x, y)],
                        vec![Self::head_edge(*sup, x, y)],
                        index,
                    );
                }
            }
            Axiom::EquivalentObjectProperties(ps) => {
                for pair in ps.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    self.add(
                        vec![Self::edge(a, x, y)],
                        vec![Self::head_edge(b, x, y)],
                        index,
                    );
                    self.add(
                        vec![Self::edge(b, x, y)],
                        vec![Self::head_edge(a, x, y)],
                        index,
                    );
                }
            }
            Axiom::InverseObjectProperties(a, b) => {
                self.add(
                    vec![Self::edge(*a, x, y)],
                    vec![Self::head_edge(*b, y, x)],
                    index,
                );
                self.add(
                    vec![Self::edge(*b, x, y)],
                    vec![Self::head_edge(*a, y, x)],
                    index,
                );
            }
            Axiom::DisjointObjectProperties(ps) => {
                for (i, &a) in ps.iter().enumerate() {
                    for &b in &ps[i + 1..] {
                        self.add(
                            vec![Self::edge(a, x, y), Self::edge(b, x, y)],
                            Vec::new(),
                            index,
                        );
                    }
                }
            }
            Axiom::SubDataPropertyOf(a, b) => {
                let v = Var::V(0);
                self.add(
                    vec![BodyAtom::Data(*a, x, v)],
                    vec![HeadAtom::DataRole(*b, x, v)],
                    index,
                );
            }
            Axiom::EquivalentDataProperties(ps) => {
                let v = Var::V(0);
                for pair in ps.windows(2) {
                    self.add(
                        vec![BodyAtom::Data(pair[0], x, v)],
                        vec![HeadAtom::DataRole(pair[1], x, v)],
                        index,
                    );
                    self.add(
                        vec![BodyAtom::Data(pair[1], x, v)],
                        vec![HeadAtom::DataRole(pair[0], x, v)],
                        index,
                    );
                }
            }
            Axiom::DisjointDataProperties(ps) => {
                // Two data variables and their inequality, decided by the datatype theory:
                // one shared variable matched only values on one data node, and missed a
                // literal and an equal value made for an existential (W3C "Inconsistent
                // Disjoint Dataproperties" answered consistent, 5 October 2026).
                let (v, w) = (Var::V(0), Var::V(1));
                for (i, &a) in ps.iter().enumerate() {
                    for &b in &ps[i + 1..] {
                        self.add(
                            vec![BodyAtom::Data(a, x, v), BodyAtom::Data(b, x, w)],
                            vec![HeadAtom::DataUnequal(v, w)],
                            index,
                        );
                    }
                }
            }
            Axiom::DataPropertyDomain(d, c) => {
                let c = self.nnf(*c);
                self.gci(
                    vec![BodyAtom::Data(*d, x, Var::V(0))],
                    vec![Item::Expr(c)],
                    index,
                );
            }
            Axiom::DataPropertyRange(d, r) => {
                let v = Var::V(0);
                self.add(
                    vec![BodyAtom::Data(*d, x, v)],
                    vec![HeadAtom::DataIn(*r, v)],
                    index,
                );
            }
            Axiom::FunctionalDataProperty(d) => {
                let (v, w) = (Var::V(0), Var::V(1));
                self.add(
                    vec![BodyAtom::Data(*d, x, v), BodyAtom::Data(*d, x, w)],
                    vec![HeadAtom::DataEqual(v, w)],
                    index,
                );
            }
            Axiom::DatatypeDefinition(t, r) => {
                self.out
                    .unsupported
                    .push((index, UNSUPPORTED_DATATYPE_DEFINITIONS));
                self.out.definitions.push((*t, *r));
            }
            Axiom::HasKey(c, objects, data) => {
                self.out.unsupported.push((index, UNSUPPORTED_KEYS));
                self.key(index, *c, objects, data);
            }
            Axiom::ClassAssertion(c, a) => {
                let c = self.nnf(*c);
                let concept = match self.get(c) {
                    ClassExpr::Class(name) => Concept::Named(name),
                    ClassExpr::Thing => return,
                    _ => Concept::Fresh(self.fresh_positive(c, index)),
                };
                self.out.facts.concepts.push((concept, *a, index));
            }
            Axiom::ObjectPropertyAssertion(p, a, b) => {
                self.out.facts.roles.push((*p, *a, *b, index))
            }
            Axiom::NegativeObjectPropertyAssertion(p, a, b) => {
                self.out.facts.not_roles.push((*p, *a, *b, index))
            }
            Axiom::DataPropertyAssertion(p, a, v) => self.out.facts.data.push((*p, *a, *v, index)),
            Axiom::NegativeDataPropertyAssertion(p, a, v) => {
                self.out.facts.not_data.push((*p, *a, *v, index))
            }
            Axiom::SameIndividual(xs) => {
                for pair in xs.windows(2) {
                    self.out.facts.same.push((pair[0], pair[1], index));
                }
            }
            Axiom::DifferentIndividuals(xs) => {
                for (i, &a) in xs.iter().enumerate() {
                    for &b in &xs[i + 1..] {
                        self.out.facts.different.push((a, b, index));
                    }
                }
            }
        }
    }

    /// A key as a DL-safe rule (`SafeRule`).
    fn key(&mut self, index: usize, class: ExprId, objects: &[ObjProp], data: &[Term]) {
        let (x, y) = (Var::X, Var::Y(0));
        let mut body = Vec::new();
        let c = self.nnf(class);
        let concept = match self.get(c) {
            ClassExpr::Thing => None,
            ClassExpr::Class(a) => Some(Concept::Named(a)),
            _ => Some(Concept::Fresh(self.fresh_negative(c, index))),
        };
        if let Some(concept) = concept {
            body.push(BodyAtom::Concept(concept, x));
            body.push(BodyAtom::Concept(concept, y));
        }
        for (i, &p) in objects.iter().enumerate() {
            let z = Var::Y(i as u16 + 1);
            body.push(Self::edge(p, x, z));
            body.push(Self::edge(p, y, z));
        }
        for (j, &d) in data.iter().enumerate() {
            let v = Var::V(j as u16);
            body.push(BodyAtom::Data(d, x, v));
            body.push(BodyAtom::Data(d, y, v));
        }
        self.out.rules.push(SafeRule {
            body,
            head: vec![HeadAtom::Equal(x, y)],
            source: index,
        });
    }

    // Fresh names ----------------------------------------------------------------------

    fn new_fresh(&mut self, of: FreshOf) -> u32 {
        self.out.fresh.push(of);
        (self.out.fresh.len() - 1) as u32
    }

    /// A fresh `Q ⊑ e` (`e` in negation normal form).
    fn fresh_positive(&mut self, e: ExprId, source: usize) -> u32 {
        if let Some(&q) = self.positive.get(&e) {
            return q;
        }
        let q = self.new_fresh(FreshOf::Expr {
            expr: e,
            positive: true,
        });
        self.positive.insert(e, q);
        self.gci(Vec::new(), vec![Item::NotFresh(q), Item::Expr(e)], source);
        q
    }

    /// A fresh `e ⊑ Q` (`e` in negation normal form).
    fn fresh_negative(&mut self, e: ExprId, source: usize) -> u32 {
        if let Some(&q) = self.negative.get(&e) {
            return q;
        }
        let q = self.new_fresh(FreshOf::Expr {
            expr: e,
            positive: false,
        });
        self.negative.insert(e, q);
        if let ClassExpr::Some(r, d) = self.get(e)
            && self.non_simple.contains(&r.named())
        {
            // `∃R.D ⊑ Q` as `D ⊑ ∀R⁻.Q`: Horn through R⁻'s automaton where D is a
            // literal (`∀R.¬D ⊔ Q` would put R's start state beside Q in a head).
            let start = self.automaton(r.inverse(), Item::Fresh(q), source);
            let not_d = self.nnf_not(d);
            self.gci(
                Vec::new(),
                vec![Item::Expr(not_d), Item::Fresh(start)],
                source,
            );
            return q;
        }
        let not = self.nnf_not(e);
        self.gci(Vec::new(), vec![Item::Expr(not), Item::Fresh(q)], source);
        q
    }

    /// Whether `c` (in negation normal form) clausifies into body atoms only: a negated
    /// name or nominal, or `⊥`.
    fn negative_filler(&self, c: ExprId) -> bool {
        match self.get(c) {
            ClassExpr::Nothing => true,
            ClassExpr::Not(inner) => match self.get(inner) {
                ClassExpr::Class(_) => true,
                ClassExpr::OneOf(xs) => xs.len() == 1,
                _ => false,
            },
            _ => false,
        }
    }

    // Clausification -------------------------------------------------------------------

    fn drain(&mut self) {
        while let Some(gci) = self.queue.pop() {
            self.clausify(gci);
        }
    }

    /// The disjuncts of `items`, flattened; `None` if one is `⊤` (a tautology).
    fn flatten(&mut self, items: Vec<Item>) -> Option<Vec<Item>> {
        let mut out = Vec::new();
        let mut stack = items;
        while let Some(item) = stack.pop() {
            match item {
                Item::Expr(e) => match self.get(e) {
                    ClassExpr::Thing => return None,
                    ClassExpr::Nothing => {}
                    ClassExpr::Or(xs) => stack.extend(xs.into_iter().map(Item::Expr)),
                    _ => out.push(item),
                },
                _ => out.push(item),
            }
        }
        out.sort_by_key(|i| format!("{i:?}"));
        out.dedup();
        Some(out)
    }

    fn clausify(&mut self, gci: Gci) {
        let Some(items) = self.flatten(gci.items) else {
            return;
        };
        // A conjunction among the disjuncts: a GCI per conjunct, `(A ⊓ B) ⊔ R` as
        // `A ⊔ R` and `B ⊔ R`, so that `A ⊔ B ⊑ C` gives the Horn clauses `A → C` and
        // `B → C` (a fresh name for `¬A ⊓ ¬B` made it `⊤ → C ∨ Q`: found by the U1
        // bound, which then typed everything C on OWL2Bench; 4 October 2026). Up to
        // `MAX_DISTRIBUTED` GCIs from one; past that, fresh names.
        let conjuncts: Vec<(ExprId, usize)> = items
            .iter()
            .filter_map(|i| match i {
                Item::Expr(e) => match self.get(*e) {
                    ClassExpr::And(xs) => Some((*e, xs.len())),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let product = conjuncts
            .iter()
            .try_fold(1usize, |acc, &(_, n)| acc.checked_mul(n))
            .unwrap_or(usize::MAX);
        if let Some(&(e, _)) = conjuncts.first()
            && product <= MAX_DISTRIBUTED
            && let ClassExpr::And(xs) = self.get(e)
        {
            for x in xs {
                let mut split: Vec<Item> = items
                    .iter()
                    .copied()
                    .filter(|i| *i != Item::Expr(e))
                    .collect();
                split.push(Item::Expr(x));
                self.gci(gci.body.clone(), split, gci.source);
            }
            return;
        }
        let mut body = gci.body;
        let mut head = Vec::new();
        let mut next_y = body
            .iter()
            .filter_map(|b| match b {
                BodyAtom::Role(_, a, b) => Some([*a, *b]),
                _ => None,
            })
            .flatten()
            .filter_map(|v| match v {
                Var::Y(i) => Some(i + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let mut next_v = body
            .iter()
            .filter_map(|b| match b {
                BodyAtom::Data(_, _, Var::V(i)) => Some(i + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let many = items.len() > 1;
        for item in items {
            let e = match item {
                Item::Fresh(q) => {
                    head.push(HeadAtom::Concept(Concept::Fresh(q), Var::X));
                    continue;
                }
                Item::NotFresh(q) => {
                    body.push(BodyAtom::Concept(Concept::Fresh(q), Var::X));
                    continue;
                }
                Item::Expr(e) => e,
            };
            match self.get(e) {
                ClassExpr::Class(a) => head.push(HeadAtom::Concept(Concept::Named(a), Var::X)),
                ClassExpr::Not(inner) => match self.get(inner) {
                    ClassExpr::Class(a) => body.push(BodyAtom::Concept(Concept::Named(a), Var::X)),
                    ClassExpr::OneOf(xs) if xs.len() == 1 => {
                        body.push(BodyAtom::Nominal(xs[0], Var::X))
                    }
                    ClassExpr::HasSelf(r) => body.push(Self::edge(r, Var::X, Var::X)),
                    _ => unreachable!("negation normal form"),
                },
                ClassExpr::OneOf(xs) => {
                    for a in xs {
                        head.push(HeadAtom::Nominal(a, Var::X));
                    }
                }
                ClassExpr::HasSelf(r) => head.push(Self::head_edge(r, Var::X, Var::X)),
                ClassExpr::And(_) => {
                    let q = self.fresh_positive(e, gci.source);
                    head.push(HeadAtom::Concept(Concept::Fresh(q), Var::X));
                }
                ClassExpr::Some(r, c) => {
                    let filler = self.filler(c, gci.source);
                    head.push(HeadAtom::AtLeast {
                        n: 1,
                        role: r,
                        filler,
                        var: Var::X,
                    });
                }
                ClassExpr::Min(0, _, _) => return,
                ClassExpr::Min(n, r, c) => {
                    let filler = self.filler(c, gci.source);
                    head.push(HeadAtom::AtLeast {
                        n,
                        role: r,
                        filler,
                        var: Var::X,
                    });
                }
                ClassExpr::All(r, c) => {
                    if matches!(self.get(c), ClassExpr::Thing) {
                        return;
                    }
                    if self.non_simple.contains(&r.named()) {
                        // `∀R.¬D` beside other disjuncts: `¬P` for a name `∃R.D ⊑ P`, which
                        // goes through R⁻'s automaton as `D ⊑ ∀R⁻.P` and stays Horn (the
                        // start state of R's own automaton made `A ⊓ ∃R.D ⊑ B` the
                        // disjunction `A → B ∨ Q`; 4 October 2026).
                        if many && self.negative_filler(c) {
                            let d = self.nnf_not(c);
                            let some = self.e(ClassExpr::Some(r, d));
                            let p = self.fresh_negative(some, gci.source);
                            body.push(BodyAtom::Concept(Concept::Fresh(p), Var::X));
                            continue;
                        }
                        // Along the automaton of r: its start state at x.
                        let start = self.automaton(r, Item::Expr(c), gci.source);
                        head.push(HeadAtom::Concept(Concept::Fresh(start), Var::X));
                        continue;
                    }
                    let y = Var::Y(next_y);
                    next_y += 1;
                    body.push(Self::edge(r, Var::X, y));
                    self.at(c, y, &mut body, &mut head, gci.source);
                }
                ClassExpr::Max(n, r, c) => {
                    let filler = match self.get(c) {
                        ClassExpr::Thing => None,
                        ClassExpr::Class(a) => Some(Concept::Named(a)),
                        _ => Some(Concept::Fresh(self.fresh_negative(c, gci.source))),
                    };
                    if n > self.expand_up_to {
                        head.push(HeadAtom::AtMost {
                            n,
                            role: r,
                            filler: filler.map_or(Filler::Top, Filler::Is),
                            var: Var::X,
                        });
                        continue;
                    }
                    let ys: Vec<Var> = (0..=n).map(|i| Var::Y(next_y + i as u16)).collect();
                    next_y += n as u16 + 1;
                    for &y in &ys {
                        body.push(Self::edge(r, Var::X, y));
                        if let Some(concept) = filler {
                            body.push(BodyAtom::Concept(concept, y));
                        }
                    }
                    for (i, &a) in ys.iter().enumerate() {
                        for &b in &ys[i + 1..] {
                            head.push(HeadAtom::Equal(a, b));
                        }
                    }
                }
                ClassExpr::DataSome(d, r) => head.push(HeadAtom::DataAtLeast {
                    n: 1,
                    property: d,
                    range: r,
                    var: Var::X,
                }),
                ClassExpr::DataMin(0, _, _) => return,
                ClassExpr::DataMin(n, d, r) => head.push(HeadAtom::DataAtLeast {
                    n,
                    property: d,
                    range: r,
                    var: Var::X,
                }),
                ClassExpr::DataAll(d, r) => {
                    let v = Var::V(next_v);
                    next_v += 1;
                    body.push(BodyAtom::Data(d, Var::X, v));
                    head.push(HeadAtom::DataIn(r, v));
                }
                ClassExpr::DataMax(n, d, r) if n > self.expand_up_to => {
                    head.push(HeadAtom::DataAtMost {
                        n,
                        property: d,
                        range: r,
                        var: Var::X,
                    })
                }
                ClassExpr::DataMax(n, d, r) => {
                    let vs: Vec<Var> = (0..=n).map(|i| Var::V(next_v + i as u16)).collect();
                    next_v += n as u16 + 1;
                    let outside = self.range_not(r);
                    for &v in &vs {
                        body.push(BodyAtom::Data(d, Var::X, v));
                        head.push(HeadAtom::DataIn(outside, v));
                    }
                    for (i, &a) in vs.iter().enumerate() {
                        for &b in &vs[i + 1..] {
                            head.push(HeadAtom::DataEqual(a, b));
                        }
                    }
                }
                ClassExpr::Thing | ClassExpr::Nothing | ClassExpr::Or(_) => {
                    unreachable!("flattened")
                }
                other => unreachable!("not in negation normal form: {other:?}"),
            }
        }
        self.add(body, head, gci.source);
    }

    /// The filler of an at-least atom for `c`.
    fn filler(&mut self, c: ExprId, source: usize) -> Filler {
        match self.get(c) {
            ClassExpr::Thing => Filler::Top,
            ClassExpr::Class(a) => Filler::Is(Concept::Named(a)),
            ClassExpr::Not(inner) if matches!(self.get(inner), ClassExpr::Class(_)) => {
                let ClassExpr::Class(a) = self.get(inner) else {
                    unreachable!()
                };
                Filler::Not(Concept::Named(a))
            }
            _ => Filler::Is(Concept::Fresh(self.fresh_positive(c, source))),
        }
    }

    /// `c` (positive) at the successor `y`, into the clause's body or head.
    fn at(
        &mut self,
        c: ExprId,
        y: Var,
        body: &mut Vec<BodyAtom>,
        head: &mut Vec<HeadAtom>,
        source: usize,
    ) {
        match self.get(c) {
            ClassExpr::Nothing => {}
            ClassExpr::Class(a) => head.push(HeadAtom::Concept(Concept::Named(a), y)),
            ClassExpr::Not(inner) => match self.get(inner) {
                ClassExpr::Class(a) => body.push(BodyAtom::Concept(Concept::Named(a), y)),
                ClassExpr::OneOf(xs) if xs.len() == 1 => body.push(BodyAtom::Nominal(xs[0], y)),
                _ => {
                    let q = self.fresh_positive(c, source);
                    head.push(HeadAtom::Concept(Concept::Fresh(q), y));
                }
            },
            ClassExpr::OneOf(xs) => {
                for a in xs {
                    head.push(HeadAtom::Nominal(a, y));
                }
            }
            _ => {
                let q = self.fresh_positive(c, source);
                head.push(HeadAtom::Concept(Concept::Fresh(q), y));
            }
        }
    }

    fn add(&mut self, body: Vec<BodyAtom>, head: Vec<HeadAtom>, source: usize) {
        self.add_needing(body, head, vec![source]);
    }

    /// Adds the clause that `needed`'s axioms give together; a clause already there gets
    /// `needed` as another way to it.
    fn add_needing(&mut self, body: Vec<BodyAtom>, head: Vec<HeadAtom>, needed: Vec<usize>) {
        let clause = Clause::new(body, head, needed.clone());
        let key = (clause.body.clone(), clause.head.clone());
        if let Some(&at) = self.clause_index.get(&key) {
            let sources = &mut self.out.clauses[at].sources;
            if !sources.contains(&needed) {
                sources.push(needed);
            }
            return;
        }
        self.clause_index.insert(key, self.out.clauses.len());
        self.out.clauses.push(clause);
    }

    // Role inclusions and automata -----------------------------------------------------

    /// The role inclusions of the RBox, each over a named superrole, and the non-simple
    /// roles.
    fn role_inclusions(&mut self) {
        let mut rias: Vec<(Vec<ObjProp>, ObjProp, usize)> = Vec::new();
        for (index, axiom) in self.ontology.axioms.iter().enumerate() {
            if self.uses_top(axiom) {
                continue;
            }
            match axiom {
                Axiom::SubObjectPropertyOf(chain, sup) => rias.push((chain.clone(), *sup, index)),
                Axiom::EquivalentObjectProperties(ps) => {
                    for &a in ps {
                        for &b in ps {
                            if a != b {
                                rias.push((vec![a], b, index));
                            }
                        }
                    }
                }
                Axiom::InverseObjectProperties(a, b) => {
                    rias.push((vec![*a], b.inverse(), index));
                    rias.push((vec![*b], a.inverse(), index));
                }
                Axiom::ObjectCharacteristic(Characteristic::Symmetric, r) => {
                    rias.push((vec![r.inverse()], *r, index));
                }
                Axiom::ObjectCharacteristic(Characteristic::Transitive, r) => {
                    rias.push((vec![*r, *r], *r, index));
                }
                _ => {}
            }
        }
        // Over a named superrole: w ⊑ R⁻ is w⁻ reversed ⊑ R.
        for (chain, sup, index) in rias {
            let (chain, named) = match sup {
                ObjProp::Named(p) => (chain, p),
                ObjProp::Inverse(p) => (chain.iter().rev().map(|r| r.inverse()).collect(), p),
            };
            self.rias.push((chain, named, index));
        }
        for (chain, sup, axiom) in &self.rias {
            if let [only] = chain[..] {
                for (a, b) in [
                    (only, ObjProp::Named(*sup)),
                    (only.inverse(), ObjProp::Inverse(*sup)),
                ] {
                    self.up.entry(a).or_default().push((b, *axiom));
                    self.down.entry(b).or_default().push((a, *axiom));
                }
            }
        }
        // Non-simple: the superroles of chains and transitive roles, up the hierarchy.
        let mut seeds: Vec<Term> = self
            .rias
            .iter()
            .filter(|(chain, ..)| chain.len() > 1)
            .map(|&(_, sup, _)| sup)
            .collect();
        while let Some(role) = seeds.pop() {
            if self.non_simple.insert(role) {
                for (chain, sup, _) in &self.rias {
                    if chain.len() == 1 && chain[0].named() == role {
                        seeds.push(*sup);
                    }
                }
            }
        }
    }

    /// The role expressions equivalent to `r` (`r` included), each with the role
    /// inclusions that make it a subrole of `r`: those `r` reaches by the single-role
    /// inclusions and that reach `r` back. `R ⊑ S⁻` with `S ⊑ R⁻` (an inverse pair)
    /// makes `S⁻` one of `R`'s.
    fn equivalents(&self, r: ObjProp) -> Vec<(ObjProp, Vec<usize>)> {
        if let Some(known) = self.equivalent.borrow().get(&r) {
            return known.clone();
        }
        // The role expressions reachable from `from` along `edges`, each with the
        // inclusions on the way (breadth first: one path each).
        let reach = |edges: &HashMap<ObjProp, Vec<(ObjProp, usize)>>| {
            let mut seen: HashMap<ObjProp, Vec<usize>> = HashMap::new();
            seen.insert(r, Vec::new());
            let mut queue = vec![r];
            let mut at = 0;
            while at < queue.len() {
                let x = queue[at];
                at += 1;
                for &(y, axiom) in edges.get(&x).into_iter().flatten() {
                    if !seen.contains_key(&y) {
                        let mut via = seen[&x].clone();
                        via.push(axiom);
                        seen.insert(y, via);
                        queue.push(y);
                    }
                }
            }
            seen
        };
        let above = reach(&self.up);
        let below = reach(&self.down);
        // Equivalent: above and below; a member needs the inclusions that lead it to `r`.
        let mut out: Vec<(ObjProp, Vec<usize>)> = below
            .into_iter()
            .filter(|(x, _)| above.contains_key(x))
            .map(|(x, mut via)| {
                via.sort_unstable();
                via.dedup();
                (x, via)
            })
            .collect();
        out.sort();
        self.equivalent.borrow_mut().insert(r, out.clone());
        out
    }

    /// The automaton of the non-simple role `role` (named), with the automata of the
    /// non-simple roles it builds on spliced in; `None` for an irregular RBox.
    ///
    /// Roles equivalent to `role` (up to inverse: an inverse pair, a cycle of
    /// subproperties) are one role here (Horrocks and Sattler's order is on their
    /// classes): their inclusions are read as `role`'s, and an edge of `role` matches an
    /// edge of each of them. Splicing them into each other was taken for a cycle, an
    /// irregular RBox (ore_ont_15971: `after` inverse of the transitive `before`).
    fn nfa(&self, role: Term, stack: &mut Vec<Term>) -> Option<Nfa> {
        if stack.contains(&role) {
            return None;
        }
        let r = ObjProp::Named(role);
        let members = self.equivalents(r);
        let class: Vec<ObjProp> = members.iter().map(|(x, _)| *x).collect();
        let names: Vec<Term> = class.iter().map(|x| x.named()).collect();
        stack.extend(&names);
        // A label as `role` where it is one of `role`'s class.
        let canonical = |x: ObjProp| -> ObjProp {
            if class.contains(&x) {
                r
            } else if class.contains(&x.inverse()) {
                r.inverse()
            } else {
                x
            }
        };
        let mut nfa = Nfa {
            states: 2,
            start: 0,
            end: 1,
            edges: vec![(0, Some(r), 1, Vec::new())],
        };
        // A path for the role inclusion `axiom`: each of its edges needs that axiom.
        let path = |nfa: &mut Nfa, from: u32, labels: &[ObjProp], to: u32, axiom: usize| {
            let mut at = from;
            for (i, &label) in labels.iter().enumerate() {
                let next = if i + 1 == labels.len() {
                    to
                } else {
                    nfa.states += 1;
                    nfa.states - 1
                };
                nfa.edges.push((at, Some(label), next, vec![axiom]));
                at = next;
            }
            if labels.is_empty() {
                nfa.edges.push((from, None, to, vec![axiom]));
            }
        };
        for (chain, sup, axiom) in &self.rias {
            // The inclusions into the class, as inclusions into `role`.
            let chain: Vec<ObjProp> = if class.contains(&ObjProp::Named(*sup)) {
                chain.iter().map(|&x| canonical(x)).collect()
            } else if class.contains(&ObjProp::Inverse(*sup)) {
                chain
                    .iter()
                    .rev()
                    .map(|&x| canonical(x.inverse()))
                    .collect()
            } else {
                continue;
            };
            let axiom = *axiom;
            if chain[..] == [r] {
                // Within the class: its members' edges are `role`'s (below).
                continue;
            }
            if chain[..] == [r, r] {
                nfa.edges.push((1, None, 0, vec![axiom]));
            } else if chain.len() > 1 && chain[0] == r {
                path(&mut nfa, 1, &chain[1..], 1, axiom);
            } else if chain.len() > 1 && chain[chain.len() - 1] == r {
                path(&mut nfa, 0, &chain[..chain.len() - 1], 0, axiom);
            } else {
                path(&mut nfa, 0, &chain, 1, axiom);
            }
        }
        // An edge of `role` (or its inverse) matches each member of the class; the other
        // non-simple roles on the edges have their automata spliced in.
        let edges = std::mem::take(&mut nfa.edges);
        for (a, label, b, axioms) in edges {
            match label {
                Some(s) if s.named() == role => {
                    for (member, via) in &members {
                        let member = if s == r { *member } else { member.inverse() };
                        // The edge needs the inclusions that make the member `role`.
                        let mut needed = axioms.clone();
                        needed.extend(via);
                        needed.sort_unstable();
                        needed.dedup();
                        nfa.edges.push((a, Some(member), b, needed));
                    }
                }
                Some(s) if self.non_simple.contains(&s.named()) => {
                    let inner = self.nfa(s.named(), stack)?;
                    let inner = match s {
                        ObjProp::Named(_) => inner,
                        ObjProp::Inverse(_) => inner.inverse(),
                    };
                    let offset = nfa.states;
                    nfa.states += inner.states;
                    // The spliced automaton stands for the edge: it needs the edge's axioms,
                    // and its own edges theirs too.
                    nfa.edges
                        .push((a, None, inner.start + offset, axioms.clone()));
                    nfa.edges
                        .push((inner.end + offset, None, b, axioms.clone()));
                    for (p, l, q, inner_axioms) in inner.edges {
                        let mut needed = axioms.clone();
                        needed.extend(inner_axioms);
                        nfa.edges.push((p + offset, l, q + offset, needed));
                    }
                }
                _ => nfa.edges.push((a, label, b, axioms)),
            }
        }
        stack.truncate(stack.len() - names.len());
        Some(nfa)
    }

    /// The start state's fresh name of `∀ role. filler` along `role`'s automaton.
    fn automaton(&mut self, role: ObjProp, filler: Item, source: usize) -> u32 {
        if let Some(&start) = self.automata.get(&(role, filler)) {
            return start;
        }
        let Some(nfa) = self.nfa(role.named(), &mut Vec::new()) else {
            self.out
                .unsupported
                .push((source, "an irregular role hierarchy"));
            // Read as over the role alone (sound for what it derives).
            let q = self.new_fresh(FreshOf::State { role, state: 0 });
            self.automata.insert((role, filler), q);
            return q;
        };
        let nfa = match role {
            ObjProp::Named(_) => nfa,
            ObjProp::Inverse(_) => nfa.inverse(),
        };
        let names: Vec<u32> = (0..nfa.states)
            .map(|state| self.new_fresh(FreshOf::State { role, state }))
            .collect();
        self.automata
            .insert((role, filler), names[nfa.start as usize]);
        let state = |s: u32| Concept::Fresh(names[s as usize]);
        // A transition needs the universal and the role inclusions of its edge together
        // (justifications through a role hierarchy or chain name them: found by the
        // context core's proof test, 3 October 2026).
        for (a, label, b, axioms) in &nfa.edges {
            let mut needed = vec![source];
            needed.extend(axioms);
            needed.sort_unstable();
            needed.dedup();
            match label {
                Some(edge) => self.add_needing(
                    vec![
                        BodyAtom::Concept(state(*a), Var::X),
                        Self::edge(*edge, Var::X, Var::Y(0)),
                    ],
                    vec![HeadAtom::Concept(state(*b), Var::Y(0))],
                    needed,
                ),
                None => self.add_needing(
                    vec![BodyAtom::Concept(state(*a), Var::X)],
                    vec![HeadAtom::Concept(state(*b), Var::X)],
                    needed,
                ),
            }
        }
        // The end state: the filler holds.
        self.gci(
            Vec::new(),
            vec![Item::NotFresh(names[nfa.end as usize]), filler],
            source,
        );
        names[nfa.start as usize]
    }

    fn finish(self) -> Normalised {
        let mut out = self.out;
        out.classes = self.classes;
        out.ranges = self.ranges;
        out
    }
}
