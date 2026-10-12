//! U1 compiled from the DL-clauses: PAGOdA's datalog strengthening `str(K)` (Zhou et
//! al., JAIR 2015, §5.1, Definition 5.1), over `nrese-owl`'s clauses instead of
//! PAGOdA's normalised rules (its Table 1):
//!
//! - **Disjunctive heads are split** (Definition 5.1): every disjunct is derived, so the
//!   head becomes a conjunction. By Lemma 5.4, every atom of every ground clause that
//!   hyperresolution derives from the ontology is then in U1's closure.
//! - **Existentials are c-Skolemised** (Definition 4.7): `≥ n R.B` at `x` derives
//!   `R(x, cᵢ)` and `B(cᵢ)` for `n` constants unique to the clause and head atom. They
//!   are pairwise different, and outside `B` where the filler is a complement `¬B`, by
//!   `⊥` rules (as §2.2's footnote 4 encodes `≥ n`), so that a clash-free U1 is a model.
//! - **Large `n` is collapsed** to [`Options::max_skolems`] constants `c'ⱼ`, tagged
//!   pairwise different as before. The full encoding is not evaluable for large `n`: an
//!   at-most atom over the same role makes all `n` constants equal, `n²` equalities
//!   (W3C's `WebOnt-description-logic-907` has `n = 60000`: 3.6·10⁹ `owl:sameAs` facts,
//!   128 GB). The collapse keeps Theorem 5.5's two properties:
//!   - *Answers* (5.5 (ii)): `h(cᵢ) = c'ᵢ mod k` (and the tags alike) maps every rule of
//!     the full encoding to one of the collapsed, except the distinctness rule, whose
//!     guard `t₁ ≠ t₂` a homomorphism (§2) needn't keep and which derives only clashes.
//!     So every other fact of the full closure has its image in the collapsed one, and
//!     the image of a fact over named terms is the fact itself.
//!   - *Clashes* (5.5 (i)): the full encoding is symmetric in its `n` constants (with
//!     their tags), so its closure is too: if two of them are equal, all are, and a
//!     distinctness clash in it means `c₀ ≈ c₁` in it, whose image `c'₀ ≈ c'₁` (for
//!     `k ≥ 2`) makes the collapsed distinctness rule fire. Every other clash has its
//!     image. So a clash-free collapsed U1 still proves consistency; it is only no
//!     longer itself a model (`≥ n` has `k` witnesses), which the rule's
//!     [`Approximations::collapsed`] says.
//! - **`⊥` is neutralised** (Definition 5.1): a clause with an empty head derives
//!   `(x clash clash)`, a fact without meaning. If no clash is derived, U1's closure is
//!   a model of the ontology and the ontology is consistent (Theorem 5.5 (i)); in any
//!   case U1 contains every certain answer of a consistent ontology (Theorem 5.5 (ii)).
//! - **At-most restrictions** the clauses keep as atoms (`≤ n R.B` with `n` above the
//!   normalisation's expansion bound) **become equality**: all `B`-successors are made
//!   equal, which is what splitting the expanded form's disjunction of equalities gives.
//! - **Equality** is axiomatised as PAGOdA §2 assumes (EQ2–EQ4). EQ1, reflexivity, is
//!   left out as OWL 2 RL leaves it out: it only adds `x sameAs x`.
//! - **`owl:Thing`** is axiomatised as in §2.2, and binds head variables a clause's body
//!   doesn't (`⊤ ⊑ C` becomes `Thing(x) → C(x)`).
//!
//! Where the clauses and PAGOdA's construction don't meet, the choice made:
//! - The clauses leave role chains and transitivity to automata, which are exact only on
//!   interpretations closed under the role inclusions. U1 adds the chains and
//!   transitivity as rules (PAGOdA's (O9)), so the role atoms they entail are in U1.
//! - Keys aren't clauses. U1 adds them as rules over every term, not only named ones
//!   (more equalities: still an upper bound).
//! - Data ranges have no theory here. Head atoms over them (`DataIn`, data equality,
//!   data at-most) are dropped: no body reads them, so no class, role or data property
//!   atom is lost. A data existential derives its value if that is one literal, else a
//!   Skolem constant. A contradiction among data values (a literal outside a range, two
//!   values of a functional data property, an empty range) then derives no clash: such
//!   clauses are listed in [`Program::unchecked`], and a clash-free U1 proves
//!   consistency only without them ([`Program::proves_consistency`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nrese_owl::{Axiom, Characteristic, ClassExpr, Normalised, ObjProp, Ontology, Term};

