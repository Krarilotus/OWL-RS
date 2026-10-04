//! The datalog program U1 is: rules over triple patterns, as the store's rule reasoner
//! evaluates them (`nrese-reasoner`'s rule IR has the same shape), each with where it
//! came from and which over-approximation made it.
//!
//! The program is plain data over the source's term ids: no dependency on the reasoner.
//! A caller hands it to an engine by copying atoms one to one ([`Rule::body`],
//! [`Rule::head`]), or as Notation3 text ([`Program::to_n3`]), the store's user-rule
//! language.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;

use nrese_owl::{Axiom, ClassExpr, EntityKind, Normalised, ObjProp, Ontology, Term};

/// A position of an atom: a rule variable (numbered densely from 0) or a term id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    Var(u8),
    Const(Term),
}

/// A triple pattern `(subject predicate object)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Atom(pub [Slot; 3]);

impl Atom {
    /// The ground triple, if the atom has no variable.
    pub fn ground(&self) -> Option<[Term; 3]> {
        let mut out = [0; 3];
        for (slot, value) in self.0.iter().zip(&mut out) {
            match slot {
                Slot::Const(t) => *value = *t,
                Slot::Var(_) => return None,
            }
        }
        Some(out)
    }
}

/// What a rule of U1 was compiled from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// A DL-clause (an index into [`nrese_owl::Normalised::clauses`]).
    Clause(usize),
    /// A role chain or transitivity axiom (the clauses leave chains to automata; U1
    /// materialises the role atoms they entail).
    Chain,
    /// A key (`HasKey`), which the clauses don't cover.
    Key,
    /// The axiomatisation of `owl:Thing` (PAGOdA §2.2: `A(x) → ⊤(x)`, `R(x, y) → ⊤(x), ⊤(y)`).
    Thing,
    /// The axiomatisation of equality (PAGOdA §2, EQ2–EQ4), by OWL 2 RL's rule names so
    /// that the reasoner's equality module replaces the copying rules.
    Equality,
    /// An ABox axiom whose meaning the triples don't state in U1's vocabulary: a complex
    /// class assertion (through its fresh name), a negative assertion, `DifferentIndividuals`.
    Assertion,
    /// The class `{a}` of an individual a rule body names, with the fact `a ∈ {a}`.
    Nominal,
    /// The semantics of OWL's built-in vocabulary: `owl:bottomObjectProperty` and
    /// `owl:bottomDataProperty` hold for no pair; differences and negative property
    /// assertions, stated as facts over U1's own predicates, clash with an equality and
    /// with the pair they deny. (The facts carry their assertions as sources.)
    Builtin,
    /// The distinctness and complement conditions of a c-Skolemised at-least atom
    /// (PAGOdA §2.2, footnote 4: `≥ n R.B` as `n` successors, pairwise different).
    Skolem,
}

/// The over-approximations of PAGOdA's datalog strengthening (§5.1) a rule carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Approximations {
    /// A disjunctive head split into a conjunction (Definition 5.1, the second case).
    pub split: bool,
    /// An existential c-Skolemised to constants (Definition 4.7).
    pub skolem: bool,
    /// `⊥` replaced by the clash predicate with no meaning (Definition 5.1, the first case).
    pub bottom: bool,
    /// An at-most restriction strengthened to the equality of all its successors.
    pub at_most: bool,
    /// An at-least restriction over more successors than [`super::Options::max_skolems`],
    /// c-Skolemised to that many constants: a homomorphic image of the full encoding,
    /// so every answer and every clash of it is kept, but a clash-free U1 is no longer a
    /// model of the restriction (the module docs of [`super::upper`]).
    pub collapsed: bool,
    /// Head atoms over data ranges dropped: no rule body reads them, so no class, role
    /// or data property atom is lost.
    pub data: bool,
}

impl Approximations {
    /// Whether the rule is exactly what it was compiled from.
    pub fn exact(&self) -> bool {
        *self == Self::default()
    }
}

