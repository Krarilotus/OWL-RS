//! The tree-witness rewriting of a conjunctive query over data closed by the
//! materialisation (docs/design/ql-rewriting.md §2): one branch per set of tree witnesses
//! with disjoint atoms, each witness's atoms replaced by "its root is a `B`" for the `B`s
//! that generate it, and each class atom given the alternatives only an existential (or an
//! inclusion the materialisation doesn't apply) gives.

use std::collections::{HashMap, HashSet};

use super::tbox::{Basic, Tbox};
use super::witness::{TreeWitness, tree_witnesses};
use super::{Atom, Cq, QTerm};
use crate::model::{ObjProp, Term};

/// Bounds on a rewriting (design §5); past one the query is left as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Existential variables of one query.
    pub existential_vars: usize,
    /// Connected sets of them tried as a witness's interior.
    pub candidates: usize,
    pub witnesses: usize,
    /// Sets of independent tree witnesses: the branches of the union.
    pub branches: usize,
    /// Atoms of the rewriting, alternatives counted.
    pub size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            existential_vars: 16,
            candidates: 4096,
            witnesses: 64,
            branches: 256,
            size: 4096,
        }
    }
}

/// What [`rewrite`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to add: the closure answers the query completely.
    Unchanged,
    Rewritten(Rewriting),
    /// A bound of [`Limits`] was reached (named): the query is left as it is.
    Exceeded(&'static str),
}

/// A union of branches, each a conjunction of [`Part`]s. Variables from `Cq::vars` on are
/// new, each local to the part that has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewriting {
    pub branches: Vec<Branch>,
    pub vars: u32,
    pub witnesses: usize,
}

impl Rewriting {
    /// Atoms, alternatives counted.
    pub fn size(&self) -> usize {
        self.branches.iter().map(Branch::size).sum()
    }
}

/// One conjunction of the union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub parts: Vec<Part>,
    /// Variables this branch makes one with another term (a tree witness's roots map to
    /// one individual): each with the term it stands for. The caller binds the ones it
    /// needs, and replaces them in its [`Atom::Other`]s.
    pub merged: Vec<(u32, QTerm)>,
}

impl Branch {
    pub fn size(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Atom(_) => 1,
                Part::Any(alternatives) => alternatives.len(),
            })
            .sum()
    }
}

/// An atom, or a union of atoms (alternatives over one term).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Part {
    Atom(Atom),
    Any(Vec<Atom>),
}

