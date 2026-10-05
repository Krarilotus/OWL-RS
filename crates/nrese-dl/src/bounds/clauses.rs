//! One DL-clause into U1's rules: the split head as one conjunction, existentials as
//! Skolem constants, `⊥` as a clash, at-most atoms as equality (the module docs of
//! [`super::upper`] give the construction and its sources).

use nrese_owl::{
    BodyAtom, Clause, Concept, DataRange, Filler, HeadAtom, ObjProp, RangeId, Term, Var,
};

use super::program::{Approximations, Atom, Origin, Provenance, Slot};
use super::project::Projections;
use super::upper::{Compiler, U1};

/// A clause's variables as rule variables.
pub(super) struct Bindings {
    slots: Vec<(Var, Slot)>,
    pub(super) next: u8,
}

impl Bindings {
    fn find(&self, var: Var) -> Option<Slot> {
        self.slots.iter().find(|(v, _)| *v == var).map(|&(_, s)| s)
    }

    fn get(&mut self, var: Var) -> Slot {
        if let Some(slot) = self.find(var) {
            return slot;
        }
        let slot = Slot::Var(self.next);
        self.next += 1;
        self.slots.push((var, slot));
        slot
    }
}

/// The axioms a clause came from, as alternative sets (each set gives the clause
/// together): the one place that reads `Clause::sources`.
fn clause_sources(clause: &Clause) -> Vec<Vec<usize>> {
    clause.sources.clone()
}

/// Whether a body atom mentions `var`.
fn mentions(atom: &BodyAtom, var: Var) -> bool {
    match atom {
        BodyAtom::Concept(_, v) | BodyAtom::Nominal(_, v) => *v == var,
        BodyAtom::Role(_, x, y) | BodyAtom::Data(_, x, y) => *x == var || *y == var,
    }
}

/// An at-most atom of a head: its role, filler and centre.
type AtMost = (ObjProp, Filler, Slot);

