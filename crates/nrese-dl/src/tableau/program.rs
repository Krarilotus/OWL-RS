//! The hypertableau's program: `nrese-owl`'s DL-clauses compiled once into HT-clauses over
//! dense ids (Motik, Shearer and Horrocks, JAIR 2009, Definition 5), with a join plan per
//! body atom (docs/design/owl2-dl.md §6, "compiled clause triggers").
//!
//! How the clauses map onto HT-clauses:
//! - variables: `x` is variable 0, each `y(i)` the next free one; data variables have no
//!   place here (datatypes are package 3.5);
//! - `Nominal(a, v)` in a body is the nominal guard `O_a(v)` with the assertion `O_a(a)`
//!   (Definition 5); in a head it is the equality `v ≈ a`, resolved to `a`'s node when
//!   the clause fires, which is what the guard construction amounts to;
//! - negative object property assertions over simple properties become the clause
//!   `R(x, y) ∧ O_a(x) ∧ O_b(y) → ⊥`.
//!
//! What the engine can't take is left out and recorded in [`Program::weakened`]: data
//! clauses and assertions, the axioms the normalisation reports unsupported, and negative
//! assertions over non-simple properties. Leaving clauses out only weakens the ontology,
//! so "inconsistent" stays a sound answer and "consistent" becomes `unsupported`.

use std::collections::BTreeSet;

use hashbrown::HashMap;
use nrese_owl::{
    Axiom, BodyAtom, Characteristic, Clause, Concept, Filler as OwlFiller, HeadAtom, Normalised,
    ObjProp, Ontology, Term, Var,
};

/// A dense concept id.
pub type ConceptId = u32;
/// A dense role (named object property) id.
pub type RoleId = u32;

/// The most variables a clause may have (`x` and the `y`s).
pub const MAX_VARS: usize = 24;

/// What a concept id stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConceptName {
    Clause(Concept),
    /// The nominal guard `O_a` of the individual with this index.
    Guard(u32),
}

/// The filler of a number restriction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Filler {
    Top,
    Is(ConceptId),
    Not(ConceptId),
}

/// A role expression: a role or its inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoleExpr {
    pub role: RoleId,
    pub inverse: bool,
}

/// `≥ n R.F` or `≤ n R.F`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Number {
    pub n: u32,
    pub role: RoleExpr,
    pub filler: Filler,
}

/// A body atom over variable indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    Concept(ConceptId, u8),
    Role(RoleId, u8, u8),
}

/// A head atom over variable indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head {
    Concept(ConceptId, u8),
    Role(RoleId, u8, u8),
    /// An at-least restriction (by its index in [`Program::at_least`]) on the variable.
    AtLeast(u32, u8),
    /// An at-most restriction (by its index in [`Program::at_most`]).
    AtMost(u32, u8),
    Equal(u8, u8),
    /// The variable is the individual with this index.
    Nominal(u32, u8),
}

/// An HT-clause.
#[derive(Debug, Clone)]
pub struct HtClause {
    pub body: Vec<Body>,
    pub head: Vec<Head>,
    pub vars: u8,
    /// The index of the DL-clause it came from in [`Normalised::clauses`] (its proof).
    pub source: u32,
}

/// One step of a join plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The atom (all of whose variables are bound) holds.
    Check(u8),
    /// Bind `to` along the role atom's edges from the bound `from`; `forward` says
    /// whether `from` is the atom's first argument.
    Extend {
        atom: u8,
        from: u8,
        to: u8,
        forward: bool,
    },
}

/// The join of a clause triggered by one of its body atoms.
#[derive(Debug, Clone)]
pub struct Plan {
    pub clause: u32,
    pub trigger: u8,
    pub steps: Vec<Step>,
}

/// The initial assertions, over individual indexes.
#[derive(Debug, Clone, Default)]
pub struct Assertions {
    pub concepts: Vec<(ConceptId, u32)>,
    pub roles: Vec<(RoleId, u32, u32)>,
    pub same: Vec<(u32, u32)>,
    pub different: Vec<(u32, u32)>,
}