use super::program::{
    Approximations, Atom, Names, Origin, Program, Provenance, Rule, Signature, Slot,
};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
/// The namespace of U1's own terms (fresh classes, Skolem constants, the clash).
pub const U1: &str = "urn:nrese:u1:";
/// The most atoms U1 puts in a rule head (the reasoner takes fewer than 256 per rule).
const MAX_HEAD: usize = 128;

/// How U1 is compiled where there is a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The most Skolem constants an at-least atom gets; `≥ n` above it is collapsed to
    /// this many (the module docs). At least 2, which distinctness needs. The closure
    /// grows with its square where an at-most atom equates the constants.
    pub max_skolems: u32,
}

impl Options {
    pub const MIN_SKOLEMS: u32 = 2;
}

impl Default for Options {
    fn default() -> Self {
        Self { max_skolems: 32 }
    }
}

/// U1 for `ontology` and its clauses with the default [`Options`].
pub fn compile(
    ontology: &Ontology,
    normalised: &Normalised,
    iri: &mut dyn FnMut(&str) -> Term,
) -> Program {
    compile_with(ontology, normalised, Options::default(), iri)
}

/// U1 for `ontology` and its clauses (`normalised` must be `ontology`'s). `iri` interns
/// an IRI into the term ids the ontology was read with: the store's dictionary, or a
/// test's table.
pub fn compile_with(
    ontology: &Ontology,
    normalised: &Normalised,
    options: Options,
    iri: &mut dyn FnMut(&str) -> Term,
) -> Program {
    let names = Names {
        rdf_type: iri(&format!("{RDF}type")),
        same_as: iri(&format!("{OWL}sameAs")),
        thing: iri(&format!("{OWL}Thing")),
        clash: iri(&format!("{U1}clash")),
        fresh: (0..normalised.fresh.len())
            .map(|q| iri(&format!("{U1}fresh{q}")))
            .collect(),
        skolems: Vec::new(),
        projections: Vec::new(),
    };
    let named_individual = iri(&format!("{OWL}NamedIndividual"));
    let literal = iri(&format!("{RDFS}Literal"));
    let [top_object, top_data, bottom_object, bottom_data] = [
        "topObjectProperty",
        "topDataProperty",
        "bottomObjectProperty",
        "bottomDataProperty",
    ]
    .map(|local| iri(&format!("{OWL}{local}")));
    let mut c = Compiler {
        program: Program::new(names, Signature::of(ontology, normalised)),
        normalised,
        iri,
        nominals: HashMap::new(),
        max_skolems: options.max_skolems.max(Options::MIN_SKOLEMS),
        literal,
        top_object,
        top_data,
    };
    // Datatype definitions only refine data ranges, which U1 doesn't read; keys are rules
    // here (`axiom`), which the clauses leave to the engines that apply them.
    c.program.incomplete = normalised
        .unsupported
        .iter()
        .filter(|(_, why)| {
            *why != nrese_owl::UNSUPPORTED_DATATYPE_DEFINITIONS
                && *why != nrese_owl::UNSUPPORTED_KEYS
        })
        .copied()
        .collect();
    for (index, clause) in normalised.clauses.iter().enumerate() {
        c.clause(index, clause);
    }
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        c.axiom(index, axiom);
    }
    c.assertions(ontology);
    c.thing(named_individual);
    c.bottom([bottom_object, bottom_data]);
    c.equality();
    c.program.incomplete.sort_unstable();
    c.program.incomplete.dedup();
    c.program
}

/// The compiler's state: the program so far, the clauses' expressions, and the interner.
pub(super) struct Compiler<'a> {
    pub(super) program: Program,
    pub(super) normalised: &'a Normalised,
    pub(super) iri: &'a mut dyn FnMut(&str) -> Term,
    /// The class `{a}` of each individual a rule body names.
    nominals: HashMap<Term, Term>,
    /// [`Options::max_skolems`], at least [`Options::MIN_SKOLEMS`].
    pub(super) max_skolems: u32,
    /// `rdfs:Literal`, the data range that holds every value.
    pub(super) literal: Term,
    /// `owl:topObjectProperty` and `owl:topDataProperty`, which hold for every pair.
    pub(super) top_object: Term,
    pub(super) top_data: Term,
}