/// The rewriting of `cq` under `tbox`.
pub fn rewrite(tbox: &Tbox, cq: &Cq, limits: &Limits) -> Outcome {
    if tbox.is_empty() {
        return Outcome::Unchanged;
    }
    let Some(mut witnesses) = tree_witnesses(tbox, cq, limits.witnesses, limits.candidates) else {
        return Outcome::Exceeded("tree witnesses");
    };
    // A witness the rest of the query implies goes, with its atoms: its root is stated to
    // be a class that generates it, so every model has the tree it maps into. The query
    // keeps its certain answers (Ontop's CQ subsumption, ISWC 2013 §2.1); a star of arms
    // on a class that generates them all becomes the class atom.
    let original = cq;
    let mut reduced = std::borrow::Cow::Borrowed(cq);
    loop {
        // Every implied witness at once, as long as their atoms and the atoms implying
        // them are apart; then the witnesses of what is left.
        let mut gone: HashSet<usize> = HashSet::new();
        let mut kept: HashSet<usize> = HashSet::new();
        for witness in &witnesses {
            let Some(because) = implied(tbox, &reduced, witness) else {
                continue;
            };
            if witness
                .atoms
                .iter()
                .all(|a| !gone.contains(a) && !kept.contains(a))
                && !gone.contains(&because)
            {
                gone.extend(witness.atoms.iter().copied());
                kept.insert(because);
            }
        }
        if gone.is_empty() {
            break;
        }
        let atoms = reduced
            .atoms
            .iter()
            .enumerate()
            .filter(|(i, _)| !gone.contains(i))
            .map(|(_, a)| a.clone())
            .collect();
        reduced = std::borrow::Cow::Owned(Cq {
            atoms,
            ..reduced.into_owned()
        });
        let Some(next) = tree_witnesses(tbox, &reduced, limits.witnesses, limits.candidates) else {
            return Outcome::Exceeded("tree witnesses");
        };
        witnesses = next;
    }
    let cq: &Cq = &reduced;
    let existential = (0..cq.vars)
        .filter(|&v| cq.existential[v as usize] && cq.atoms.iter().any(|a| a.has_var(v)))
        .count();
    if existential > limits.existential_vars {
        return Outcome::Exceeded("existential variables");
    }
    let mut alternatives: HashMap<Term, Option<Vec<Basic>>> = HashMap::new();
    for atom in &cq.atoms {
        if let Atom::Class(_, class) = atom {
            alternatives
                .entry(*class)
                .or_insert_with(|| tbox.class_alternatives(*class));
        }
    }
    if witnesses.is_empty() && alternatives.values().all(Option::is_none) {
        if cq.atoms.len() == original.atoms.len() {
            return Outcome::Unchanged;
        }
        return Outcome::Rewritten(Rewriting {
            branches: vec![Branch {
                parts: cq.atoms.iter().cloned().map(Part::Atom).collect(),
                merged: Vec::new(),
            }],
            vars: cq.vars,
            witnesses: 0,
        });
    }
    let Some(sets) = independent_sets(&witnesses, limits.branches) else {
        return Outcome::Exceeded("branches");
    };
    let mut builder = Build {
        cq,
        alternatives: &alternatives,
        next: cq.vars,
    };
    // Each witness's fold once, for every branch it is in.
    let folds: Vec<Vec<Basic>> = witnesses
        .iter()
        .map(|w| tbox.generator_alternatives(w.generators.iter().copied()))
        .collect();
    let mut branches = Vec::new();
    let mut size = 0;
    for set in sets {
        let chosen: Vec<(&TreeWitness, &[Basic])> = set
            .iter()
            .map(|&i| (&witnesses[i], folds[i].as_slice()))
            .collect();
        if let Some(branch) = builder.branch(&chosen) {
            size += branch.size();
            if size > limits.size {
                return Outcome::Exceeded("size");
            }
            if !branches.contains(&branch) {
                branches.push(branch);
            }
        }
    }
    Outcome::Rewritten(Rewriting {
        branches,
        vars: builder.next,
        witnesses: witnesses.len(),
    })
}

/// The atom of `cq` that implies `witness`, if one does: the witness has one root, and the
/// atom states the root to be of a class below the left side of an axiom that generates
/// it.
fn implied(tbox: &Tbox, cq: &Cq, witness: &TreeWitness) -> Option<usize> {
    let [root] = witness.roots[..] else {
        return None;
    };
    cq.atoms.iter().enumerate().position(|(i, atom)| {
        matches!(atom, Atom::Class(t, class) if *t == root
            && !witness.atoms.contains(&i)
            && tbox.generates(*class, &witness.generators))
    })
}

/// Every set of witnesses whose atoms are disjoint (the empty set first); `None` past
/// `limit`.
fn independent_sets(witnesses: &[TreeWitness], limit: usize) -> Option<Vec<Vec<usize>>> {
    fn grow(
        witnesses: &[TreeWitness],
        from: usize,
        current: &mut Vec<usize>,
        used: &mut HashSet<usize>,
        out: &mut Vec<Vec<usize>>,
        limit: usize,
    ) -> bool {
        out.push(current.clone());
        if out.len() > limit {
            return false;
        }
        for i in from..witnesses.len() {
            if witnesses[i].atoms.iter().any(|a| used.contains(a)) {
                continue;
            }
            current.push(i);
            used.extend(witnesses[i].atoms.iter().copied());
            let ok = grow(witnesses, i + 1, current, used, out, limit);
            for a in &witnesses[i].atoms {
                used.remove(a);
            }
            current.pop();
            if !ok {
                return false;
            }
        }
        true
    }
    let mut out = Vec::new();
    grow(
        witnesses,
        0,
        &mut Vec::new(),
        &mut HashSet::new(),
        &mut out,
        limit,
    )
    .then_some(out)
}