/// Where a rule came from: its origin and the ontology's axioms (by index), with the
/// approximations it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub origin: Origin,
    /// Alternative sets of the ontology's axioms (by index), each of which gives the
    /// rule together (as `nrese_owl::Clause::sources`); empty for the axiomatic rules.
    pub sources: Vec<Vec<usize>>,
    pub approximations: Approximations,
}

/// A rule: the body's atoms jointly imply every head atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// `u1-c{clause}` and the like; OWL 2 RL's names for the equality rules.
    pub name: String,
    pub body: Vec<Atom>,
    /// Pairs of body terms that must be bound to different terms (the reasoner's
    /// `NotEqual` guard, N3's `log:notEqualTo`).
    pub distinct: Vec<(Slot, Slot)>,
    pub head: Vec<Atom>,
    pub provenance: Provenance,
}

impl Rule {
    /// Number of distinct variables (numbered densely from 0).
    pub fn variables(&self) -> usize {
        self.body
            .iter()
            .chain(&self.head)
            .flat_map(|a| a.0)
            .filter_map(|s| match s {
                Slot::Var(v) => Some(usize::from(v) + 1),
                Slot::Const(_) => None,
            })
            .max()
            .unwrap_or(0)
    }
}

/// The terms U1 adds to the source's: fresh classes, Skolem constants, the clash
/// predicate, and the OWL terms it uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names {
    pub rdf_type: Term,
    pub same_as: Term,
    pub thing: Term,
    /// `(x clash clash)`: a `⊥` clause fired at `x`. Without meaning in U1 (PAGOdA's
    /// `⊥s`); if no clash is derived, the ontology is consistent (Theorem 5.5 (i)).
    pub clash: Term,
    /// The class of each fresh name of the clauses, by its number.
    pub fresh: Vec<Term>,
    /// The Skolem constants, in the order they were made.
    pub skolems: Vec<Term>,
    /// The classes that say a part of a rule body is satisfiable (the exact rewriting of
    /// `project`, against cross products of independent successors).
    pub projections: Vec<Term>,
}

/// The entities of the ontology U1 is over (named only, never U1's own terms).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signature {
    pub classes: Vec<Term>,
    pub object_properties: Vec<Term>,
    pub data_properties: Vec<Term>,
    pub individuals: Vec<Term>,
}

