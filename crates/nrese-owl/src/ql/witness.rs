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

/// Steps a rewriting may take (design §5): each candidate interior, each place a witness
/// search tries, each set of witnesses. Counted, not timed: the same query over the same
/// TBox takes the same steps, so the bound is deterministic.
pub(crate) struct Work(std::cell::Cell<usize>);

impl Work {
    pub(crate) fn new(steps: usize) -> Self {
        Self(std::cell::Cell::new(steps))
    }

    /// Takes a step; `false` once there are none left.
    pub(crate) fn spend(&self) -> bool {
        match self.0.get() {
            0 => false,
            left => {
                self.0.set(left - 1);
                true
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
    let existential: Vec<u32> = (0..cq.vars)
        .filter(|&v| cq.existential[v as usize] && cq.atoms.iter().any(|a| a.has_var(v)))
        .collect();
    if existential.is_empty() || tbox.generators.is_empty() {
        return Ok(Vec::new());
    }
    // Existential variables are neighbours when an atom has both.
    let mut neighbours: HashMap<u32, BTreeSet<u32>> = HashMap::new();
    for atom in &cq.atoms {
        let vars: Vec<u32> = atom
            .vars()
            .into_iter()
            .filter(|&v| cq.existential[v as usize])
            .collect();
        for &a in &vars {
            for &b in &vars {
                if a != b {
                    neighbours.entry(a).or_default().insert(b);
                }
            }
        }
    }
    let subsets = connected_subsets(&existential, &neighbours, candidates).ok_or("candidates")?;
    let search = Search { tbox, cq, work };
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

/// Every connected set of `vars` (ESU: each once, from its least member); `None` past
/// `limit`.
fn connected_subsets(
    vars: &[u32],
    neighbours: &HashMap<u32, BTreeSet<u32>>,
    limit: usize,
) -> Option<Vec<Vec<u32>>> {
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
    ) -> bool {
        out.push(subset.clone());
        if out.len() > limit {
            return false;
        }
        let mut extension = extension;
        while let Some(w) = extension.pop() {
            let near: HashSet<u32> = subset.iter().flat_map(|&s| of(s).iter().copied()).collect();
            let mut next = extension.clone();
            for &u in of(w) {
                if u > start && !subset.contains(&u) && !near.contains(&u) && !next.contains(&u) {
                    next.push(u);
                }
            }
            subset.push(w);
            let ok = extend(subset, next, start, of, out, limit);
            subset.pop();
            if !ok {
                return false;
            }
        }
        true
    }
    for &v in vars {
        let extension: Vec<u32> = of(v).iter().copied().filter(|&u| u > v).collect();
        if !extend(&mut vec![v], extension, v, &of, &mut out, limit) {
            return None;
        }
    }
    for subset in &mut out {
        subset.sort_unstable();
    }
    Some(out)
}

/// An anonymous element: the types along the path from the individual.
type Path = Vec<usize>;

struct Search<'a> {
    tbox: &'a Tbox,
    cq: &'a Cq,
    work: &'a Work,
}

impl Search<'_> {
    /// The tree witness with interior `interior`, if some tree takes it.
    fn witness(&self, interior: Vec<u32>) -> Option<TreeWitness> {
        let inside = |t: &QTerm| matches!(t, QTerm::Var(v) if interior.contains(v));
        let atoms: Vec<usize> = (0..self.cq.atoms.len())
            .filter(|&i| self.cq.atoms[i].terms().iter().any(inside))
            .collect();
        let mut roots: Vec<QTerm> = Vec::new();
        for &i in &atoms {
            let atom = &self.cq.atoms[i];
            if matches!(atom, Atom::Other(..)) {
                return None;
            }
            for term in atom.terms() {
                if !inside(&term) && !roots.contains(&term) {
                    roots.push(term);
                }
            }
        }
        let constants = roots
            .iter()
            .filter(|t| matches!(t, QTerm::Const(_)))
            .count();
        if constants > 1 {
            return None;
        }
        let tbox = self.tbox;
        let types: Vec<usize> = if roots.is_empty() {
            (0..tbox.types.len())
                .filter(|&ty| {
                    interior
                        .iter()
                        .any(|&top| self.maps(&interior, &atoms, &roots, top, ty))
                })
                .collect()
        } else {
            // A variable next to a root, and the role of the edge from the root to it: only
            // the types that edge can reach are tried.
            let (first, role) = atoms
                .iter()
                .find_map(|&i| match self.cq.atoms[i] {
                    Atom::Role(s, p, QTerm::Var(o))
                        if roots.contains(&s) && interior.contains(&o) =>
                    {
                        Some((o, ObjProp::Named(p)))
                    }
                    Atom::Role(QTerm::Var(s), p, o)
                        if roots.contains(&o) && interior.contains(&s) =>
                    {
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
                .filter(|&ty| self.maps(&interior, &atoms, &roots, first, ty))
                .collect()
        };
        if types.is_empty() {
            return None;
        }
        // Without roots, the trees that reach one of these types take it too.
        let types: HashSet<usize> = if roots.is_empty() {
            let mut reached: HashSet<usize> = types.iter().copied().collect();
            loop {
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
    /// of the individual the roots map to; without roots, the top of the image).
    fn maps(
        &self,
        interior: &[u32],
        atoms: &[usize],
        roots: &[QTerm],
        start: u32,
        ty: usize,
    ) -> bool {
        if !self.work.spend() {
            return false;
        }
        let mut assigned: HashMap<u32, Path> = HashMap::new();
        assigned.insert(start, vec![ty]);
        if !self.consistent(start, &assigned, atoms, roots) {
            return false;
        }
        self.extend(interior, atoms, roots, &mut assigned)
    }

    fn extend(
        &self,
        interior: &[u32],
        atoms: &[usize],
        roots: &[QTerm],
        assigned: &mut HashMap<u32, Path>,
    ) -> bool {
        if assigned.len() == interior.len() {
            return true;
        }
        // The next variable: one an atom links to an assigned one (the interior is
        // connected).
        let tbox = self.tbox;
        for &i in atoms {
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
                if self.consistent(to, assigned, atoms, roots)
                    && self.extend(interior, atoms, roots, assigned)
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

    /// Whether the atoms with `var` whose terms are all placed hold.
    fn consistent(
        &self,
        var: u32,
        assigned: &HashMap<u32, Path>,
        atoms: &[usize],
        roots: &[QTerm],
    ) -> bool {
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
            if !atom.has_var(var) {
                continue;
            }
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
