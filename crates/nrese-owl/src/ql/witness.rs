//! Tree witnesses (Kikot, Kontchakov and Zakharyaschev, KR 2012; Ontop, ISWC 2013 §2.1):
//! the parts of a conjunctive query that can map into the anonymous trees the generating
//! axioms grow below an individual.
//!
//! A tree witness is a connected set `tᵢ` of existential variables, the atoms touching it
//! (`q_t`), and their other terms `tᵣ`, such that `q_t` maps into the tree below one
//! individual: `tᵣ` onto the individual, `tᵢ` onto anonymous elements. With `tᵣ` empty
//! the part maps into a tree anywhere. Sets of `tᵢ` connected only through `tᵣ` are left
//! out: each of their components is a tree witness of its own, and the rewriting combines
//! independent ones.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::tbox::Tbox;
use super::{Atom, Cq, QTerm};
use crate::model::ObjProp;

/// A tree witness of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeWitness {
    /// `tᵢ`: the variables mapped to anonymous elements.
    pub interior: Vec<u32>,
    /// `tᵣ`: the terms mapped to the individual (empty: anywhere).
    pub roots: Vec<QTerm>,
    /// `q_t`: indices of the atoms it covers, sorted.
    pub atoms: Vec<usize>,
    /// The generating axioms whose tree takes it: with roots, those that make the
    /// individual's child it maps below; without, every one from which a tree that takes
    /// it is reached.
    pub generators: Vec<usize>,
}

/// Steps a rewriting may take (design §5): each candidate interior and each of its
/// members, each place a witness search tries, each set of witnesses, each atom a pass over
/// the query reads, and each element and edge a walk of the TBox's hierarchy reaches.
/// Taken before the work they pay for (review A-1206: with none left, nothing of the query
/// is read). Counted, not timed: the same query over the same TBox takes the same steps,
/// so the bound is deterministic.
pub(crate) struct Work(std::cell::Cell<usize>);

impl Work {
    pub(crate) fn new(steps: usize) -> Self {
        Self(std::cell::Cell::new(steps))
    }

    /// Takes a step; `false` once there are none left.
    pub(crate) fn spend(&self) -> bool {
        self.take(1)
    }

    /// Takes `steps` steps; `false`, and none left, where there are fewer.
    pub(crate) fn take(&self, steps: usize) -> bool {
        match self.0.get().checked_sub(steps) {
            Some(left) => {
                self.0.set(left);
                true
            }
            None => {
                self.0.set(0);
                false
            }
        }
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.0.get() == 0
    }

    pub(crate) fn left(&self) -> usize {
        self.0.get()
    }
}

/// The query's tree witnesses; `Err` with the bound reached: more than `limit` witnesses,
/// more than `candidates` sets of variables to try, or no `work` left.
pub(crate) fn tree_witnesses(
    tbox: &Tbox,
    cq: &Cq,
    limit: usize,
    candidates: usize,
    work: &Work,
) -> Result<Vec<TreeWitness>, &'static str> {
    // One pass over the atoms, a step each (and each variable of an `Other` atom): the
    // existential variables of the atoms the rewriting reads, with their atoms. An `Other`
    // atom's variables are never in an interior (a witness can't have the atom), so they are
    // no candidates.
    if !work.take(cq.atoms.len()) {
        return Err("work");
    }
    let mut excluded: HashSet<u32> = HashSet::new();
    let mut by_var: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, atom) in cq.atoms.iter().enumerate() {
        match atom {
            Atom::Other(_, vars) => {
                if !work.take(vars.len()) {
                    return Err("work");
                }
                excluded.extend(vars.iter().copied());
            }
            _ => {
                for v in atom.vars() {
                    if cq.existential[v as usize] {
                        let atoms = by_var.entry(v).or_default();
                        if atoms.last() != Some(&i) {
                            atoms.push(i);
                        }
                    }
                }
            }
        }
    }
    by_var.retain(|v, _| !excluded.contains(v));
    let mut existential: Vec<u32> = by_var.keys().copied().collect();
    existential.sort_unstable();
    if existential.is_empty() || tbox.generators.is_empty() {
        return Ok(Vec::new());
    }
    // Existential variables are neighbours when an atom has both.
    let mut neighbours: HashMap<u32, BTreeSet<u32>> = HashMap::new();
    for atom in &cq.atoms {
        if let Atom::Role(QTerm::Var(a), _, QTerm::Var(b)) = *atom
            && a != b
            && by_var.contains_key(&a)
            && by_var.contains_key(&b)
        {
            neighbours.entry(a).or_default().insert(b);
            neighbours.entry(b).or_default().insert(a);
        }
    }
    let subsets = connected_subsets(&existential, &neighbours, candidates, work)?;
    let search = Search {
        tbox,
        cq,
        work,
        by_var: &by_var,
    };
    let mut witnesses = Vec::new();
    for interior in subsets {
        if !work.spend() {
            return Err("work");
        }
        let witness = search.witness(interior);
        // A search cut short decided nothing: the bound, not "no witness".
        if work.exhausted() {
            return Err("work");
        }
        if let Some(witness) = witness {
            witnesses.push(witness);
            if witnesses.len() > limit {
                return Err("tree witnesses");
            }
        }
    }
    Ok(witnesses)
}

