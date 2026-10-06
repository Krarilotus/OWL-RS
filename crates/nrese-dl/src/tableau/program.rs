//! The hypertableau's program: `nrese-owl`'s DL-clauses compiled once into HT-clauses over
//! dense ids (Motik, Shearer and Horrocks, JAIR 2009, Definition 5), with a join plan per
//! body atom (docs/design/owl2-dl.md §6, "compiled clause triggers").
//!
//! How the clauses map onto HT-clauses:
//! - variables: `x` is variable 0, each `y(i)` and `v(i)` the next free one;
//! - data properties are roles whose successors are data values (concrete nodes), data
//!   ranges are concepts of those ([`ConceptName::Range`]), data equalities equalities:
//!   the datatype theory checks the values (`data.rs`);
//! - `Nominal(a, v)` in a body is the nominal guard `O_a(v)` with the assertion `O_a(a)`
//!   (Definition 5); in a head it is the equality `v ≈ a`, resolved to `a`'s node when
//!   the clause fires, which is what the guard construction amounts to;
//! - negative object property assertions over simple properties become the clause
//!   `R(x, y) ∧ O_a(x) ∧ O_b(y) → ⊥`.
//!
//! - negative data property assertions `¬p(a, v)` become `p(x, w) ∧ O_a(x) → ¬{v}(w)`;
//! - keys (DL-safe rules) become [`KeyRule`]s, applied to named individuals (`keys.rs`).
//!
//! What the engine can't take is left out and recorded in [`Program::weakened`]: the
//! axioms the normalisation reports unsupported (but keys and datatype definitions, which
//! the engine reads from the rules and definitions), the axioms the reader couldn't read
//! (its fatal diagnostics), and negative assertions over non-simple properties. Leaving
//! clauses out only weakens the ontology, so "inconsistent" stays a sound answer and
//! "consistent" becomes `unsupported`.

use std::collections::BTreeSet;

use hashbrown::HashMap;

use super::graph::NONE;
use crate::datatypes::Ranges;
use nrese_owl::{
    Axiom, BodyAtom, Characteristic, Clause, Concept, DataRange, Filler as OwlFiller, HeadAtom,
    Normalised, ObjProp, Ontology, RangeId, SafeRule, Term, UNSUPPORTED_DATATYPE_DEFINITIONS,
    UNSUPPORTED_KEYS, Var,
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
    /// A data range, as a concept of data values.
    Range(RangeId),
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
    /// Two data values differ (`DisjointDataProperties`): the datatype theory decides.
    Unequal(u8, u8),
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
    /// The at-most restriction its equalities stand for (an index into
    /// [`Program::annotations`]: the annotation `@x ≤ n R.B` of JAIR 2009, Definition 5),
    /// or `NONE`.
    pub annotation: u32,
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
    /// Per step, the head atoms whose variables it binds last: if one of them holds, the
    /// instance is satisfied and the join stops there (for the at-most clauses, this
    /// cuts every binding of two successors to one node).
    pub heads_after: Vec<Vec<u8>>,
}

/// The initial assertions, over individual indexes.
#[derive(Debug, Clone, Default)]
pub struct Assertions {
    pub concepts: Vec<(ConceptId, u32)>,
    pub roles: Vec<(RoleId, u32, u32)>,
    pub same: Vec<(u32, u32)>,
    pub different: Vec<(u32, u32)>,
    /// `(data property, individual, literal)`, the literal by its index in
    /// [`DataProgram::literals`].
    pub data: Vec<(RoleId, u32, u32)>,
}

/// The datatype part of a program (where it has data clauses or assertions).
#[derive(Debug, Clone, Default)]
pub struct DataProgram {
    pub ranges: Ranges,
    /// Per literal of the data assertions (one node each): the concept of its singleton.
    pub literals: Vec<ConceptId>,
    literal_ids: HashMap<Term, u32>,
    /// The filler of an at-most data restriction, as a range concept: its complement's.
    pub complement: HashMap<ConceptId, ConceptId>,
}