impl Compiler<'_> {
    /// The class `{a}`, with the fact `a ∈ {a}`. Rule bodies test it instead of naming
    /// `a`: an engine that does equality by representatives (the store's) rewrites facts
    /// to them, not the constants of rules, so a body naming `a` would miss a fact about
    /// a representative of `a`'s class.
    pub(super) fn nominal(&mut self, a: Term) -> Term {
        if let Some(&class) = self.nominals.get(&a) {
            return class;
        }
        let class = (self.iri)(&format!("{U1}nominal{a}"));
        self.program.add_internal(class);
        self.nominals.insert(a, class);
        let provenance = Provenance {
            origin: Origin::Nominal,
            sources: Vec::new(),
            approximations: Approximations::default(),
        };
        let fact = [a, self.program.names.rdf_type, class];
        self.program.facts.push((fact, provenance));
        class
    }

    pub(super) fn type_atom(&self, s: Slot, class: Term) -> Atom {
        Atom([
            s,
            Slot::Const(self.program.names.rdf_type),
            Slot::Const(class),
        ])
    }

    pub(super) fn edge(role: ObjProp, from: Slot, to: Slot) -> Atom {
        match role {
            ObjProp::Named(p) => Atom([from, Slot::Const(p), to]),
            ObjProp::Inverse(p) => Atom([to, Slot::Const(p), from]),
        }
    }

    pub(super) fn same(&self, a: Slot, b: Slot) -> Atom {
        Atom([a, Slot::Const(self.program.names.same_as), b])
    }

    pub(super) fn clash(&self, at: Slot) -> Atom {
        let clash = Slot::Const(self.program.names.clash);
        Atom([at, clash, clash])
    }

    /// A term of U1's own (a predicate of its machinery), internal.
    fn own(&mut self, iri: &str) -> Term {
        let id = (self.iri)(iri);
        self.program.add_internal(id);
        id
    }

    pub(super) fn skolem(&mut self, iri: &str) -> Term {
        let id = (self.iri)(iri);
        self.program.add_skolem(id);
        id
    }

    pub(super) fn push(
        &mut self,
        name: String,
        body: Vec<Atom>,
        head: Vec<Atom>,
        provenance: Provenance,
    ) {
        let body = self.fold(body, &provenance);
        // A long head (`≥ 300 r.C` makes 600 atoms) as rules of the same body.
        let mut head = head;
        while head.len() > MAX_HEAD {
            let rest = head.split_off(MAX_HEAD);
            self.program.rules.push(Rule {
                name: name.clone(),
                body: body.clone(),
                distinct: Vec::new(),
                head,
                provenance: provenance.clone(),
            });
            head = rest;
        }
        self.program.rules.push(Rule {
            name,
            body,
            distinct: Vec::new(),
            head,
            provenance,
        });
    }

    /// Adds a `⊥` rule whose body needs `distinct` pairs bound to different terms.
    pub(super) fn push_distinct(
        &mut self,
        name: String,
        body: Vec<Atom>,
        distinct: Vec<(Slot, Slot)>,
        head: Vec<Atom>,
        provenance: Provenance,
    ) {
        self.program.rules.push(Rule {
            name,
            body,
            distinct,
            head,
            provenance,
        });
    }

    /// Chains, transitivity and keys: what the clauses don't hold.
    fn axiom(&mut self, index: usize, axiom: &Axiom) {
        let provenance = |origin| Provenance {
            origin,
            sources: vec![vec![index]],
            approximations: Approximations::default(),
        };
        match axiom {
            Axiom::SubObjectPropertyOf(chain, sup) if chain.len() > 1 => {
                let Ok(len) = u8::try_from(chain.len()) else {
                    self.program
                        .incomplete
                        .push((index, "a property chain of more than 255 properties"));
                    return;
                };
                let body: Vec<Atom> = chain
                    .iter()
                    .zip(0..len)
                    .map(|(&r, i)| Self::edge(r, Slot::Var(i), Slot::Var(i + 1)))
                    .collect();
                let head = vec![Self::edge(*sup, Slot::Var(0), Slot::Var(len))];
                let name = format!("u1-chain{index}");
                self.push(name, body, head, provenance(Origin::Chain));
            }
            Axiom::ObjectCharacteristic(Characteristic::Transitive, r) => {
                let p = Slot::Const(r.named());
                let (x, y, z) = (Slot::Var(0), Slot::Var(1), Slot::Var(2));
                let body = vec![Atom([x, p, y]), Atom([y, p, z])];
                let name = format!("u1-trans{index}");
                self.push(name, body, vec![Atom([x, p, z])], provenance(Origin::Chain));
            }
            Axiom::HasKey(class, objects, data) => {
                let (x, y) = (Slot::Var(0), Slot::Var(1));
                let mut body = Vec::new();
                // A complex class is left out of the condition: more equalities, still
                // an upper bound.
                if let ClassExpr::Class(c) = self.normalised.classes.get(class.0) {
                    body.push(self.type_atom(x, *c));
                    body.push(self.type_atom(y, *c));
                }
                let mut next = 2u8;
                for &r in objects {
                    let z = Slot::Var(next);
                    next = next.saturating_add(1);
                    body.push(Self::edge(r, x, z));
                    body.push(Self::edge(r, y, z));
                }
                for &d in data {
                    let z = Slot::Var(next);
                    next = next.saturating_add(1);
                    body.push(Atom([x, Slot::Const(d), z]));
                    body.push(Atom([y, Slot::Const(d), z]));
                }
                if objects.is_empty() && data.is_empty() {
                    return;
                }
                let head = vec![self.same(x, y)];
                self.push(
                    format!("u1-key{index}"),
                    body,
                    head,
                    provenance(Origin::Key),
                );
            }
            _ => {}
        }
    }

    /// The ABox axioms whose meaning the input triples don't state in U1's vocabulary.
    fn assertions(&mut self, ontology: &Ontology) {
        let facts = &self.normalised.facts;
        let bottom = |source: usize| Provenance {
            origin: Origin::Assertion,
            sources: vec![vec![source]],
            approximations: Approximations {
                bottom: true,
                ..Approximations::default()
            },
        };
        for &(concept, a, source) in &facts.concepts {
            let class = self.concept(concept);
            // Only the original named assertion is already in the input RDF. A complex
            // expression may normalise to a named concept whose membership is missing.
            if let Axiom::ClassAssertion(expr, individual) = &ontology.axioms[source]
                && *individual == a
                && matches!(ontology.classes.get(expr.0), ClassExpr::Class(c) if *c == class)
            {
                continue;
            }
            let fact = [a, self.program.names.rdf_type, class];
            self.program.facts.push((fact, assertion(source)));
        }
        // Differences and negative assertions as facts over U1's own predicates, with
        // one `⊥` rule per predicate: linear in the assertions (a rule per pair made
        // 2,429 rules of OWL2Bench's differences), and right under equality by copying
        // and by representatives alike (two equal individuals make `x different x`).
        let clash = self.program.names.clash;
        let different = self.own(&format!("{U1}different"));
        let (x, y) = (Slot::Var(0), Slot::Var(1));
        let mut negated: BTreeMap<Term, Term> = BTreeMap::new();
        for &(a, b, source) in &facts.different {
            if a == b {
                // `a` different from itself: a clash outright.
                self.program.facts.push(([a, clash, clash], bottom(source)));
            } else {
                self.program.facts.push(([a, different, b], bottom(source)));
            }
        }
        let negative = facts.not_roles.iter().map(|&(p, a, b, s)| (p, a, b, s));
        let negative = negative.chain(facts.not_data.iter().map(|&(d, a, v, s)| (d, a, v, s)));
        for (p, a, b, source) in negative.collect::<Vec<_>>() {
            let not = match negated.get(&p) {
                Some(&not) => not,
                None => {
                    let not = self.own(&format!("{U1}not{p}"));
                    negated.insert(p, not);
                    not
                }
            };
            self.program.facts.push(([a, not, b], bottom(source)));
        }
        let builtin = Provenance {
            origin: Origin::Builtin,
            sources: Vec::new(),
            approximations: Approximations::default(),
        };
        if !facts.different.is_empty() {
            let body = vec![Atom([x, Slot::Const(different), x])];
            let head = vec![self.clash(x)];
            self.push("u1-different".to_owned(), body, head, builtin.clone());
        }
        for (p, not) in negated {
            let body = vec![Atom([x, Slot::Const(not), y]), Atom([x, Slot::Const(p), y])];
            let head = vec![self.clash(x)];
            self.push(format!("u1-not{p}"), body, head, builtin.clone());
        }
        // Individuals the rules may not type: owl:Thing outright.
        let mut individuals: BTreeSet<Term> = BTreeSet::new();
        for &(a, b, _) in facts.same.iter().chain(&facts.different) {
            individuals.extend([a, b]);
        }
        for &(_, a, b, _) in facts.not_roles.iter() {
            individuals.extend([a, b]);
        }
        for &(_, a, _, _) in facts.not_data.iter() {
            individuals.insert(a);
        }
        for id in 0..self.normalised.classes.len() as u32 {
            match self.normalised.classes.get(id) {
                ClassExpr::OneOf(xs) => individuals.extend(xs.iter().copied()),
                ClassExpr::HasValue(_, a) => {
                    individuals.insert(*a);
                }
                _ => {}
            }
        }
        let (rdf_type, thing) = (self.program.names.rdf_type, self.program.names.thing);
        for a in individuals {
            let provenance = Provenance {
                origin: Origin::Thing,
                sources: Vec::new(),
                approximations: Approximations::default(),
            };
            self.program.facts.push(([a, rdf_type, thing], provenance));
        }
    }

    /// `owl:Thing` (PAGOdA §2.2): every member of a class, both ends of an object
    /// property, the subject of a data property, and every declared individual.
    fn thing(&mut self, named_individual: Term) {
        let (x, y) = (Slot::Var(0), Slot::Var(1));
        let thing = self.program.names.thing;
        let provenance = Provenance {
            origin: Origin::Thing,
            sources: Vec::new(),
            approximations: Approximations::default(),
        };
        let sig = self.program.signature.clone();
        let classes = sig
            .classes
            .iter()
            .chain(&self.program.names.fresh.clone())
            .copied()
            .chain([named_individual])
            .filter(|&c| c != thing)
            .collect::<Vec<_>>();
        for c in classes {
            let body = vec![self.type_atom(x, c)];
            let head = vec![self.type_atom(x, thing)];
            self.push(format!("u1-thing-c{c}"), body, head, provenance.clone());
        }
        for &p in &sig.object_properties {
            let body = vec![Atom([x, Slot::Const(p), y])];
            let head = vec![self.type_atom(x, thing), self.type_atom(y, thing)];
            self.push(format!("u1-thing-p{p}"), body, head, provenance.clone());
        }
        for &d in &sig.data_properties {
            let body = vec![Atom([x, Slot::Const(d), y])];
            let head = vec![self.type_atom(x, thing)];
            self.push(format!("u1-thing-d{d}"), body, head, provenance.clone());
        }
    }

    /// `owl:bottomObjectProperty` and `owl:bottomDataProperty` hold for no pair (Direct
    /// Semantics, §2.3): a fact over one is a clash. Only where the ontology names them.
    fn bottom(&mut self, properties: [Term; 2]) {
        let sig = &self.program.signature;
        let named: Vec<Term> = properties
            .into_iter()
            .filter(|p| sig.object_properties.contains(p) || sig.data_properties.contains(p))
            .collect();
        for p in named {
            let (x, y) = (Slot::Var(0), Slot::Var(1));
            let provenance = Provenance {
                origin: Origin::Builtin,
                sources: Vec::new(),
                approximations: Approximations::default(),
            };
            let body = vec![Atom([x, Slot::Const(p), y])];
            let head = vec![self.clash(x)];
            self.push(format!("u1-bottom{p}"), body, head, provenance);
        }
    }

    /// Equality (PAGOdA §2, EQ2–EQ4) by OWL 2 RL's rule names, so that the reasoner's
    /// equality module takes the copying rules' place.
    fn equality(&mut self) {
        let v = Slot::Var;
        let same = |a, b| Atom([a, Slot::Const(self.program.names.same_as), b]);
        let rules = [
            ("eq-sym", vec![same(v(0), v(1))], same(v(1), v(0))),
            (
                "eq-trans",
                vec![same(v(0), v(1)), same(v(1), v(2))],
                same(v(0), v(2)),
            ),
            (
                "eq-rep-s",
                vec![same(v(0), v(1)), Atom([v(0), v(2), v(3)])],
                Atom([v(1), v(2), v(3)]),
            ),
            (
                "eq-rep-p",
                vec![same(v(0), v(1)), Atom([v(2), v(0), v(3)])],
                Atom([v(2), v(1), v(3)]),
            ),
            (
                "eq-rep-o",
                vec![same(v(0), v(1)), Atom([v(2), v(3), v(0)])],
                Atom([v(2), v(3), v(1)]),
            ),
        ];
        for (name, body, head) in rules {
            let provenance = Provenance {
                origin: Origin::Equality,
                sources: Vec::new(),
                approximations: Approximations::default(),
            };
            self.push(name.to_owned(), body, vec![head], provenance);
        }
    }
}

fn assertion(source: usize) -> Provenance {
    Provenance {
        origin: Origin::Assertion,
        sources: vec![vec![source]],
        approximations: Approximations::default(),
    }
}