/// Every connected set of `vars` (ESU: each once, from its least member); `Err` past
/// `limit` ("candidates") or once `work` runs out ("work"): each set takes a step per
/// member, and growing it a step per neighbour and extension entry it reads.
fn connected_subsets(
    vars: &[u32],
    neighbours: &HashMap<u32, BTreeSet<u32>>,
    limit: usize,
    work: &Work,
) -> Result<Vec<Vec<u32>>, &'static str> {
    let none = BTreeSet::new();
    let of = |v: u32| neighbours.get(&v).unwrap_or(&none);
    let mut out = Vec::new();
    fn extend<'n>(
        subset: &mut Vec<u32>,
        extension: Vec<u32>,
        start: u32,
        of: &dyn Fn(u32) -> &'n BTreeSet<u32>,
        out: &mut Vec<Vec<u32>>,
        limit: usize,
        work: &Work,
    ) -> Result<(), &'static str> {
        if !work.take(subset.len()) {
            return Err("work");
        }
        out.push(subset.clone());
        if out.len() > limit {
            return Err("candidates");
        }
        let members: HashSet<u32> = subset.iter().copied().collect();
        let mut extension = extension;
        while let Some(w) = extension.pop() {
            let read =
                subset.iter().map(|&s| of(s).len()).sum::<usize>() + of(w).len() + extension.len();
            if !work.take(read) {
                return Err("work");
            }
            let near: HashSet<u32> = subset.iter().flat_map(|&s| of(s).iter().copied()).collect();
            let mut next = extension.clone();
            let mut listed: HashSet<u32> = next.iter().copied().collect();
            for &u in of(w) {
                if u > start && !members.contains(&u) && !near.contains(&u) && listed.insert(u) {
                    next.push(u);
                }
            }
            subset.push(w);
            let grown = extend(subset, next, start, of, out, limit, work);
            subset.pop();
            grown?;
        }
        Ok(())
    }
    for &v in vars {
        let extension: Vec<u32> = of(v).iter().copied().filter(|&u| u > v).collect();
        extend(&mut vec![v], extension, v, &of, &mut out, limit, work)?;
    }
    for subset in &mut out {
        subset.sort_unstable();
    }
    Ok(out)
}

/// An anonymous element: the types along the path from the individual.
type Path = Vec<usize>;

struct Search<'a> {
    tbox: &'a Tbox,
    cq: &'a Cq,
    work: &'a Work,
    /// The atoms of each candidate variable.
    by_var: &'a HashMap<u32, Vec<usize>>,
}