/// A key: named individuals in `class` (any, if `None`) that share a named neighbour by
/// each object property and a value by each data property are equal.
#[derive(Debug, Clone)]
pub struct KeyRule {
    pub class: Option<ConceptId>,
    pub objects: Vec<RoleExpr>,
    pub data: Vec<RoleId>,
    pub source: u32,
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
    /// The at-most restrictions equalities are annotated with (the NI rule's `≤ n R.B`).
    pub annotations: Vec<Number>,
    annotation_ids: HashMap<Number, u32>,
    /// By at-most atom (index into `at_most`): its annotation.
    pub at_most_annotation: Vec<u32>,
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
    /// The datatype part, if the program has data values.
    pub data: Option<Box<DataProgram>>,
    /// By role: whether it is a data property (its successors are data values).
    pub data_roles: Vec<bool>,
    pub keys: Vec<KeyRule>,
    /// By individual: whether it is anonymous (keys don't apply to it).
    pub anonymous: Vec<bool>,
    /// Pairs of concepts no node may have together: `C(x) ∧ D(x) → ⊥` (each pair both
    /// ways; `C` with itself for `C(x) → ⊥`). The ≤-rule skips merges they would refute.
    pub disjoint: hashbrown::HashSet<(ConceptId, ConceptId)>,
}

/// The axioms any program of `normalised` leaves out, and why: those the normalisation
/// reports unsupported (but datatype definitions, the datatype theory's, and keys, which
/// are rules), and those the reader couldn't take as OWL 2 DL. Known before any test: with
/// one, no test can show a class satisfiable or a subsumption absent.
pub fn left_out(ontology: &Ontology, normalised: &Normalised) -> Vec<String> {
    let reasons: BTreeSet<&str> = normalised
        .unsupported
        .iter()
        .map(|(_, r)| *r)
        .filter(|r| *r != UNSUPPORTED_DATATYPE_DEFINITIONS && *r != UNSUPPORTED_KEYS)
        .collect();
    let mut why: Vec<String> = reasons
        .into_iter()
        .map(|reason| format!("axioms left out: {reason}"))
        .collect();
    let fatal = ontology.diagnostics.iter().filter(|d| d.is_fatal()).count();
    if fatal > 0 {
        why.push(format!(
            "{fatal} reader diagnostics (not OWL 2 DL, left out)"
        ));
    }
    why
}

impl Program {
    /// The program of `normalised` (the normalisation of `ontology`).
    pub fn compile(ontology: &Ontology, normalised: &Normalised) -> Self {
        let mut p = Program {
            simple: true,
            ..Program::default()
        };
        p.weakened = left_out(ontology, normalised);
        let facts = &normalised.facts;
        if normalised.clauses.iter().any(|c| c.flags.datatype)
            || !facts.data.is_empty()
            || !facts.not_data.is_empty()
        {
            p.data = Some(Box::new(DataProgram {
                ranges: Ranges::new(ontology, normalised),
                ..DataProgram::default()
            }));
        }
        for (index, clause) in normalised.clauses.iter().enumerate() {
            match p.clause(clause, index as u32) {
                Ok(ht) => p.clauses.push(ht),
                Err(why) => p.weakened.push(format!("clause {index} left out: {why}")),
            }
        }
        p.facts(ontology, normalised);
        for rule in &normalised.rules {
            match p.key(rule) {
                Ok(key) => p.keys.push(key),
                Err(why) => p
                    .weakened
                    .push(format!("a key left out (axiom {}): {why}", rule.source)),
            }
        }
        p.anonymous = p
            .individuals
            .iter()
            .map(|t| ontology.anonymous.contains(t))
            .collect();
        if let Some(data) = &mut p.data {
            data.ranges.finish();
        }
        p.data_roles.resize(p.roles.len(), false);
        p.plans();
        p
    }

    /// A data property's role id.
    fn data_role(&mut self, term: Term) -> RoleId {
        let id = self.role(term);
        if self.data_roles.len() <= id as usize {
            self.data_roles.resize(id as usize + 1, false);
        }
        self.data_roles[id as usize] = true;
        id
    }

    fn data(&mut self) -> Result<&mut DataProgram, String> {
        self.data
            .as_deref_mut()
            .ok_or_else(|| "a data atom without data clauses".to_owned())
    }