impl Signature {
    /// The named entities of the ontology and its clauses.
    pub(crate) fn of(ontology: &Ontology, n: &Normalised) -> Self {
        let mut classes = BTreeSet::new();
        let mut roles = BTreeSet::new();
        let mut data = BTreeSet::new();
        let mut individuals = BTreeSet::new();
        for id in 0..n.classes.len() as u32 {
            match n.classes.get(id) {
                ClassExpr::Class(t) => {
                    classes.insert(*t);
                }
                ClassExpr::OneOf(xs) => individuals.extend(xs.iter().copied()),
                ClassExpr::HasValue(r, a) => {
                    roles.insert(r.named());
                    individuals.insert(*a);
                }
                ClassExpr::Some(r, _)
                | ClassExpr::All(r, _)
                | ClassExpr::HasSelf(r)
                | ClassExpr::Min(_, r, _)
                | ClassExpr::Max(_, r, _)
                | ClassExpr::Exact(_, r, _) => {
                    roles.insert(r.named());
                }
                ClassExpr::DataSome(d, _)
                | ClassExpr::DataAll(d, _)
                | ClassExpr::DataHasValue(d, _)
                | ClassExpr::DataMin(_, d, _)
                | ClassExpr::DataMax(_, d, _)
                | ClassExpr::DataExact(_, d, _) => {
                    data.insert(*d);
                }
                _ => {}
            }
        }
        for axiom in &ontology.axioms {
            let mut role = |ps: &[ObjProp]| roles.extend(ps.iter().map(|p| p.named()));
            match axiom {
                Axiom::Declaration(EntityKind::Class, t) | Axiom::DisjointUnion(t, _) => {
                    classes.insert(*t);
                }
                Axiom::Declaration(EntityKind::ObjectProperty, t) => role(&[ObjProp::Named(*t)]),
                Axiom::Declaration(EntityKind::DataProperty, t)
                | Axiom::DataPropertyDomain(t, _)
                | Axiom::DataPropertyRange(t, _)
                | Axiom::FunctionalDataProperty(t) => {
                    data.insert(*t);
                }
                Axiom::Declaration(EntityKind::NamedIndividual, t)
                | Axiom::ClassAssertion(_, t) => {
                    individuals.insert(*t);
                }
                Axiom::SubObjectPropertyOf(chain, sup) => {
                    role(chain);
                    role(&[*sup]);
                }
                Axiom::EquivalentObjectProperties(ps) | Axiom::DisjointObjectProperties(ps) => {
                    role(ps)
                }
                Axiom::InverseObjectProperties(a, b) => role(&[*a, *b]),
                Axiom::ObjectPropertyDomain(r, _)
                | Axiom::ObjectPropertyRange(r, _)
                | Axiom::ObjectCharacteristic(_, r) => role(&[*r]),
                Axiom::HasKey(_, ps, ds) => {
                    role(ps);
                    data.extend(ds.iter().copied());
                }
                Axiom::SubDataPropertyOf(a, b) => data.extend([*a, *b]),
                Axiom::EquivalentDataProperties(ds) | Axiom::DisjointDataProperties(ds) => {
                    data.extend(ds.iter().copied())
                }
                Axiom::ObjectPropertyAssertion(p, a, b)
                | Axiom::NegativeObjectPropertyAssertion(p, a, b) => {
                    role(&[ObjProp::Named(*p)]);
                    individuals.extend([*a, *b]);
                }
                Axiom::DataPropertyAssertion(d, a, _)
                | Axiom::NegativeDataPropertyAssertion(d, a, _) => {
                    data.insert(*d);
                    individuals.insert(*a);
                }
                Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => {
                    individuals.extend(xs.iter().copied())
                }
                _ => {}
            }
        }
        Self {
            classes: classes.into_iter().collect(),
            object_properties: roles.into_iter().collect(),
            data_properties: data.into_iter().collect(),
            individuals: individuals.into_iter().collect(),
        }
    }
}

/// The upper-bound program U1 (PAGOdA's datalog strengthening `str(K)`, §5.1).
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub rules: Vec<Rule>,
    /// Ground atoms the input triples don't state (complex class assertions through
    /// their fresh names, and `owl:Thing` for the ontology's individuals), with where
    /// each came from.
    pub facts: Vec<([Term; 3], Provenance)>,
    pub names: Names,
    pub signature: Signature,
    /// Axioms (by index) U1 doesn't cover, and why. Empty means that U1 contains every
    /// certain answer (if the ontology is consistent).
    pub incomplete: Vec<(usize, &'static str)>,
    /// Clauses (by index into [`nrese_owl::Normalised::clauses`]) whose datatype
    /// conditions U1 doesn't check, and why: U1 has no theory of data ranges, so a
    /// contradiction they hold derives no clash. Their answers are all in U1; only
    /// [`Program::proves_consistency`] depends on this.
    pub unchecked: Vec<(usize, &'static str)>,
    /// U1's own terms, for [`Program::is_internal`].
    internal: HashSet<Term>,
}

/// A program that has no Notation3 form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// A rule or fact names a blank node, which N3 reads as a variable.
    BlankNode { rule: String },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlankNode { rule } => write!(f, "{rule} names a blank node"),
        }
    }
}

impl std::error::Error for RenderError {}

