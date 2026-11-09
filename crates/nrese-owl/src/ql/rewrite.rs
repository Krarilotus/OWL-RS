//! The tree-witness rewriting of a conjunctive query over data closed by the
//! materialisation (docs/design/ql-rewriting.md §2): one branch per set of tree witnesses
//! with disjoint atoms, each witness's atoms replaced by "its root is a `B`" for the `B`s
//! that generate it, and each class atom given the alternatives only an existential (or an
//! inclusion the materialisation doesn't apply) gives.

use std::collections::{HashMap, HashSet};

use super::tbox::{Basic, Tbox};
use super::witness::{TreeWitness, Work, tree_witnesses};
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
    /// Steps of work to build it (candidate interiors, places a witness search tries,
    /// sets of witnesses): bounds the time the other bounds don't.
    pub work: usize,
    /// Steps of work for all the patterns of one query together ([`Budget`]): a query of
    /// many patterns can't multiply the time `work` bounds.
    pub query_work: usize,
    /// Questions one query may ask the data (whether it has a witness's tree wherever the
    /// witness folds, [`Probe`]); past them witnesses are folded unasked.
    pub checks: usize,
    /// Statements one question may read (the concept's and the tree's, counted before it
    /// runs); past them the witness is folded unasked.
    pub check_rows: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            existential_vars: 16,
            candidates: 4096,
            witnesses: 64,
            branches: 256,
            size: 4096,
            work: 50_000,
            query_work: 200_000,
            checks: 16,
            check_rows: 1_000_000,
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
    /// Terms (classes, or a folded witness's first predicate) whose atom holds of every
    /// individual (`⊤ ⊑ …`) on an answer variable nothing else binds: their answers are
    /// every individual, which a query can't list, so those beyond the closure's may be
    /// missing.
    pub unlisted: Vec<Term>,
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
                Part::Atom(_) | Part::Top(_) => 1,
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
    /// The term is an individual (not a literal): a constant, or a variable another part
    /// of the branch binds. What an atom that holds of every individual (`⊤ ⊑ …`) leaves.
    Top(QTerm),
}

/// What the data may be asked about a tree witness before it is folded: whether every
/// individual of each basic concept it folds to (`alternatives`) has its tree in the data,
/// `atoms` with every root made `root` (a constant root, or a variable no other atom has).
/// If so, folding it adds no answer: a branch with it gives only what the same branch
/// without it gives (the tree is there at the individual its roots stand for).
pub struct Probe<'a> {
    pub atoms: Vec<Atom>,
    pub root: QTerm,
    pub alternatives: &'a [Basic],
}

/// The steps of work one query's rewritings have left together ([`Limits::query_work`]):
/// each pattern takes at most [`Limits::work`] of them.
pub struct Budget(std::cell::Cell<usize>);

impl Budget {
    pub fn new(steps: usize) -> Self {
        Self(std::cell::Cell::new(steps))
    }

    pub fn left(&self) -> usize {
        self.0.get()
    }
}

/// The rewriting of `cq` under `tbox`.
pub fn rewrite(tbox: &Tbox, cq: &Cq, limits: &Limits) -> Outcome {
    rewrite_with(
        tbox,
        cq,
        limits,
        &Budget::new(limits.query_work),
        &mut |_| false,
    )
}