impl Compiler<'_> {
    fn concept(&self, c: Concept) -> Term {
        match c {
            Concept::Named(t) => t,
            Concept::Fresh(q) => self.program.names.fresh[q as usize],
        }
    }

    /// The body of a clause as atoms; a nominal `x = a` as `x ∈ {a}` ([`Compiler::nominal`]).
    /// With whether a body atom over `owl:topDataProperty` has a value nothing else
    /// binds (whose values can't be enumerated: the clause is not covered).
    fn body(&mut self, clause: &Clause, b: &mut Bindings) -> (Vec<Atom>, bool) {
        let mut out = Vec::new();
        let mut unbound_top = false;
        for atom in &clause.body {
            match atom {
                BodyAtom::Concept(c, v) => {
                    let s = b.get(*v);
                    out.push(self.type_atom(s, self.concept(*c)));
                }
                // `owl:topObjectProperty` holds for every pair (Direct Semantics, §2.3):
                // its atom is that both ends exist.
                BodyAtom::Role(p, x, y) if *p == self.top_object => {
                    let thing = self.program.names.thing;
                    let (x, y) = (b.get(*x), b.get(*y));
                    out.extend([self.type_atom(x, thing), self.type_atom(y, thing)]);
                }
                // `owl:topDataProperty` holds for every element and value: where another
                // atom binds the value, its atom is that the element exists.
                BodyAtom::Data(p, x, v) if *p == self.top_data => {
                    let bound = clause.body.iter().filter(|a| mentions(a, *v)).count() > 1;
                    if !bound {
                        unbound_top = true;
                    }
                    let s = b.get(*x);
                    out.push(self.type_atom(s, self.program.names.thing));
                }
                BodyAtom::Role(p, x, y) | BodyAtom::Data(p, x, y) => {
                    let (x, y) = (b.get(*x), b.get(*y));
                    out.push(Atom([x, Slot::Const(*p), y]));
                }
                BodyAtom::Nominal(a, v) => {
                    let s = b.get(*v);
                    let class = self.nominal(*a);
                    out.push(self.type_atom(s, class));
                }
            }
        }
        (out, unbound_top)
    }

    /// A head variable's slot; one the body doesn't bind ranges over `owl:Thing`.
    fn bind(&self, var: Var, body: &mut Vec<Atom>, b: &mut Bindings) -> Slot {
        if let Some(slot) = b.find(var) {
            return slot;
        }
        let slot = b.get(var);
        body.push(self.type_atom(slot, self.program.names.thing));
        slot
    }

    /// The rules of one clause.
    pub(super) fn clause(&mut self, index: usize, clause: &Clause) {
        let mut b = Bindings {
            slots: Vec::new(),
            next: 0,
        };
        let (mut body, unbound_top) = self.body(clause, &mut b);
        let sources = clause_sources(clause);
        if unbound_top {
            for &axiom in sources.iter().flatten() {
                let why = "the top data property over a value nothing else binds";
                self.program.incomplete.push((axiom, why));
            }
        }
        let mut approx = Approximations {
            split: clause.head.len() > 1,
            ..Approximations::default()
        };
        let mut head = Vec::new();
        let mut at_most: Vec<AtMost> = Vec::new();
        // Why a contradiction among this clause's data values would derive no clash.
        let mut unchecked: Option<&'static str> = None;
        // Head atoms that are false whatever the values (over an empty data range).
        let mut false_atoms = 0;
        for (k, atom) in clause.head.iter().enumerate() {
            match atom {
                HeadAtom::Concept(c, v) => {
                    let s = self.bind(*v, &mut body, &mut b);
                    head.push(self.type_atom(s, self.concept(*c)));
                }
                HeadAtom::Role(p, x, y) | HeadAtom::DataRole(p, x, y) => {
                    let x = self.bind(*x, &mut body, &mut b);
                    let y = self.bind(*y, &mut body, &mut b);
                    head.push(Atom([x, Slot::Const(*p), y]));
                }
                HeadAtom::Nominal(a, v) => {
                    let s = self.bind(*v, &mut body, &mut b);
                    head.push(self.same(s, Slot::Const(*a)));
                }
                HeadAtom::Equal(x, y) => {
                    let x = self.bind(*x, &mut body, &mut b);
                    let y = self.bind(*y, &mut body, &mut b);
                    head.push(self.same(x, y));
                }
                HeadAtom::AtLeast {
                    n,
                    role,
                    filler,
                    var,
                } => {
                    approx.skolem = true;
                    let s = self.bind(*var, &mut body, &mut b);
                    approx.collapsed |=
                        self.at_least((index, k), &sources, (*n, *role, *filler), s, &mut head);
                }
                HeadAtom::AtMost {
                    role, filler, var, ..
                } => {
                    let s = self.bind(*var, &mut body, &mut b);
                    at_most.push((*role, *filler, s));
                }
                HeadAtom::DataAtLeast {
                    n,
                    property,
                    range,
                    var,
                } => {
                    if self.is_empty(*range) {
                        // `≥ n` (n ≥ 1) over no values: a false disjunct.
                        false_atoms += 1;
                        continue;
                    }
                    let s = self.bind(*var, &mut body, &mut b);
                    let (values, skolem) = self.data_values((index, k), *range);
                    approx.skolem |= skolem;
                    approx.split |= values.len() > 1;
                    for value in values {
                        head.push(Atom([s, Slot::Const(*property), Slot::Const(value)]));
                    }
                    if !self.has_values(*n, *range) {
                        unchecked
                            .get_or_insert("a data existential over a range that may be too small");
                    }
                }
                HeadAtom::DataIn(range, _) => {
                    if self.is_empty(*range) {
                        // A value in no values (`≤ 0 d`, `∀ d.⊥`): a false disjunct.
                        false_atoms += 1;
                    } else if !self.holds_all(*range) {
                        approx.data = true;
                        unchecked.get_or_insert("data values tested against a data range");
                    }
                }
                HeadAtom::DataAtMost { .. } => {
                    approx.data = true;
                    unchecked.get_or_insert("a data at-most restriction");
                }
                HeadAtom::DataEqual(..) => {
                    approx.data = true;
                    unchecked.get_or_insert("the equality of data values");
                }
                HeadAtom::DataUnequal(..) => {
                    approx.data = true;
                    unchecked.get_or_insert("the inequality of data values");
                }
            }
        }
        if let Some(why) = unchecked {
            self.program.unchecked.push((index, why));
        }
        if clause.head.len() == false_atoms {
            // `⊥`: an empty head, or only false atoms.
            approx.bottom = true;
            let at = b
                .find(Var::X)
                .unwrap_or(Slot::Const(self.program.names.clash));
            head.push(self.clash(at));
        }
        let provenance = |approximations| Provenance {
            origin: Origin::Clause(index),
            sources: sources.clone(),
            approximations,
        };
        let x = b.find(Var::X);
        let mut projections = Projections::new();
        for (m, (role, filler, centre)) in at_most.into_iter().enumerate() {
            let mut body = body.clone();
            let mut next = b.next;
            let mut successor = |body: &mut Vec<Atom>| {
                let y = Slot::Var(next);
                next += 1;
                if role.named() == self.top_object {
                    let thing = self.program.names.thing;
                    body.extend([self.type_atom(centre, thing), self.type_atom(y, thing)]);
                } else {
                    body.push(Self::edge(role, centre, y));
                }
                if let Filler::Is(c) = filler {
                    body.push(self.type_atom(y, self.concept(c)));
                }
                y
            };
            let (y1, y2) = (successor(&mut body), successor(&mut body));
            let head = vec![self.same(y1, y2)];
            let approx = Approximations {
                at_most: true,
                ..approx
            };
            let name = format!("u1-c{index}-max{m}");
            self.emit(name, body, head, x, provenance(approx), &mut projections);
        }
        if !head.is_empty() {
            let name = format!("u1-c{index}");
            self.emit(name, body, head, x, provenance(approx), &mut projections);
        }
    }

    /// `≥ n role.filler` at `x`: `n` Skolem constants of this clause and head atom (at
    /// most [`Compiler::max_skolems`]), with their edges and filler, and `⊥` rules for
    /// their distinctness and for a complement filler. Whether `n` was collapsed.
    fn at_least(
        &mut self,
        (index, k): (usize, usize),
        sources: &[Vec<usize>],
        (n, role, filler): (u32, ObjProp, Filler),
        x: Slot,
        head: &mut Vec<Atom>,
    ) -> bool {
        let collapsed = n > self.max_skolems;
        let constants: Vec<Term> = (0..n.clamp(1, self.max_skolems))
            .map(|i| self.skolem(&format!("{U1}sk{index}-{k}-{i}")))
            .collect();
        let provenance = Provenance {
            origin: Origin::Skolem,
            sources: sources.to_vec(),
            approximations: Approximations {
                bottom: true,
                collapsed,
                ..Approximations::default()
            },
        };
        for &c in &constants {
            let c = Slot::Const(c);
            head.push(Self::edge(role, x, c));
            if let Filler::Is(concept) = filler {
                head.push(self.type_atom(c, self.concept(concept)));
            }
        }
        let (y, t1, t2) = (Slot::Var(0), Slot::Var(1), Slot::Var(2));
        if let Filler::Not(concept) = filler {
            // The constants are outside the concept: a class of their own, and a clash
            // where an element of it is in the concept.
            let family = self.skolem(&format!("{U1}sk{index}-{k}-not"));
            for &c in &constants {
                head.push(self.type_atom(Slot::Const(c), family));
            }
            let body = vec![
                self.type_atom(y, family),
                self.type_atom(y, self.concept(concept)),
            ];
            let head = vec![self.clash(y)];
            self.push(
                format!("u1-sk{index}-{k}-not"),
                body,
                head,
                provenance.clone(),
            );
        }
        if constants.len() > 1 {
            // Pairwise different: each constant carries a tag of its own, and an element
            // with two tags is two of them made equal. Linear in `n`, and right under
            // equality by copying and by representatives alike (tags are never merged).
            let tag = self.skolem(&format!("{U1}sk{index}-{k}-tag"));
            for (i, &c) in constants.iter().enumerate() {
                let value = self.skolem(&format!("{U1}sk{index}-{k}-tag{i}"));
                head.push(Atom([Slot::Const(c), Slot::Const(tag), Slot::Const(value)]));
            }
            let body = vec![
                Atom([y, Slot::Const(tag), t1]),
                Atom([y, Slot::Const(tag), t2]),
            ];
            let head = vec![self.clash(y)];
            let name = format!("u1-sk{index}-{k}-ne");
            self.push_distinct(name, body, vec![(t1, t2)], head, provenance);
        }
        collapsed
    }

    /// Whether `range` holds every data value (`rdfs:Literal`), so that a value can't
    /// contradict it.
    fn holds_all(&self, range: RangeId) -> bool {
        matches!(self.normalised.ranges.get(range.0), DataRange::Datatype(t) if *t == self.literal)
    }

    /// Whether `range` surely has no value: the complement of `rdfs:Literal`, an empty
    /// enumeration, an intersection with an empty range, a union of empty ranges.
    fn is_empty(&self, range: RangeId) -> bool {
        match self.normalised.ranges.get(range.0) {
            DataRange::Not(r) => self.holds_all(*r),
            DataRange::OneOf(values) => values.is_empty(),
            DataRange::And(rs) => rs.iter().any(|&r| self.is_empty(r)),
            DataRange::Or(rs) => rs.iter().all(|&r| self.is_empty(r)),
            _ => false,
        }
    }

    /// Whether `range` surely has `n` values, so that `≥ n` over it can't be a
    /// contradiction: every datatype has at least one value, `rdfs:Literal` infinitely
    /// many, a one-value enumeration one.
    fn has_values(&self, n: u32, range: RangeId) -> bool {
        match self.normalised.ranges.get(range.0) {
            DataRange::Datatype(t) => n <= 1 || *t == self.literal,
            DataRange::OneOf(values) => n <= 1 && !values.is_empty(),
            _ => false,
        }
    }

    /// The values a data existential derives, and whether they are a Skolem constant:
    /// an enumeration's values (its disjunction split, as Definition 5.1 splits a head),
    /// else one Skolem constant that stands for values the datatypes would fix
    /// ([`super::Bounds`] reads it as any value). One value for `≥ n`: rule bodies only
    /// test that a value exists.
    fn data_values(&mut self, (index, k): (usize, usize), range: RangeId) -> (Vec<Term>, bool) {
        if let DataRange::OneOf(values) = self.normalised.ranges.get(range.0)
            && !values.is_empty()
        {
            return (values.clone(), false);
        }
        (vec![self.skolem(&format!("{U1}sk{index}-{k}-v"))], true)
    }
}