/// The compiled program.
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub concepts: Vec<ConceptName>,
    concept_ids: HashMap<ConceptName, ConceptId>,
    pub roles: Vec<Term>,
    role_ids: HashMap<Term, RoleId>,
    pub individuals: Vec<Term>,
    individual_ids: HashMap<Term, u32>,
    pub at_least: Vec<Number>,
    pub at_most: Vec<Number>,
    numbers: HashMap<(bool, Number), u32>,
    pub clauses: Vec<HtClause>,
    /// Plans by the concept and by the role of their trigger atom.
    pub by_concept: Vec<Vec<Plan>>,
    pub by_role: Vec<Vec<Plan>>,
    /// Clauses with an empty body: they apply to every node.
    pub everywhere: Vec<u32>,
    pub assertions: Assertions,
    /// What was left out, and why.
    pub weakened: Vec<String>,
    /// Whether every clause is simple (Definition 10: no edge towards `x`, no inverse in a
    /// number restriction), so that single blocking is complete.
    pub simple: bool,
    pub nominals: bool,
}

impl Program {
    /// The program of `normalised` (the normalisation of `ontology`).
    pub fn compile(ontology: &Ontology, normalised: &Normalised) -> Self {
        let mut p = Program {
            simple: true,
            ..Program::default()
        };
        let reasons: BTreeSet<&str> = normalised.unsupported.iter().map(|(_, r)| *r).collect();
        for reason in reasons {
            p.weakened.push(format!("axioms left out: {reason}"));
        }
        let mut data_clauses = 0usize;
        for (index, clause) in normalised.clauses.iter().enumerate() {
            if clause.flags.datatype {
                data_clauses += 1;
                continue;
            }
            match p.clause(clause, index as u32) {
                Ok(ht) => p.clauses.push(ht),
                Err(why) => p.weakened.push(format!("clause {index} left out: {why}")),
            }
        }
        if data_clauses > 0 {
            p.weakened
                .push(format!("{data_clauses} data clauses left out (datatypes)"));
        }
        p.facts(ontology, normalised);
        p.plans();
        p
    }

    pub fn concept(&mut self, name: ConceptName) -> ConceptId {
        if let Some(&id) = self.concept_ids.get(&name) {
            return id;
        }
        let id = self.concepts.len() as ConceptId;
        self.concepts.push(name);
        self.concept_ids.insert(name, id);
        id
    }

    /// The id of a concept, if the program has it.
    pub fn find_concept(&self, name: ConceptName) -> Option<ConceptId> {
        self.concept_ids.get(&name).copied()
    }

    fn role(&mut self, term: Term) -> RoleId {
        if let Some(&id) = self.role_ids.get(&term) {
            return id;
        }
        let id = self.roles.len() as RoleId;
        self.roles.push(term);
        self.role_ids.insert(term, id);
        id
    }

    /// The index of an individual (its guard concept is made with it).
    pub fn individual(&mut self, term: Term) -> u32 {
        if let Some(&id) = self.individual_ids.get(&term) {
            return id;
        }
        let id = self.individuals.len() as u32;
        self.individuals.push(term);
        self.individual_ids.insert(term, id);
        id
    }

    pub fn guard(&mut self, individual: u32) -> ConceptId {
        self.concept(ConceptName::Guard(individual))
    }

    fn number(&mut self, at_most: bool, number: Number) -> u32 {
        if let Some(&id) = self.numbers.get(&(at_most, number)) {
            return id;
        }
        let list = if at_most {
            &mut self.at_most
        } else {
            &mut self.at_least
        };
        let id = list.len() as u32;
        list.push(number);
        self.numbers.insert((at_most, number), id);
        id
    }

    fn role_expr(&mut self, role: ObjProp) -> RoleExpr {
        match role {
            ObjProp::Named(t) => RoleExpr {
                role: self.role(t),
                inverse: false,
            },
            ObjProp::Inverse(t) => RoleExpr {
                role: self.role(t),
                inverse: true,
            },
        }
    }

    fn filler(&mut self, filler: OwlFiller) -> Filler {
        match filler {
            OwlFiller::Top => Filler::Top,
            OwlFiller::Is(c) => Filler::Is(self.concept(ConceptName::Clause(c))),
            OwlFiller::Not(c) => Filler::Not(self.concept(ConceptName::Clause(c))),
        }
    }