/// [`rewrite`], leaving out the witnesses `realised` says the data has wherever they fold
/// ([`Probe`]): where none adds an answer the query is left as it is, at no cost.
pub fn rewrite_with(
    tbox: &Tbox,
    cq: &Cq,
    limits: &Limits,
    budget: &Budget,
    realised: &mut dyn FnMut(&Probe<'_>) -> bool,
) -> Outcome {
    // This pattern's steps: its own bound, or what the query has left.
    let steps = limits.work.min(budget.left());
    let work = Work::new(steps);
    let out = rewrite_within(tbox, cq, limits, &work, realised);
    budget
        .0
        .set(budget.left().saturating_sub(steps - work.left()));
    match out {
        Outcome::Exceeded("work") if steps < limits.work => Outcome::Exceeded("the query's work"),
        out => out,
    }
}

fn rewrite_within(
    tbox: &Tbox,
    cq: &Cq,
    limits: &Limits,
    work: &Work,
    realised: &mut dyn FnMut(&Probe<'_>) -> bool,
) -> Outcome {
    if tbox.is_empty() {
        return Outcome::Unchanged;
    }
    // Every pass below is paid from `work` before it reads the query (review A-1206): with
    // none left, nothing is read.
    if work.exhausted() {
        return Outcome::Exceeded("work");
    }
    let mut witnesses = match tree_witnesses(tbox, cq, limits.witnesses, limits.candidates, work) {
        Ok(witnesses) => witnesses,
        Err(bound) => return Outcome::Exceeded(bound),
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
            let Ok(because) = implied(tbox, &reduced, witness, work) else {
                return Outcome::Exceeded("work");
            };
            let Some(because) = because else {
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
        if !work.take(reduced.atoms.len()) {
            return Outcome::Exceeded("work");
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
        let next = match tree_witnesses(tbox, &reduced, limits.witnesses, limits.candidates, work) {
            Ok(next) => next,
            Err(bound) => return Outcome::Exceeded(bound),
        };
        witnesses = next;
    }
    let cq: &Cq = &reduced;
    // Each witness's fold once, for every branch it is in; the witnesses the data has
    // wherever they fold go.
    let mut folds: Vec<Vec<Basic>> = Vec::new();
    let mut kept = Vec::new();
    for w in witnesses {
        // Paid before the data is asked: a fold the work cuts short asks nothing.
        let Ok(fold) = tbox.generator_alternatives(&w.generators, work) else {
            return Outcome::Exceeded("work");
        };
        if !work.take(w.roots.len() + w.atoms.len()) {
            return Outcome::Exceeded("work");
        }
        let root = w
            .roots
            .iter()
            .copied()
            .find(|t| matches!(t, QTerm::Const(_)))
            .unwrap_or(QTerm::Var(cq.vars));
        let roots: HashSet<QTerm> = w.roots.iter().copied().collect();
        let probe = Probe {
            atoms: w
                .atoms
                .iter()
                .map(|&i| cq.atoms[i].map(&|t| if roots.contains(&t) { root } else { t }))
                .collect(),
            root,
            alternatives: &fold,
        };
        if !fold.is_empty() && realised(&probe) {
            continue;
        }
        folds.push(fold);
        kept.push(w);
    }
    let witnesses = kept;
    // The existential variables an atom has, in one pass.
    if !work.take(cq.atoms.len() + cq.vars as usize) {
        return Outcome::Exceeded("work");
    }
    let mut used = vec![false; cq.vars as usize];
    for atom in &cq.atoms {
        for v in atom.vars() {
            used[v as usize] = true;
        }
    }
    let existential = (0..cq.vars as usize)
        .filter(|&v| cq.existential[v] && used[v])
        .count();
    if existential > limits.existential_vars {
        return Outcome::Exceeded("existential variables");
    }
    // Each class atom's alternatives (memoised, paid alike: `Tbox::class_alternatives`).
    if !work.take(cq.atoms.len()) {
        return Outcome::Exceeded("work");
    }
    let mut alternatives: HashMap<Term, Option<Vec<Basic>>> = HashMap::new();
    for atom in &cq.atoms {
        if let Atom::Class(_, class) = atom
            && !alternatives.contains_key(class)
        {
            let Ok(found) = tbox.class_alternatives(*class, work) else {
                return Outcome::Exceeded("work");
            };
            alternatives.insert(*class, found);
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
            unlisted: Vec::new(),
        });
    }
    let Some(sets) = independent_sets(&witnesses, limits.branches, work) else {
        return Outcome::Exceeded(if work.exhausted() { "work" } else { "branches" });
    };
    let mut builder = Build {
        cq,
        alternatives: &alternatives,
        next: cq.vars,
        work,
        unlisted: Vec::new(),
    };
    let mut branches = Vec::new();
    let mut size = 0;
    for set in sets {
        let chosen: Vec<(&TreeWitness, &[Basic])> = set
            .iter()
            .map(|&i| (&witnesses[i], folds[i].as_slice()))
            .collect();
        let Ok(branch) = builder.branch(&chosen) else {
            return Outcome::Exceeded("work");
        };
        if let Some(branch) = branch {
            size += branch.size();
            if size > limits.size {
                return Outcome::Exceeded("size");
            }
            if !branches.contains(&branch) {
                branches.push(branch);
            }
        }
    }
    let mut unlisted = builder.unlisted;
    unlisted.sort_unstable();
    unlisted.dedup();
    Outcome::Rewritten(Rewriting {
        branches,
        vars: builder.next,
        witnesses: witnesses.len(),
        unlisted,
    })
}

/// The atom of `cq` that implies `witness`, if one does: the witness has one root, and the
/// atom states the root to be of a class below the left side of an axiom that generates
/// it. A step per atom read and the walks, from `work`; `Err(())` once it runs out.
fn implied(tbox: &Tbox, cq: &Cq, witness: &TreeWitness, work: &Work) -> Result<Option<usize>, ()> {
    let [root] = witness.roots[..] else {
        return Ok(None);
    };
    if !work.take(cq.atoms.len()) {
        return Err(());
    }
    for (i, atom) in cq.atoms.iter().enumerate() {
        if let Atom::Class(t, class) = atom
            && *t == root
            && witness.atoms.binary_search(&i).is_err()
            && tbox.generates(*class, &witness.generators, work)?
        {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// Every set of witnesses whose atoms are disjoint (the empty set first); `None` past
/// `limit`.
fn independent_sets(
    witnesses: &[TreeWitness],
    limit: usize,
    work: &Work,
) -> Option<Vec<Vec<usize>>> {
    fn grow(
        work: &Work,
        witnesses: &[TreeWitness],
        from: usize,
        current: &mut Vec<usize>,
        used: &mut HashSet<usize>,
        out: &mut Vec<Vec<usize>>,
        limit: usize,
    ) -> bool {
        out.push(current.clone());
        // A step for the set, and one for each atom of the witnesses it reads to grow it.
        let read: usize = witnesses[from..].iter().map(|w| w.atoms.len()).sum();
        if out.len() > limit || !work.take(1 + read) {
            return false;
        }
        for i in from..witnesses.len() {
            if witnesses[i].atoms.iter().any(|a| used.contains(a)) {
                continue;
            }
            current.push(i);
            used.extend(witnesses[i].atoms.iter().copied());
            let ok = grow(work, witnesses, i + 1, current, used, out, limit);
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
        work,
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
    /// A branch takes a step per atom and alternative it writes, and per root it merges.
    work: &'a Work,
    /// [`Rewriting::unlisted`].
    unlisted: Vec<Term>,
}

/// An atom that holds of every individual, before the branch says what binds its term:
/// the term, the atom as written (for a class atom), and the term it is reported under.
struct Everyone {
    t: QTerm,
    written: Option<Atom>,
    term: Term,
}

impl Build<'_> {
    fn fresh(&mut self) -> u32 {
        self.next += 1;
        self.next - 1
    }

    /// The branch of the witnesses `chosen`, each with its fold (the basic concepts its root
    /// may be stated to be); `None` if it can't match (roots on two constants, or a witness
    /// no named individual generates); `Err(())` once the work runs out.
    fn branch(&mut self, chosen: &[(&TreeWitness, &[Basic])]) -> Result<Option<Branch>, ()> {
        let cq = self.cq;
        // The roots of each witness are one individual.
        let mut classes: Vec<Vec<QTerm>> = Vec::new();
        for (witness, _) in chosen.iter().filter(|(w, _)| !w.roots.is_empty()) {
            let merged: usize = classes.iter().map(Vec::len).sum();
            if !self.work.take(witness.roots.len() + merged) {
                return Err(());
            }
            let mut class: Vec<QTerm> = witness.roots.clone();
            let mut members: HashSet<QTerm> = class.iter().copied().collect();
            classes.retain(|other| {
                if other.iter().any(|t| members.contains(t)) {
                    class.extend(other.iter().copied());
                    members.extend(other.iter().copied());
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
                return Ok(None);
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
        let mut everyone: Vec<Everyone> = Vec::new();
        let mut written: HashSet<Part> = HashSet::new();
        let table = self.alternatives;
        for (i, atom) in cq.atoms.iter().enumerate() {
            let alternatives = match atom {
                Atom::Class(_, class) => table[class].as_ref(),
                _ => None,
            };
            if !self.work.take(1 + alternatives.map_or(0, Vec::len)) {
                return Err(());
            }
            if covered.contains(&i) {
                continue;
            }
            let atom = atom.map(&map);
            let part = match (&atom, alternatives) {
                (Atom::Class(t, class), Some(alternatives))
                    if alternatives[..] == [Basic::Thing] =>
                {
                    everyone.push(Everyone {
                        t: *t,
                        written: Some(atom.clone()),
                        term: *class,
                    });
                    continue;
                }
                (Atom::Class(t, _), Some(alternatives)) => {
                    let fresh = self.fresh();
                    Part::Any(alternatives.iter().map(|&b| stated(b, *t, fresh)).collect())
                }
                _ => Part::Atom(atom),
            };
            if written.insert(part.clone()) {
                parts.push(part);
            }
        }
        for &(witness, fold) in chosen {
            let root = match witness.roots.first() {
                Some(&root) => map(root),
                None => QTerm::Var(self.fresh()),
            };
            if fold.is_empty() {
                return Ok(None);
            }
            if !self.work.take(fold.len()) {
                return Err(());
            }
            if fold[..] == [Basic::Thing] {
                let term = witness.atoms.first().and_then(|&i| match &cq.atoms[i] {
                    Atom::Class(_, c) => Some(*c),
                    Atom::Role(_, p, _) => Some(*p),
                    Atom::Other(..) => None,
                });
                everyone.push(Everyone {
                    t: root,
                    written: None,
                    term: term.unwrap_or_default(),
                });
                continue;
            }
            let fresh = self.fresh();
            parts.push(Part::Any(
                fold.iter().map(|&b| stated(b, root, fresh)).collect(),
            ));
        }
        // An atom that holds of every individual needs only its term to be one: a
        // constant, or a variable another part binds. An existential variable nothing else
        // has is some individual, and the domain is never empty. An answer variable nothing
        // else binds would be every individual: kept as written, and said.
        // The terms the other parts bind, read once, a step per term (Sol r2 S1).
        let mut bound: HashSet<QTerm> = HashSet::new();
        if !everyone.is_empty() {
            for p in &parts {
                let read: Vec<Vec<QTerm>> = match p {
                    Part::Atom(a) => vec![a.terms()],
                    Part::Any(alternatives) => alternatives.iter().map(Atom::terms).collect(),
                    Part::Top(_) => continue,
                };
                if !self.work.take(read.iter().map(Vec::len).sum()) {
                    return Err(());
                }
                // A union binds a term every alternative has.
                if let Some((first, others)) = read.split_first() {
                    bound.extend(
                        first
                            .iter()
                            .filter(|t| others.iter().all(|terms| terms.contains(t))),
                    );
                }
            }
        }
        let mut tops = Vec::new();
        for e in everyone {
            match e.t {
                QTerm::Const(_) => tops.push(Part::Top(e.t)),
                t if bound.contains(&t) => tops.push(Part::Top(t)),
                // Variables from `cq.vars` on are the branch's own: existential.
                QTerm::Var(v) if cq.existential.get(v as usize).is_none_or(|&e| e) => {}
                QTerm::Var(_) => {
                    self.unlisted.push(e.term);
                    // A witness's root: the branch can't bind it, so it goes.
                    let Some(atom) = e.written else {
                        return Ok(None);
                    };
                    tops.push(Part::Atom(atom));
                }
            }
        }
        for top in tops {
            if written.insert(top.clone()) {
                parts.push(top);
            }
        }
        let mut merged: Vec<(u32, QTerm)> = stands_for
            .into_iter()
            .filter_map(|(t, r)| match t {
                QTerm::Var(v) => Some((v, r)),
                QTerm::Const(_) => None,
            })
            .collect();
        merged.sort();
        Ok(Some(Branch { parts, merged }))
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