struct Build<'a> {
    cq: &'a Cq,
    alternatives: &'a HashMap<Term, Option<Vec<Basic>>>,
    next: u32,
}

impl Build<'_> {
    fn fresh(&mut self) -> u32 {
        self.next += 1;
        self.next - 1
    }

    /// The branch of the witnesses `chosen`, each with its fold (the basic concepts its root
    /// may be stated to be); `None` if it can't match (roots on two constants, or a witness
    /// no named individual generates).
    fn branch(&mut self, chosen: &[(&TreeWitness, &[Basic])]) -> Option<Branch> {
        let cq = self.cq;
        // The roots of each witness are one individual.
        let mut classes: Vec<Vec<QTerm>> = Vec::new();
        for (witness, _) in chosen.iter().filter(|(w, _)| !w.roots.is_empty()) {
            let mut class: Vec<QTerm> = witness.roots.clone();
            classes.retain(|other| {
                if other.iter().any(|t| class.contains(t)) {
                    class.extend(other.iter().copied());
                    false
                } else {
                    true
                }
            });
            class.sort();
            class.dedup();
            classes.push(class);
        }
        let mut stands_for: HashMap<QTerm, QTerm> = HashMap::new();
        for class in &classes {
            let constants: Vec<QTerm> = class
                .iter()
                .copied()
                .filter(|t| matches!(t, QTerm::Const(_)))
                .collect();
            if constants.len() > 1 {
                return None;
            }
            let representative = constants.first().copied().or_else(|| {
                class
                    .iter()
                    .copied()
                    .find(|t| matches!(t, QTerm::Var(v) if !cq.existential[*v as usize]))
            });
            let representative = representative.unwrap_or(class[0]);
            for &t in class {
                if t != representative {
                    stands_for.insert(t, representative);
                }
            }
        }
        let map = |t: QTerm| stands_for.get(&t).copied().unwrap_or(t);
        let covered: HashSet<usize> = chosen
            .iter()
            .flat_map(|(w, _)| w.atoms.iter().copied())
            .collect();
        let mut parts: Vec<Part> = Vec::new();
        for (i, atom) in cq.atoms.iter().enumerate() {
            if covered.contains(&i) {
                continue;
            }
            let atom = atom.map(&map);
            let part = match &atom {
                Atom::Class(t, class) => match &self.alternatives[class] {
                    Some(alternatives) => {
                        let fresh = self.fresh();
                        Part::Any(alternatives.iter().map(|&b| stated(b, *t, fresh)).collect())
                    }
                    None => Part::Atom(atom),
                },
                _ => Part::Atom(atom),
            };
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
        for &(witness, fold) in chosen {
            let root = match witness.roots.first() {
                Some(&root) => map(root),
                None => QTerm::Var(self.fresh()),
            };
            if fold.is_empty() {
                return None;
            }
            let fresh = self.fresh();
            parts.push(Part::Any(
                fold.iter().map(|&b| stated(b, root, fresh)).collect(),
            ));
        }
        let mut merged: Vec<(u32, QTerm)> = stands_for
            .into_iter()
            .filter_map(|(t, r)| match t {
                QTerm::Var(v) => Some((v, r)),
                QTerm::Const(_) => None,
            })
            .collect();
        merged.sort();
        Some(Branch { parts, merged })
    }
}

/// The atom stating that `t` is a `basic` (`fresh` for the other end of an existential).
fn stated(basic: Basic, t: QTerm, fresh: u32) -> Atom {
    match basic {
        Basic::Class(class) => Atom::Class(t, class),
        Basic::Exists(ObjProp::Named(p)) => Atom::Role(t, p, QTerm::Var(fresh)),
        Basic::Exists(ObjProp::Inverse(p)) => Atom::Role(QTerm::Var(fresh), p, t),
        Basic::Thing | Basic::Fresh(_) => unreachable!("only classes and existentials are stated"),
    }
}