impl Search<'_> {
    /// The tree witness with interior `interior`, if some tree takes it (`None` too where
    /// the work ran out: the caller reads that).
    fn witness(&self, interior: Vec<u32>) -> Option<TreeWitness> {
        let inside = |t: &QTerm| matches!(t, QTerm::Var(v) if interior.contains(v));
        let mut atoms: Vec<usize> = interior
            .iter()
            .flat_map(|v| self.by_var.get(v).into_iter().flatten().copied())
            .collect();
        if !self.work.take(atoms.len() * interior.len()) {
            return None;
        }
        atoms.sort_unstable();
        atoms.dedup();
        let mut roots: Vec<QTerm> = Vec::new();
        let mut root_set: HashSet<QTerm> = HashSet::new();
        for &i in &atoms {
            let atom = &self.cq.atoms[i];
            if matches!(atom, Atom::Other(..)) {
                return None;
            }
            for term in atom.terms() {
                if !inside(&term) && root_set.insert(term) {
                    roots.push(term);
                }
            }
        }
        let root_of = |t: &QTerm| root_set.contains(t);
        let constants = roots
            .iter()
            .filter(|t| matches!(t, QTerm::Const(_)))
            .count();
        if constants > 1 {
            return None;
        }
        // The atoms between interior variables: what places the next one.
        let links: Vec<usize> = atoms
            .iter()
            .copied()
            .filter(|&i| {
                matches!(self.cq.atoms[i], Atom::Role(QTerm::Var(a), _, QTerm::Var(b))
                    if interior.contains(&a) && interior.contains(&b))
            })
            .collect();
        let tbox = self.tbox;
        // Each scan of types stops once the work runs out (`maps` then fails at once, which
        // isn't a mismatch).
        let types: Vec<usize> = if roots.is_empty() {
            (0..tbox.types.len())
                .take_while(|_| !self.work.exhausted())
                .filter(|&ty| {
                    interior
                        .iter()
                        .take_while(|_| !self.work.exhausted())
                        .any(|&top| self.maps(&interior, &links, &root_set, top, ty))
                })
                .collect()
        } else {
            // A variable next to a root, and the role of the edge from the root to it: only
            // the types that edge can reach are tried.
            let (first, role) = atoms
                .iter()
                .find_map(|&i| match self.cq.atoms[i] {
                    Atom::Role(s, p, QTerm::Var(o)) if root_of(&s) && interior.contains(&o) => {
                        Some((o, ObjProp::Named(p)))
                    }
                    Atom::Role(QTerm::Var(s), p, o) if root_of(&o) && interior.contains(&s) => {
                        Some((s, ObjProp::Inverse(p)))
                    }
                    _ => None,
                })
                .expect("an atom of q_t has a root");
            tbox.reaching
                .get(&role)
                .into_iter()
                .flatten()
                .copied()
                .take_while(|_| !self.work.exhausted())
                .filter(|&ty| self.maps(&interior, &links, &root_set, first, ty))
                .collect()
        };
        if types.is_empty() || self.work.exhausted() {
            return None;
        }
        // Without roots, the trees that reach one of these types take it too (a step per
        // type read, each round).
        let types: HashSet<usize> = if roots.is_empty() {
            let mut reached: HashSet<usize> = types.iter().copied().collect();
            loop {
                if !self.work.take(tbox.types.len()) {
                    return None;
                }
                let more: Vec<usize> = (0..tbox.types.len())
                    .filter(|ty| !reached.contains(ty))
                    .filter(|&ty| tbox.types[ty].children.iter().any(|c| reached.contains(c)))
                    .collect();
                if more.is_empty() {
                    break reached;
                }
                reached.extend(more);
            }
        } else {
            types.into_iter().collect()
        };
        if !self.work.take(tbox.generators.len()) {
            return None;
        }
        let generators = (0..tbox.generators.len())
            .filter(|&g| types.contains(&tbox.generators[g].ty))
            .collect();
        Some(TreeWitness {
            interior,
            roots,
            atoms,
            generators,
        })
    }

    /// Whether the atoms map into a tree with `start` on an element of type `ty` (a child
    /// of the individual the roots map to; without roots, the top of the image). `links`:
    /// the atoms between interior variables.
    fn maps(
        &self,
        interior: &[u32],
        links: &[usize],
        roots: &HashSet<QTerm>,
        start: u32,
        ty: usize,
    ) -> bool {
        if !self.work.spend() {
            return false;
        }
        let mut assigned: HashMap<u32, Path> = HashMap::new();
        assigned.insert(start, vec![ty]);
        if !self.consistent(start, &assigned, roots) {
            return false;
        }
        self.extend(interior, links, roots, &mut assigned)
    }

    fn extend(
        &self,
        interior: &[u32],
        links: &[usize],
        roots: &HashSet<QTerm>,
        assigned: &mut HashMap<u32, Path>,
    ) -> bool {
        if assigned.len() == interior.len() {
            return true;
        }
        // The next variable: one an atom links to an assigned one (the interior is
        // connected).
        let tbox = self.tbox;
        for &i in links {
            let Atom::Role(s, p, o) = &self.cq.atoms[i] else {
                continue;
            };
            let (from, to, forward) = match (s, o) {
                (QTerm::Var(a), QTerm::Var(b))
                    if assigned.contains_key(a)
                        && interior.contains(b)
                        && !assigned.contains_key(b) =>
                {
                    (*a, *b, true)
                }
                (QTerm::Var(a), QTerm::Var(b))
                    if assigned.contains_key(b)
                        && interior.contains(a)
                        && !assigned.contains_key(a) =>
                {
                    (*b, *a, false)
                }
                _ => continue,
            };
            // `forward`: p(from, to); else p(to, from).
            let (out, back) = if forward {
                (ObjProp::Named(*p), ObjProp::Inverse(*p))
            } else {
                (ObjProp::Inverse(*p), ObjProp::Named(*p))
            };
            let here = assigned[&from].clone();
            let last = *here.last().expect("paths start at a type");
            // A step per child type read, before reading them.
            if !self.work.take(tbox.types[last].children.len()) {
                return false;
            }
            let mut candidates: Vec<Path> = Vec::new();
            // The parent: the edge from it to `here` is `here`'s role; p(here, parent)
            // holds when that role is included in `back`.
            if here.len() >= 2 && tbox.implies(tbox.types[last].role, back) {
                candidates.push(here[..here.len() - 1].to_vec());
            }
            for &child in &tbox.types[last].children {
                if tbox.implies(tbox.types[child].role, out) {
                    let mut path = here.clone();
                    path.push(child);
                    candidates.push(path);
                }
            }
            if !tbox.types[last].data && tbox.is_reflexive(*p) {
                candidates.push(here.clone());
            }
            for path in candidates {
                if !self.work.spend() {
                    return false;
                }
                assigned.insert(to, path);
                if self.consistent(to, assigned, roots)
                    && self.extend(interior, links, roots, assigned)
                {
                    return true;
                }
                assigned.remove(&to);
            }
            // Every way to place `to` failed.
            return false;
        }
        false
    }

    /// Whether the atoms with `var` whose terms are all placed hold (a step per atom read).
    fn consistent(&self, var: u32, assigned: &HashMap<u32, Path>, roots: &HashSet<QTerm>) -> bool {
        let atoms = self.by_var.get(&var).map_or(&[][..], Vec::as_slice);
        if !self.work.take(atoms.len()) {
            return false;
        }
        let tbox = self.tbox;
        // Where a term is: an anonymous element, or the individual (`None`).
        let place = |t: &QTerm| -> Option<Option<&Path>> {
            if roots.contains(t) {
                return Some(None);
            }
            match t {
                QTerm::Var(v) => assigned.get(v).map(Some),
                QTerm::Const(_) => None,
            }
        };
        for &i in atoms {
            let atom = &self.cq.atoms[i];
            let ok = match atom {
                Atom::Class(t, class) => match place(t) {
                    Some(Some(path)) => tbox.type_has_class(*path.last().unwrap(), *class),
                    _ => true,
                },
                Atom::Role(s, p, o) => match (place(s), place(o)) {
                    (Some(s), Some(o)) => self.edge(s, *p, o),
                    _ => true,
                },
                Atom::Other(..) => false,
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Whether `p(s, o)` holds between two places of the tree (`None`: the individual).
    fn edge(&self, s: Option<&Path>, p: crate::model::Term, o: Option<&Path>) -> bool {
        let tbox = self.tbox;
        let role = |path: &Path| tbox.types[*path.last().unwrap()].role;
        match (s, o) {
            (None, None) => false,
            (None, Some(o)) => o.len() == 1 && tbox.implies(role(o), ObjProp::Named(p)),
            (Some(s), None) => s.len() == 1 && tbox.implies(role(s), ObjProp::Inverse(p)),
            (Some(s), Some(o)) => {
                if s == o {
                    !tbox.types[*s.last().unwrap()].data && tbox.is_reflexive(p)
                } else if o.len() == s.len() + 1 && o.starts_with(s) {
                    tbox.implies(role(o), ObjProp::Named(p))
                } else if s.len() == o.len() + 1 && s.starts_with(o) {
                    tbox.implies(role(s), ObjProp::Inverse(p))
                } else {
                    false
                }
            }
        }
    }
}