    /// One DL-clause as an HT-clause.
    fn clause(&mut self, clause: &Clause, source: u32) -> Result<HtClause, String> {
        let mut vars: Vec<Var> = vec![Var::X];
        let mut body = Vec::new();
        for atom in &clause.body {
            body.push(match atom {
                BodyAtom::Concept(c, v) => {
                    Body::Concept(self.concept(ConceptName::Clause(*c)), var(&mut vars, *v)?)
                }
                BodyAtom::Role(r, a, b) => {
                    let (a, b) = (var(&mut vars, *a)?, var(&mut vars, *b)?);
                    if a != 0 && b != 0 {
                        return Err("an edge between two successors".into());
                    }
                    if b == 0 && a != 0 {
                        self.simple = false;
                    }
                    Body::Role(self.role(*r), a, b)
                }
                BodyAtom::Nominal(t, v) => {
                    self.nominals = true;
                    let i = self.individual(*t);
                    Body::Concept(self.guard(i), var(&mut vars, *v)?)
                }
                BodyAtom::Data(..) => return Err("a data atom".into()),
            });
        }
        let bound = vars.len();
        // Every successor must hang off x in the body (HT-clause shape).
        for y in 1..bound as u8 {
            let linked = body
                .iter()
                .any(|b| matches!(b, Body::Role(_, a, b) if (*a == y && *b == 0) || (*a == 0 && *b == y)));
            if !linked {
                return Err("a successor not linked to x".into());
            }
        }
        let mut head = Vec::new();
        for atom in &clause.head {
            let h = match atom {
                HeadAtom::Concept(c, v) => {
                    Head::Concept(self.concept(ConceptName::Clause(*c)), var(&mut vars, *v)?)
                }
                HeadAtom::Role(r, a, b) => {
                    let (a, b) = (var(&mut vars, *a)?, var(&mut vars, *b)?);
                    if b == 0 && a != 0 {
                        self.simple = false;
                    }
                    Head::Role(self.role(*r), a, b)
                }
                HeadAtom::AtLeast {
                    n,
                    role,
                    filler,
                    var: v,
                } => {
                    let number = Number {
                        n: *n,
                        role: self.role_expr(*role),
                        filler: self.filler(*filler),
                    };
                    self.simple &= !number.role.inverse;
                    Head::AtLeast(self.number(false, number), var(&mut vars, *v)?)
                }
                HeadAtom::AtMost {
                    n,
                    role,
                    filler,
                    var: v,
                } => {
                    let number = Number {
                        n: *n,
                        role: self.role_expr(*role),
                        filler: self.filler(*filler),
                    };
                    self.simple &= !number.role.inverse;
                    Head::AtMost(self.number(true, number), var(&mut vars, *v)?)
                }
                HeadAtom::Equal(a, b) => Head::Equal(var(&mut vars, *a)?, var(&mut vars, *b)?),
                HeadAtom::Nominal(t, v) => {
                    self.nominals = true;
                    let i = self.individual(*t);
                    self.guard(i);
                    Head::Nominal(i, var(&mut vars, *v)?)
                }
                _ => return Err("a data atom".into()),
            };
            head.push(h);
        }
        if vars.len() != bound {
            return Err("a head variable not in the body".into());
        }
        Ok(HtClause {
            body,
            head,
            vars: bound as u8,
            source,
        })
    }

    /// The assertions, and the clauses for negative assertions.
    fn facts(&mut self, ontology: &Ontology, normalised: &Normalised) {
        let facts = &normalised.facts;
        for &(c, a, _) in &facts.concepts {
            let (c, a) = (self.concept(ConceptName::Clause(c)), self.individual(a));
            self.assertions.concepts.push((c, a));
        }
        for &(r, a, b, _) in &facts.roles {
            let (r, a, b) = (self.role(r), self.individual(a), self.individual(b));
            self.assertions.roles.push((r, a, b));
        }
        for &(a, b, _) in &facts.same {
            let (a, b) = (self.individual(a), self.individual(b));
            self.assertions.same.push((a, b));
        }
        for &(a, b, _) in &facts.different {
            let (a, b) = (self.individual(a), self.individual(b));
            self.assertions.different.push((a, b));
        }
        if !facts.data.is_empty() || !facts.not_data.is_empty() {
            self.weakened
                .push("data assertions left out (datatypes)".into());
        }
        let non_simple = non_simple(ontology);
        for &(r, a, b, source) in &facts.not_roles {
            if non_simple.contains(&r) {
                self.weakened
                    .push("a negative assertion over a non-simple property left out".into());
                continue;
            }
            let (role, a, b) = (self.role(r), self.individual(a), self.individual(b));
            let (ga, gb) = (self.guard(a), self.guard(b));
            self.clauses.push(HtClause {
                body: vec![
                    Body::Role(role, 0, 1),
                    Body::Concept(ga, 0),
                    Body::Concept(gb, 1),
                ],
                head: Vec::new(),
                vars: 2,
                source: source as u32,
            });
        }
        // Every individual's guard holds at it.
        for i in 0..self.individuals.len() as u32 {
            if let Some(g) = self.find_concept(ConceptName::Guard(i)) {
                self.assertions.concepts.push((g, i));
            }
        }
    }

