//! Splitting a head into a conjunction makes body parts independent that the disjunction
//! tied together. A clause such as `r(x, y₁) ∧ … ∧ r(x, yₙ) → A₁(y₁) ∨ … ∨ Aₙ(yₙ)` (a
//! class defined by `n` existentials, read backwards), split, joins all `n` successors at
//! once: `kⁿ` bindings at an element with `k` successors. Each head atom needs only the
//! body components its variables are in; for the others it only matters that they are
//! satisfiable at `x`, which a projection rule `Cⱼ(x, ȳⱼ) → Pⱼ(x)` derives. The rewriting
//! is exact: the rules derive the same atoms over the ontology's vocabulary.
//!
//! A single part the head doesn't need is cut off the centre's atoms the same way:
//! `Person(x) ∧ takesCourse(x, y) ∧ Course(y) → Student(x)` as `takesCourse(x, y) ∧
//! Course(y) → P(x)` and `Person(x) ∧ P(x) → Student(x)`. The rule reasoner's planner
//! takes the atom with most known positions next, constants and bound variables alike,
//! so from `Person(x)` it may pick `Course(y)`, the smaller relation, and join every
//! person with every course (LUBM(1): 13·10⁶ bindings, U1 thirteen times L's time).

use std::collections::{BTreeMap, HashMap};

use super::program::{Atom, Provenance, Slot};
use super::upper::{Compiler, U1};

/// The body's components, keyed by their atoms, and the projection atom of each.
pub(super) type Projections = HashMap<Vec<Atom>, Atom>;

/// The atoms of `body` over `x` alone (or over constants only), and the components that
/// the other variables connect.
fn components(body: &[Atom], x: Option<Slot>) -> (Vec<Atom>, Vec<Vec<Atom>>) {
    let vars = |a: &Atom| -> Vec<Slot> {
        a.0.iter()
            .copied()
            .filter(|s| matches!(s, Slot::Var(_)) && Some(*s) != x)
            .collect()
    };
    let mut core = Vec::new();
    let mut parts: Vec<(Vec<Slot>, Vec<Atom>)> = Vec::new();
    for atom in body {
        let own = vars(atom);
        if own.is_empty() {
            core.push(*atom);
            continue;
        }
        // Merge every component that shares a variable with this atom.
        let mut merged = (own.clone(), vec![*atom]);
        parts.retain(|(vs, atoms)| {
            if vs.iter().any(|v| own.contains(v)) {
                merged.0.extend(vs.iter().copied());
                merged.1.extend(atoms.iter().copied());
                false
            } else {
                true
            }
        });
        parts.push(merged);
    }
    (core, parts.into_iter().map(|(_, atoms)| atoms).collect())
}

impl Compiler<'_> {
    /// Adds `body → head`, with the body cut where its components are independent of a
    /// head atom (see the module docs). `x` is the clause's centre.
    pub(super) fn emit(
        &mut self,
        name: String,
        body: Vec<Atom>,
        head: Vec<Atom>,
        x: Option<Slot>,
        provenance: Provenance,
        projections: &mut Projections,
    ) {
        let (core, parts) = components(&body, x);
        // One part and nothing beside it: nothing to cut.
        if parts.is_empty() || (parts.len() == 1 && core.is_empty()) {
            self.push(name, body, head, provenance);
            return;
        }
        let part_of = |slot: &Slot| {
            parts
                .iter()
                .position(|p| p.iter().any(|a| a.0.contains(slot)))
        };
        let mut groups: BTreeMap<Vec<usize>, Vec<Atom>> = BTreeMap::new();
        for atom in head {
            let mut touched: Vec<usize> = atom
                .0
                .iter()
                .filter(|s| matches!(s, Slot::Var(_)) && Some(**s) != x)
                .filter_map(part_of)
                .collect();
            touched.sort_unstable();
            touched.dedup();
            groups.entry(touched).or_default().push(atom);
        }
        if groups.keys().all(|touched| touched.len() == parts.len()) {
            // Every head atom needs every part.
            let head = groups.into_values().flatten().collect();
            self.push(name, body, head, provenance);
            return;
        }
        let single = groups.len() == 1;
        for (g, (touched, head)) in groups.into_iter().enumerate() {
            let mut body = core.clone();
            for (j, part) in parts.iter().enumerate() {
                if touched.contains(&j) {
                    body.extend(part.iter().copied());
                } else {
                    body.push(self.projection(part, x, &provenance, projections));
                }
            }
            let name = if single {
                name.clone()
            } else {
                format!("{name}-g{g}")
            };
            self.push(name, body, head, provenance.clone());
        }
    }

    /// The atom that says `part` is satisfiable (at `x`, if it mentions `x`), with its
    /// rule, made once per clause.
    fn projection(
        &mut self,
        part: &[Atom],
        x: Option<Slot>,
        provenance: &Provenance,
        projections: &mut Projections,
    ) -> Atom {
        if let Some(atom) = projections.get(part) {
            return *atom;
        }
        let at = x.filter(|x| part.iter().any(|a| a.0.contains(x)));
        let atom = self.new_projection(part.to_vec(), at, provenance);
        projections.insert(part.to_vec(), atom);
        atom
    }

    /// A new projection class `P` and the rule `atoms → P(at)` (`at` absent: a ground
    /// atom over `P`).
    fn new_projection(
        &mut self,
        atoms: Vec<Atom>,
        at: Option<Slot>,
        provenance: &Provenance,
    ) -> Atom {
        let n = self.program.names.projections.len();
        let term = (self.iri)(&format!("{U1}proj{n}"));
        self.program.add_projection(term);
        let atom = match at {
            Some(x) => self.type_atom(x, term),
            None => Atom([Slot::Const(term); 3]),
        };
        self.push(format!("u1-proj{n}"), atoms, vec![atom], provenance.clone());
        atom
    }

    /// A body short enough for the reasoner (fewer than 256 atoms; a class defined as the
    /// intersection of hundreds of classes reaches it): atoms over one variable (or none)
    /// are folded into projections of at most [`MAX_BODY`] atoms each, exactly.
    pub(super) fn fold(&mut self, body: Vec<Atom>, provenance: &Provenance) -> Vec<Atom> {
        if body.len() <= MAX_BODY {
            return body;
        }
        let before = body.len();
        let mut groups: BTreeMap<Option<Slot>, Vec<Atom>> = BTreeMap::new();
        let mut out = Vec::new();
        for atom in body {
            let mut vars: Vec<Slot> = atom
                .0
                .iter()
                .copied()
                .filter(|s| matches!(s, Slot::Var(_)))
                .collect();
            vars.dedup();
            match vars[..] {
                [] => groups.entry(None).or_default().push(atom),
                [v] => groups.entry(Some(v)).or_default().push(atom),
                _ => out.push(atom),
            }
        }
        for (at, atoms) in groups {
            if atoms.len() < 2 {
                out.extend(atoms);
                continue;
            }
            for chunk in atoms.chunks(MAX_BODY) {
                let atom = self.new_projection(chunk.to_vec(), at, provenance);
                out.push(atom);
            }
        }
        if out.len() > MAX_BODY && out.len() < before {
            return self.fold(out, provenance);
        }
        out
    }
}

/// The most atoms U1 puts in a rule body.
const MAX_BODY: usize = 128;