    /// The concept of the data range `r`.
    fn range_concept(&mut self, r: RangeId) -> ConceptId {
        self.concept(ConceptName::Range(r))
    }

    /// The filler of a data number restriction: `rdfs:Literal` is any value.
    fn data_filler(&mut self, r: RangeId) -> Result<Filler, String> {
        if self.data()?.ranges.is_literal(r) {
            return Ok(Filler::Top);
        }
        Ok(Filler::Is(self.range_concept(r)))
    }

    /// The node index of a literal of the data assertions.
    fn literal(&mut self, term: Term) -> Result<u32, String> {
        if let Some(&id) = self.data()?.literal_ids.get(&term) {
            return Ok(id);
        }
        let one = self.data()?.ranges.intern(DataRange::OneOf(vec![term]));
        let c = self.range_concept(one);
        let data = self.data()?;
        let id = data.literals.len() as u32;
        data.literals.push(c);
        data.literal_ids.insert(term, id);
        Ok(id)
    }

    /// A key rule from the normalisation's DL-safe rule.
    fn key(&mut self, rule: &SafeRule) -> Result<KeyRule, String> {
        let (x, y) = (Var::X, Var::Y(0));
        if rule.head != [HeadAtom::Equal(x, y)] {
            return Err("a DL-safe rule other than a key's".into());
        }
        let mut class: Option<(Option<Concept>, Option<Concept>)> = None;
        let mut objects: std::collections::BTreeMap<Var, [Option<RoleExpr>; 2]> =
            Default::default();
        let mut data: std::collections::BTreeMap<Var, [Option<Term>; 2]> = Default::default();
        let side = |v: Var| -> Result<usize, String> {
            match v {
                Var::X => Ok(0),
                Var::Y(0) => Ok(1),
                _ => Err("a key atom off x and y".into()),
            }
        };
        for atom in &rule.body {
            match *atom {
                BodyAtom::Concept(c, v) => {
                    let entry = class.get_or_insert((None, None));
                    if side(v)? == 0 {
                        entry.0 = Some(c);
                    } else {
                        entry.1 = Some(c);
                    }
                }
                BodyAtom::Role(r, a, b) => {
                    let (from, to, inverse) = match (a, b) {
                        (Var::Y(i), _) if i > 0 => (b, a, true),
                        _ => (a, b, false),
                    };
                    let role = RoleExpr {
                        role: self.role(r),
                        inverse,
                    };
                    objects.entry(to).or_default()[side(from)?] = Some(role);
                }
                BodyAtom::Data(d, a, v) => {
                    data.entry(v).or_default()[side(a)?] = Some(d);
                }
                BodyAtom::Nominal(..) => return Err("a nominal in a key".into()),
            }
        }
        let class = match class {
            None => None,
            Some((Some(a), Some(b))) if a == b => Some(self.concept(ConceptName::Clause(a))),
            Some(_) => return Err("a key's class differs between x and y".into()),
        };
        let mut key = KeyRule {
            class,
            objects: Vec::new(),
            data: Vec::new(),
            source: rule.source as u32,
        };
        for (_, pair) in objects {
            match pair {
                [Some(a), Some(b)] if a == b => key.objects.push(a),
                _ => return Err("a key's object property differs between x and y".into()),
            }
        }
        for (_, pair) in data {
            match pair {
                [Some(a), Some(b)] if a == b => {
                    let role = self.data_role(a);
                    key.data.push(role);
                }
                _ => return Err("a key's data property differs between x and y".into()),
            }
        }
        Ok(key)
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

    /// The annotation id of `number`.
    fn annotation(&mut self, number: Number) -> u32 {
        if let Some(&id) = self.annotation_ids.get(&number) {
            return id;
        }
        let id = self.annotations.len() as u32;
        self.annotations.push(number);
        self.annotation_ids.insert(number, id);
        id
    }

    /// The at-most restriction a clause's equalities between successors spell out: `≤ n
    /// R.B(x)` as `R(x, y₀) ∧ B(y₀) ∧ … ∧ R(x, yₙ) ∧ B(yₙ) → ⋁ yᵢ ≈ yⱼ`; `NONE` if they
    /// don't have that shape.
    fn clause_annotation(&mut self, body: &[Body], head: &[Head]) -> u32 {
        let mut ys: Vec<u8> = Vec::new();
        for h in head {
            if let Head::Equal(a, b) = *h {
                if a == 0 || b == 0 {
                    return NONE;
                }
                ys.extend([a, b]);
            }
        }
        ys.sort_unstable();
        ys.dedup();
        if ys.len() < 2 {
            return NONE;
        }
        let mut shape: Option<(RoleExpr, Vec<ConceptId>)> = None;
        for &y in &ys {
            let roles: Vec<RoleExpr> = body
                .iter()
                .filter_map(|b| match *b {
                    Body::Role(r, 0, v) if v == y => Some(RoleExpr {
                        role: r,
                        inverse: false,
                    }),
                    Body::Role(r, v, 0) if v == y => Some(RoleExpr {
                        role: r,
                        inverse: true,
                    }),
                    _ => None,
                })
                .collect();
            let [role] = roles[..] else {
                return NONE;
            };
            let mut fillers: Vec<ConceptId> = body
                .iter()
                .filter_map(|b| match *b {
                    Body::Concept(c, v) if v == y => Some(c),
                    _ => None,
                })
                .collect();
            fillers.sort_unstable();
            match &shape {
                None => shape = Some((role, fillers)),
                Some(known) if *known == (role, fillers) => {}
                Some(_) => return NONE,
            }
        }
        let Some((role, fillers)) = shape else {
            return NONE;
        };
        let filler = match fillers[..] {
            [] => Filler::Top,
            [c] => Filler::Is(c),
            _ => return NONE,
        };
        self.annotation(Number {
            n: ys.len() as u32 - 1,
            role,
            filler,
        })
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
                BodyAtom::Data(p, a, b) => {
                    self.data()?;
                    let (a, b) = (var(&mut vars, *a)?, var(&mut vars, *b)?);
                    if a != 0 {
                        return Err("a data edge off x".into());
                    }
                    Body::Role(self.data_role(*p), a, b)
                }
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
                    let id = self.number(true, number);
                    if self.at_most_annotation.len() <= id as usize {
                        let annotation = self.annotation(number);
                        self.at_most_annotation.push(annotation);
                    }
                    Head::AtMost(id, var(&mut vars, *v)?)
                }
                HeadAtom::Equal(a, b) => Head::Equal(var(&mut vars, *a)?, var(&mut vars, *b)?),
                HeadAtom::Nominal(t, v) => {
                    self.nominals = true;
                    let i = self.individual(*t);
                    self.guard(i);
                    Head::Nominal(i, var(&mut vars, *v)?)
                }
                HeadAtom::DataAtLeast {
                    n,
                    property,
                    range,
                    var: v,
                } => {
                    let number = Number {
                        n: *n,
                        role: RoleExpr {
                            role: self.data_role(*property),
                            inverse: false,
                        },
                        filler: self.data_filler(*range)?,
                    };
                    Head::AtLeast(self.number(false, number), var(&mut vars, *v)?)
                }
                HeadAtom::DataAtMost {
                    n,
                    property,
                    range,
                    var: v,
                } => {
                    let filler = self.data_filler(*range)?;
                    if let Filler::Is(c) = filler {
                        let not = self.data()?.ranges.intern(DataRange::Not(*range));
                        let not = self.range_concept(not);
                        self.data()?.complement.insert(c, not);
                    }
                    let number = Number {
                        n: *n,
                        role: RoleExpr {
                            role: self.data_role(*property),
                            inverse: false,
                        },
                        filler,
                    };
                    let id = self.number(true, number);
                    if self.at_most_annotation.len() <= id as usize {
                        let annotation = self.annotation(number);
                        self.at_most_annotation.push(annotation);
                    }
                    Head::AtMost(id, var(&mut vars, *v)?)
                }
                HeadAtom::DataIn(r, v) => {
                    self.data()?;
                    Head::Concept(self.range_concept(*r), var(&mut vars, *v)?)
                }
                HeadAtom::DataEqual(a, b) => Head::Equal(var(&mut vars, *a)?, var(&mut vars, *b)?),
                HeadAtom::DataUnequal(a, b) => {
                    self.data()?;
                    Head::Unequal(var(&mut vars, *a)?, var(&mut vars, *b)?)
                }
                HeadAtom::DataRole(p, a, b) => {
                    let (a, b) = (var(&mut vars, *a)?, var(&mut vars, *b)?);
                    Head::Role(self.data_role(*p), a, b)
                }
            };
            head.push(h);
        }
        if vars.len() != bound {
            return Err("a head variable not in the body".into());
        }
        let annotation = self.clause_annotation(&body, &head);
        Ok(HtClause {
            body,
            head,
            vars: bound as u8,
            source,
            annotation,
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
        for &(p, a, v, _) in &facts.data {
            let (role, a) = (self.data_role(p), self.individual(a));
            match self.literal(v) {
                Ok(l) => self.assertions.data.push((role, a, l)),
                Err(why) => self
                    .weakened
                    .push(format!("a data assertion left out: {why}")),
            }
        }
        for &(p, a, v, source) in &facts.not_data {
            // ¬p(a, v): p(x, w) ∧ O_a(x) → ¬{v}(w).
            let (role, a) = (self.data_role(p), self.individual(a));
            let ga = self.guard(a);
            let Ok(data) = self.data() else { continue };
            let one = data.ranges.intern(DataRange::OneOf(vec![v]));
            let not = data.ranges.intern(DataRange::Not(one));
            let not = self.range_concept(not);
            self.clauses.push(HtClause {
                body: vec![Body::Role(role, 0, 1), Body::Concept(ga, 0)],
                head: vec![Head::Concept(not, 1)],
                vars: 2,
                source: source as u32,
                annotation: NONE,
            });
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
                annotation: NONE,
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
        for clause in &self.clauses {
            if !clause.head.is_empty() {
                continue;
            }
            match clause.body[..] {
                [Body::Concept(a, 0), Body::Concept(b, 0)] => {
                    self.disjoint.insert((a, b));
                    self.disjoint.insert((b, a));
                }
                [Body::Concept(a, 0)] => {
                    self.disjoint.insert((a, a));
                }
                _ => {}
            }
        }
        self.by_concept = vec![Vec::new(); self.concepts.len()];
        self.by_role = vec![Vec::new(); self.roles.len()];
        for (index, clause) in self.clauses.iter().enumerate() {
            if clause.body.is_empty() {
                self.everywhere.push(index as u32);
                continue;
            }
            for (t, atom) in clause.body.iter().enumerate() {
                let steps = plan(clause, t);
                let heads_after = heads_after(clause, t, &steps);
                let plan = Plan {
                    clause: index as u32,
                    trigger: t as u8,
                    steps,
                    heads_after,
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
    if let Some(i) = vars.iter().position(|&w| w == v) {
        return Ok(i as u8);
    }
    if vars.len() >= MAX_VARS {
        return Err(format!("more than {MAX_VARS} variables"));
    }
    vars.push(v);
    Ok((vars.len() - 1) as u8)
}

/// For each step of a plan, the head atoms (other than nominal ones) it binds last.
fn heads_after(clause: &HtClause, trigger: usize, steps: &[Step]) -> Vec<Vec<u8>> {
    let mask = |vs: &[u8]| vs.iter().fold(0u32, |m, &v| m | (1 << v));
    let head_vars = |h: &Head| -> Option<u32> {
        Some(match *h {
            Head::Concept(_, v) | Head::AtLeast(_, v) | Head::AtMost(_, v) => mask(&[v]),
            Head::Role(_, a, b) | Head::Equal(a, b) | Head::Unequal(a, b) => mask(&[a, b]),
            Head::Nominal(..) => return None,
        })
    };
    let mut bound = match clause.body[trigger] {
        Body::Concept(_, v) => mask(&[v]),
        Body::Role(_, a, b) => mask(&[a, b]),
    };
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        let before = bound;
        if let Step::Extend { to, .. } = *step {
            bound |= 1 << to;
        }
        let ready: Vec<u8> = clause
            .head
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                let vars = head_vars(h)?;
                (vars & !bound == 0 && vars & !before != 0).then_some(i as u8)
            })
            .collect();
        out.push(ready);
    }
    out
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