    /// The join plans, one per body atom.
    fn plans(&mut self) {
        self.by_concept = vec![Vec::new(); self.concepts.len()];
        self.by_role = vec![Vec::new(); self.roles.len()];
        for (index, clause) in self.clauses.iter().enumerate() {
            if clause.body.is_empty() {
                self.everywhere.push(index as u32);
                continue;
            }
            for (t, atom) in clause.body.iter().enumerate() {
                let plan = Plan {
                    clause: index as u32,
                    trigger: t as u8,
                    steps: plan(clause, t),
                };
                match atom {
                    Body::Concept(c, _) => self.by_concept[*c as usize].push(plan),
                    Body::Role(r, _, _) => self.by_role[*r as usize].push(plan),
                }
            }
        }
    }

    /// The program grows concepts after compiling (a class to test): its plan table too.
    pub fn ensure_tables(&mut self) {
        self.by_concept.resize(self.concepts.len(), Vec::new());
        self.by_role.resize(self.roles.len(), Vec::new());
    }
}

/// The index of the clause variable `v`, added if new.
fn var(vars: &mut Vec<Var>, v: Var) -> Result<u8, String> {
    if matches!(v, Var::V(_)) {
        return Err("a data variable".into());
    }
    if let Some(i) = vars.iter().position(|&w| w == v) {
        return Ok(i as u8);
    }
    if vars.len() >= MAX_VARS {
        return Err(format!("more than {MAX_VARS} variables"));
    }
    vars.push(v);
    Ok((vars.len() - 1) as u8)
}

/// The join order for `clause` triggered by its atom `trigger`: checks as soon as their
/// variables are bound, then one edge step at a time out of a bound variable.
fn plan(clause: &HtClause, trigger: usize) -> Vec<Step> {
    let vars = |a: &Body| match *a {
        Body::Concept(_, v) => vec![v],
        Body::Role(_, a, b) => vec![a, b],
    };
    let mut bound: u32 = 0;
    for v in vars(&clause.body[trigger]) {
        bound |= 1 << v;
    }
    let mut done = vec![false; clause.body.len()];
    done[trigger] = true;
    let mut steps = Vec::new();
    loop {
        // Checks first: they only filter.
        let mut progress = false;
        for (i, atom) in clause.body.iter().enumerate() {
            if !done[i] && vars(atom).iter().all(|&v| bound & (1 << v) != 0) {
                steps.push(Step::Check(i as u8));
                done[i] = true;
                progress = true;
            }
        }
        if done.iter().all(|&d| d) {
            return steps;
        }
        if progress {
            continue;
        }
        // An edge out of a bound variable to an unbound one.
        let next = clause
            .body
            .iter()
            .enumerate()
            .find_map(|(i, atom)| match *atom {
                Body::Role(_, a, b) if !done[i] => {
                    let (ba, bb) = (bound & (1 << a) != 0, bound & (1 << b) != 0);
                    if ba && !bb {
                        Some((i, a, b, true))
                    } else if bb && !ba {
                        Some((i, b, a, false))
                    } else {
                        None
                    }
                }
                _ => None,
            });
        let Some((i, from, to, forward)) = next else {
            // Unreachable for HT-clauses (every successor is linked to x).
            return steps;
        };
        steps.push(Step::Extend {
            atom: i as u8,
            from,
            to,
            forward,
        });
        done[i] = true;
        bound |= 1 << to;
    }
}

/// The non-simple object properties of `ontology`: superproperties of chains and
/// transitive properties, up the hierarchy (with inverses read as their property).
pub fn non_simple(ontology: &Ontology) -> BTreeSet<Term> {
    let mut subs: Vec<(Term, Term)> = Vec::new();
    let mut seeds: Vec<Term> = Vec::new();
    for axiom in &ontology.axioms {
        match axiom {
            Axiom::SubObjectPropertyOf(chain, sup) if chain.len() > 1 => seeds.push(sup.named()),
            Axiom::SubObjectPropertyOf(chain, sup) => subs.push((chain[0].named(), sup.named())),
            Axiom::ObjectCharacteristic(Characteristic::Transitive, r) => seeds.push(r.named()),
            Axiom::EquivalentObjectProperties(ps) => {
                for a in ps {
                    for b in ps {
                        subs.push((a.named(), b.named()));
                    }
                }
            }
            Axiom::InverseObjectProperties(a, b) => {
                subs.push((a.named(), b.named()));
                subs.push((b.named(), a.named()));
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    while let Some(r) = seeds.pop() {
        if out.insert(r) {
            seeds.extend(subs.iter().filter(|(s, _)| *s == r).map(|&(_, sup)| sup));
        }
    }
    out
}