impl Program {
    pub(crate) fn new(names: Names, signature: Signature) -> Self {
        let mut internal: HashSet<Term> = names.fresh.iter().copied().collect();
        internal.insert(names.clash);
        Self {
            names,
            signature,
            internal,
            ..Self::default()
        }
    }

    pub(crate) fn add_internal(&mut self, id: Term) {
        self.internal.insert(id);
    }

    pub(crate) fn add_projection(&mut self, id: Term) {
        self.names.projections.push(id);
        self.internal.insert(id);
    }

    pub(crate) fn add_skolem(&mut self, id: Term) {
        self.names.skolems.push(id);
        self.internal.insert(id);
    }

    /// Whether `term` is one of U1's own (a fresh class, a Skolem constant, a projection,
    /// the clash predicate): facts over it are U1's machinery, never answers.
    pub fn is_internal(&self, term: Term) -> bool {
        self.internal.contains(&term)
    }

    /// Whether a clash-free U1 proves the ontology consistent (PAGOdA, Theorem 5.5 (i)):
    /// U1 covers every axiom and checks every clause's `⊥`.
    pub fn proves_consistency(&self) -> bool {
        self.incomplete.is_empty() && self.unchecked.is_empty()
    }

    /// The rules of an origin.
    pub fn rules_of(&self, origin: Origin) -> impl Iterator<Item = &Rule> {
        self.rules
            .iter()
            .filter(move |r| r.provenance.origin == origin)
    }

    /// Notation3, the store's user-rule language: one `{ body } => { head } .` per rule,
    /// a comment with its provenance before it, and the facts as triples. `term` gives a
    /// term's N-Triples form (`<iri>`, `"lexical"^^<datatype>`). The reasoner names N3
    /// rules by position, so its equality module doesn't replace the copying rules here:
    /// the closure is the same, only slower.
    pub fn to_n3(&self, term: &dyn Fn(Term) -> String) -> Result<String, RenderError> {
        let mut out = String::from("# U1, the upper bound (PAGOdA's datalog strengthening)\n");
        let blank = |text: &str| text.starts_with("_:");
        let slot = |s: &Slot, rule: &str| -> Result<String, RenderError> {
            match s {
                Slot::Var(v) => Ok(format!("?v{v}")),
                Slot::Const(t) => {
                    let text = term(*t);
                    if blank(&text) {
                        return Err(RenderError::BlankNode {
                            rule: rule.to_owned(),
                        });
                    }
                    Ok(text)
                }
            }
        };
        let atoms = |atoms: &[Atom], rule: &str| -> Result<String, RenderError> {
            let mut parts = Vec::with_capacity(atoms.len());
            for a in atoms {
                let [s, p, o] = &a.0;
                parts.push(format!(
                    "{} {} {}",
                    slot(s, rule)?,
                    slot(p, rule)?,
                    slot(o, rule)?
                ));
            }
            Ok(parts.join(" . "))
        };
        for rule in &self.rules {
            let p = &rule.provenance;
            let _ = writeln!(
                out,
                "# {} {:?} sources {:?} {:?}",
                rule.name, p.origin, p.sources, p.approximations
            );
            let mut body = atoms(&rule.body, &rule.name)?;
            for (a, b) in &rule.distinct {
                let _ = write!(
                    body,
                    " . {} <http://www.w3.org/2000/10/swap/log#notEqualTo> {}",
                    slot(a, &rule.name)?,
                    slot(b, &rule.name)?
                );
            }
            let _ = writeln!(
                out,
                "{{ {body} }} => {{ {} }} .",
                atoms(&rule.head, &rule.name)?
            );
        }
        for (fact, provenance) in &self.facts {
            let rule = format!("fact {:?}", provenance.sources);
            let _ = writeln!(
                out,
                "{} {} {} .",
                slot(&Slot::Const(fact[0]), &rule)?,
                slot(&Slot::Const(fact[1]), &rule)?,
                slot(&Slot::Const(fact[2]), &rule)?
            );
        }
        Ok(out)
    }
}
