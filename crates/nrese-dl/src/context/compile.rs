//! From `nrese-owl`'s DL-clauses to the [`Program`] the Horn rules run on, or the reason
//! the Horn stage can't decide the ontology ([`Unsupported`]: never a wrong taxonomy).
//!
//! In order:
//! 1. **Data:** with no clause that requires a data value (an at-least data atom in a
//!    head), every clause with a data atom holds when all data properties are empty, and
//!    the clauses without data atoms don't mention them, so those clauses are dropped
//!    (exact for subsumptions between classes). Otherwise: unsupported.
//! 2. **Assertions:** class assertions become query contexts per individual (after
//!    `SameIndividual`); property assertions are unsupported (they need the nominal stage).
//! 3. **Nominals and equality** (at-most atoms, `≤ n` spelled out) are unsupported.
//! 4. **Renaming** of fresh names to make clauses Horn ([`super::horn`]).
//! 5. **Shapes:** a DL-clause has its concept body atoms on the centre (Bate et al.,
//!    §2.4). A neighbour with body concepts that the head doesn't mention is split off
//!    under a fresh name (`R(x, y) ∧ A(y) ∧ B(x) → C(x)` becomes `R(z, x) ∧ A(x) → T(z)`
//!    and `B(x) ∧ T(x) → C(x)`); a clause whose only body concepts are on one neighbour is
//!    re-centred there (`R(x, y) ∧ A(y) → C(x)` is `R(z, x) ∧ A(x) → C(z)`).
//! 6. **Existentials:** `≥ n R.B` in a head is `R(x, f(x))` and `B(f(x))`, `f` the Skolem
//!    function of `∃R.B` (shared by every axiom with that restriction); without equality
//!    `≥ n` and `≥ 1` have the same models up to copying successors, so `n` is dropped.
//!    A complemented filler `¬B` gets a fresh name `N` with `N(x) ∧ B(x) → ⊥`.

use std::collections::{BTreeSet, HashMap};

use nrese_owl::{
    BodyAtom, Clause, Concept, Filler, HeadAtom, Normalised, ObjProp, Term, Var as OwlVar,
};

use super::abox::Abox;
use super::atoms::{ConceptId, FuncId, MAX_ID, RoleId, TermOrder};
use super::horn;
use super::program::{BodyPat, DlClause, Func, HeadPat, KindPat, Program, TermPat, Var};
pub use super::unsupported::Unsupported;

/// What the compilation did, for the profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompileStats {
    pub input_clauses: usize,
    pub dropped_data: usize,
    pub renamed: usize,
    pub split: usize,
    pub recentred: usize,
}

/// The program, the assertions, and the statistics.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub program: Program,
    pub abox: Abox,
    pub stats: CompileStats,
}

fn axiom_of(clause: &Clause) -> usize {
    clause
        .sources
        .first()
        .and_then(|set| set.first())
        .copied()
        .unwrap_or(0)
}

/// Compiles `n` for the Horn stage; `classes` are the named classes to classify (every
/// class of the signature, also those no clause mentions).
pub fn compile(n: &Normalised, classes: &[Term]) -> Result<Compiled, Unsupported> {
    let mut stats = CompileStats {
        input_clauses: n.clauses.len(),
        ..CompileStats::default()
    };
    let kept = admissible(n, &mut stats)?;
    let flips = horn::renaming(&kept, n.fresh.len()).map_err(|e| Unsupported::NotHorn {
        axiom: e.clause.map(|i| axiom_of(kept[i])),
    })?;
    stats.renamed = flips.iter().filter(|&&f| f).count();
    let mut names: BTreeSet<Term> = classes.iter().copied().collect();
    for clause in &kept {
        for b in &clause.body {
            if let BodyAtom::Concept(Concept::Named(t), _) = b {
                names.insert(*t);
            }
        }
        for h in &clause.head {
            if let HeadAtom::Concept(Concept::Named(t), _) = h {
                names.insert(*t);
            }
            if let HeadAtom::AtLeast {
                filler: Filler::Is(Concept::Named(t)) | Filler::Not(Concept::Named(t)),
                ..
            } = h
            {
                names.insert(*t);
            }
        }
    }
    for (c, _, _) in &n.facts.concepts {
        if let Concept::Named(t) = c {
            names.insert(*t);
        }
    }
    let names: Vec<Term> = names.into_iter().collect();
    let fresh_base = names.len() as u32;
    if names.len() + n.fresh.len() > MAX_ID as usize / 2 {
        return Err(Unsupported::TooLarge);
    }
    let mut c = Compiler {
        index: names
            .iter()
            .enumerate()
            .map(|(i, &t)| (t, i as u32))
            .collect(),
        program: Program {
            order: TermOrder { named: fresh_base },
            names,
            ..Program::default()
        },
        flips,
        fresh_base,
        next: fresh_base + n.fresh.len() as u32,
        roles: HashMap::new(),
        funcs: HashMap::new(),
        negations: HashMap::new(),
        splits: HashMap::new(),
        clauses: HashMap::new(),
        stats,
    };
    for clause in &kept {
        c.clause(clause)?;
    }
    let abox = c.abox(n)?;
    if c.next > MAX_ID || c.program.funcs.len() > (MAX_ID / 2) as usize {
        return Err(Unsupported::TooLarge);
    }
    c.program.concepts = c.next;
    c.program.roles = c.roles.len() as u32;
    c.program.index();
    Ok(Compiled {
        program: c.program,
        abox,
        stats: c.stats,
    })
}

/// The clauses the Horn stage reads, after the data, assertion, nominal and equality
/// checks of steps 1–3.
fn admissible<'n>(
    n: &'n Normalised,
    stats: &mut CompileStats,
) -> Result<Vec<&'n Clause>, Unsupported> {
    let requires_data = n.clauses.iter().find(|c| {
        c.head
            .iter()
            .any(|h| matches!(h, HeadAtom::DataAtLeast { .. }))
    });
    if let Some(clause) = requires_data {
        return Err(Unsupported::Datatypes {
            axiom: axiom_of(clause),
        });
    }
    let facts = &n.facts;
    if !facts.not_roles.is_empty() {
        return Err(Unsupported::Assertions(
            "negative object property assertions",
        ));
    }
    if !facts.data.is_empty() || !facts.not_data.is_empty() {
        return Err(Unsupported::Assertions("data property assertions"));
    }
    for &(axiom, why) in &n.unsupported {
        // Datatype definitions only constrain data values, of which there are none once
        // the data clauses are dropped; keys only apply to individuals with property
        // values (and then make them equal, which needs equality).
        if why.starts_with("datatype definitions")
            || (why.starts_with("keys") && facts.roles.is_empty())
        {
            continue;
        }
        return Err(Unsupported::Normalisation { axiom, why });
    }
    let mut kept = Vec::with_capacity(n.clauses.len());
    for clause in &n.clauses {
        if clause.flags.datatype {
            stats.dropped_data += 1;
            continue;
        }
        if clause.flags.nominal {
            return Err(Unsupported::Nominals {
                axiom: axiom_of(clause),
            });
        }
        if clause.flags.equality {
            return Err(Unsupported::Equality {
                axiom: axiom_of(clause),
            });
        }
        kept.push(clause);
    }
    Ok(kept)
}

pub(super) struct Compiler {
    index: HashMap<Term, ConceptId>,
    pub(super) program: Program,
    flips: Vec<bool>,
    fresh_base: u32,
    /// The next internal concept.
    next: u32,
    roles: HashMap<Term, RoleId>,
    funcs: HashMap<Func, FuncId>,
    negations: HashMap<ConceptId, ConceptId>,
    /// Split-off neighbour parts, by their DL-clause body: the name they define.
    splits: HashMap<Box<[BodyPat]>, ConceptId>,
    clauses: HashMap<(Box<[BodyPat]>, Option<HeadPat>), usize>,
    stats: CompileStats,
}

/// A clause being shaped: body and head over `nrese-owl`'s variables.
struct Work {
    body: Vec<BodyAtom>,
    head: Option<HeadAtom>,
}

impl Compiler {
    /// A concept and whether it is flipped (stands for its complement now).
    pub(super) fn concept(&self, c: Concept) -> (ConceptId, bool) {
        match c {
            Concept::Named(t) => (self.index[&t], false),
            Concept::Fresh(q) => (self.fresh_base + q, self.flips[q as usize]),
        }
    }

    pub(super) fn role(&mut self, r: Term) -> RoleId {
        let next = self.roles.len() as RoleId;
        *self.roles.entry(r).or_insert(next)
    }

    pub(super) fn internal(&mut self) -> ConceptId {
        self.next += 1;
        self.next - 1
    }

    /// A concept `N` with `N(x) ∧ c(x) → ⊥`. It is a definition (`N := ¬c`), true in a
    /// conservative extension of any axiom set, so it has no source axiom.
    pub(super) fn negation(&mut self, c: ConceptId) -> ConceptId {
        if let Some(&n) = self.negations.get(&c) {
            return n;
        }
        let n = self.internal();
        self.negations.insert(c, n);
        self.add(vec![BodyPat::Concept(n), BodyPat::Concept(c)], None, &[]);
        n
    }

    pub(super) fn add(
        &mut self,
        mut body: Vec<BodyPat>,
        head: Option<HeadPat>,
        sources: &[Box<[u32]>],
    ) {
        body.sort_unstable();
        body.dedup();
        // A head that repeats a body atom: a tautology.
        if let Some(h) = head {
            let same = match (h.kind, h.term) {
                (KindPat::Concept, TermPat::Var(Var::X)) => {
                    body.contains(&BodyPat::Concept(h.pred))
                }
                (KindPat::Out, TermPat::Var(v)) => body.contains(&BodyPat::Out(h.pred, v)),
                (KindPat::In, TermPat::Var(Var::Z(i))) => body.contains(&BodyPat::In(h.pred, i)),
                _ => false,
            };
            if same {
                return;
            }
        }
        let key = (body.into_boxed_slice(), head);
        if let Some(&at) = self.clauses.get(&key) {
            let clause = &mut self.program.clauses[at];
            let mut merged: Vec<Box<[u32]>> =
                clause.sources.iter().chain(sources).cloned().collect();
            merged.sort_unstable();
            merged.dedup();
            clause.sources = merged.into_boxed_slice();
            return;
        }
        self.clauses.insert(key.clone(), self.program.clauses.len());
        let mut sources = sources.to_vec();
        sources.sort_unstable();
        sources.dedup();
        self.program.clauses.push(DlClause {
            body: key.0,
            head: key.1,
            sources: sources.into_boxed_slice(),
        });
    }

    /// Steps 4–6 for one clause.
    fn clause(&mut self, clause: &Clause) -> Result<(), Unsupported> {
        let axiom = axiom_of(clause);
        let sources: Vec<Box<[u32]>> = clause
            .sources
            .iter()
            .map(|set| {
                let mut set: Vec<u32> = set.iter().map(|&s| s as u32).collect();
                set.sort_unstable();
                set.dedup();
                set.into_boxed_slice()
            })
            .collect();
        let mut work = Work {
            body: Vec::new(),
            head: None,
        };
        let mut heads = Vec::new();
        // The renaming: a flipped name changes sides.
        for b in &clause.body {
            match b {
                BodyAtom::Concept(c, v) if self.concept(*c).1 => {
                    heads.push(HeadAtom::Concept(*c, *v))
                }
                _ => work.body.push(b.clone()),
            }
        }
        for h in &clause.head {
            match h {
                HeadAtom::Concept(c, v) if self.concept(*c).1 => {
                    work.body.push(BodyAtom::Concept(*c, *v))
                }
                _ => heads.push(h.clone()),
            }
        }
        if heads.len() > 1 {
            return Err(Unsupported::NotHorn { axiom: Some(axiom) });
        }
        work.head = heads.pop();
        self.shape(&mut work, axiom)?;
        self.emit(work, &sources, axiom)
    }

    /// Step 5: concept body atoms onto the centre.
    fn shape(&mut self, work: &mut Work, axiom: usize) -> Result<(), Unsupported> {
        let shape = Unsupported::ClauseShape { axiom };
        let mut with_concepts: BTreeSet<OwlVar> = BTreeSet::new();
        for b in &work.body {
            if let BodyAtom::Concept(_, v) = b
                && *v != OwlVar::X
            {
                with_concepts.insert(*v);
            }
        }
        let in_head: BTreeSet<OwlVar> = match &work.head {
            None => BTreeSet::new(),
            Some(HeadAtom::Concept(_, v)) => [*v].into(),
            Some(HeadAtom::Role(_, a, b)) => [*a, *b].into(),
            Some(HeadAtom::AtLeast { var, .. }) => [*var].into(),
            Some(_) => return Err(shape),
        };
        // One neighbour with body concepts and none on the centre: re-centre there, the
        // EL-calculus form (`∃R.B ⊑ C` as `R(z, x) ∧ B(x) → C(z)`, the paper's DL3).
        if let [y] = with_concepts.iter().copied().collect::<Vec<_>>()[..]
            && !work
                .body
                .iter()
                .any(|b| matches!(b, BodyAtom::Concept(_, OwlVar::X)))
            && !matches!(work.head, Some(HeadAtom::AtLeast { .. }))
        {
            let old = OwlVar::Y(u16::MAX);
            let swap = |v: OwlVar| match v {
                OwlVar::X => old,
                v if v == y => OwlVar::X,
                v => v,
            };
            let body: Vec<BodyAtom> = work.body.iter().map(|b| rename_body(b, &swap)).collect();
            let head = work.head.as_ref().map(|h| rename_head(h, &swap));
            let star = body.iter().all(|b| match b {
                BodyAtom::Role(_, a, c) => *a == OwlVar::X || *c == OwlVar::X,
                _ => true,
            }) && match &head {
                Some(HeadAtom::Role(_, a, c)) => *a == OwlVar::X || *c == OwlVar::X,
                _ => true,
            };
            if star {
                work.body = body;
                work.head = head;
                self.stats.recentred += 1;
                return Ok(());
            }
        }
        let mut remaining = Vec::new();
        for y in with_concepts {
            if in_head.contains(&y) {
                remaining.push(y);
                continue;
            }
            // Split y's part off.
            let (part, rest): (Vec<BodyAtom>, Vec<BodyAtom>) =
                work.body.drain(..).partition(|b| mentions(b, y));
            work.body = rest;
            let t = self.split(part, y, axiom)?;
            work.body.push(BodyAtom::Concept(t, OwlVar::X));
            self.stats.split += 1;
        }
        match remaining[..] {
            [] => Ok(()),
            [y] => {
                let centre_concepts = work
                    .body
                    .iter()
                    .any(|b| matches!(b, BodyAtom::Concept(_, OwlVar::X)));
                let existential = matches!(work.head, Some(HeadAtom::AtLeast { .. }));
                if centre_concepts || existential {
                    return Err(shape);
                }
                // y becomes the centre; the old centre a neighbour.
                let old = OwlVar::Y(u16::MAX);
                let swap = |v: OwlVar| match v {
                    OwlVar::X => old,
                    v if v == y => OwlVar::X,
                    v => v,
                };
                work.body = work.body.iter().map(|b| rename_body(b, &swap)).collect();
                work.head = work.head.as_ref().map(|h| rename_head(h, &swap));
                self.stats.recentred += 1;
                Ok(())
            }
            _ => Err(shape),
        }
    }

    /// The fresh name of a neighbour's part `part` (its role atoms with the centre and its
    /// concepts): `part[y ↦ x, x ↦ z] → T(z)`. A definition (`T := ∃R⁻.(…)` read from the
    /// neighbour), so without a source axiom.
    fn split(
        &mut self,
        part: Vec<BodyAtom>,
        y: OwlVar,
        axiom: usize,
    ) -> Result<Concept, Unsupported> {
        let old = OwlVar::Y(u16::MAX);
        let swap = |v: OwlVar| match v {
            OwlVar::X => old,
            v if v == y => OwlVar::X,
            v => v,
        };
        let body: Vec<BodyAtom> = part.iter().map(|b| rename_body(b, &swap)).collect();
        let (mut pats, vars) = self.body(&body, axiom)?;
        pats.sort_unstable();
        pats.dedup();
        let key = pats.clone().into_boxed_slice();
        let t = match self.splits.get(&key) {
            Some(&t) => t,
            None => {
                let t = self.internal();
                self.splits.insert(key, t);
                let Some(&z) = vars.get(&old) else {
                    return Err(Unsupported::ClauseShape { axiom });
                };
                self.add(
                    pats,
                    Some(HeadPat {
                        kind: KindPat::Concept,
                        pred: t,
                        term: TermPat::Var(z),
                    }),
                    &[],
                );
                t
            }
        };
        // An internal concept in the clauses' terms: past the fresh names, never flipped.
        Ok(Concept::Fresh(t - self.fresh_base))
    }

    /// Body patterns, with `nrese-owl`'s neighbours numbered `z₀, z₁, …`.
    fn body(
        &mut self,
        body: &[BodyAtom],
        axiom: usize,
    ) -> Result<(Vec<BodyPat>, HashMap<OwlVar, Var>), Unsupported> {
        let shape = Unsupported::ClauseShape { axiom };
        let mut vars: HashMap<OwlVar, Var> = HashMap::new();
        vars.insert(OwlVar::X, Var::X);
        let mut pats = Vec::with_capacity(body.len());
        for b in body {
            match b {
                BodyAtom::Concept(c, OwlVar::X) => pats.push(BodyPat::Concept(self.concept_id(*c))),
                BodyAtom::Role(r, a, b) => {
                    let r = self.role(*r);
                    let next = vars.len() as u8 - 1;
                    match (*a, *b) {
                        (OwlVar::X, OwlVar::X) => pats.push(BodyPat::Out(r, Var::X)),
                        (OwlVar::X, v) => {
                            let z = *vars.entry(v).or_insert(Var::Z(next));
                            pats.push(BodyPat::Out(r, z));
                        }
                        (v, OwlVar::X) => {
                            let Var::Z(i) = *vars.entry(v).or_insert(Var::Z(next)) else {
                                return Err(shape);
                            };
                            pats.push(BodyPat::In(r, i));
                        }
                        _ => return Err(shape),
                    }
                }
                _ => return Err(shape),
            }
        }
        if vars.len() > 200 {
            return Err(shape);
        }
        Ok((pats, vars))
    }

    /// The id of a concept that is neither flipped nor complemented here (internal ones
    /// included).
    fn concept_id(&self, c: Concept) -> ConceptId {
        match c {
            Concept::Named(t) => self.index[&t],
            Concept::Fresh(q) => self.fresh_base + q,
        }
    }

    /// Step 6 and the DL-clauses.
    fn emit(
        &mut self,
        work: Work,
        sources: &[Box<[u32]>],
        axiom: usize,
    ) -> Result<(), Unsupported> {
        let shape = Unsupported::ClauseShape { axiom };
        let (body, vars) = self.body(&work.body, axiom)?;
        let var = |v: OwlVar| vars.get(&v).copied().ok_or(shape.clone());
        let head = match work.head {
            None => None,
            Some(HeadAtom::Concept(c, v)) => Some(HeadPat {
                kind: KindPat::Concept,
                pred: self.concept_id(c),
                term: TermPat::Var(var(v)?),
            }),
            Some(HeadAtom::Role(r, a, b)) => {
                let r = self.role(r);
                let (kind, v) = match (a, b) {
                    (OwlVar::X, v) => (KindPat::Out, v),
                    (v, OwlVar::X) => (KindPat::In, v),
                    _ => return Err(shape),
                };
                Some(HeadPat {
                    kind,
                    pred: r,
                    term: TermPat::Var(var(v)?),
                })
            }
            Some(HeadAtom::AtLeast {
                role,
                filler,
                var: OwlVar::X,
                ..
            }) => {
                let f = self.func(role, filler);
                let func = self.program.funcs[f as usize];
                self.add(
                    body.clone(),
                    Some(HeadPat {
                        kind: func.role.0,
                        pred: func.role.1,
                        term: TermPat::Func(f),
                    }),
                    sources,
                );
                let Some(b) = func.filler else {
                    return Ok(());
                };
                Some(HeadPat {
                    kind: KindPat::Concept,
                    pred: b,
                    term: TermPat::Func(f),
                })
            }
            Some(_) => return Err(shape),
        };
        self.add(body, head, sources);
        Ok(())
    }

    /// The Skolem function of `∃role.filler`.
    fn func(&mut self, role: ObjProp, filler: Filler) -> FuncId {
        let role = match role {
            ObjProp::Named(p) => (KindPat::Out, self.role(p)),
            ObjProp::Inverse(p) => (KindPat::In, self.role(p)),
        };
        let filler = match filler {
            Filler::Top => None,
            Filler::Is(c) | Filler::Not(c) => {
                let (id, flipped) = self.concept(c);
                let complemented = matches!(filler, Filler::Not(_)) != flipped;
                Some(if complemented { self.negation(id) } else { id })
            }
        };
        let key = Func { role, filler };
        if let Some(&f) = self.funcs.get(&key) {
            return f;
        }
        let f = self.program.funcs.len() as FuncId;
        self.program.funcs.push(key);
        self.funcs.insert(key, f);
        f
    }
}

fn mentions(b: &BodyAtom, v: OwlVar) -> bool {
    match b {
        BodyAtom::Concept(_, w) | BodyAtom::Nominal(_, w) => *w == v,
        BodyAtom::Role(_, a, c) | BodyAtom::Data(_, a, c) => *a == v || *c == v,
    }
}

fn rename_body(b: &BodyAtom, f: &dyn Fn(OwlVar) -> OwlVar) -> BodyAtom {
    match b {
        BodyAtom::Concept(c, v) => BodyAtom::Concept(*c, f(*v)),
        BodyAtom::Role(r, a, c) => BodyAtom::Role(*r, f(*a), f(*c)),
        BodyAtom::Nominal(t, v) => BodyAtom::Nominal(*t, f(*v)),
        BodyAtom::Data(p, a, c) => BodyAtom::Data(*p, f(*a), f(*c)),
    }
}

fn rename_head(h: &HeadAtom, f: &dyn Fn(OwlVar) -> OwlVar) -> HeadAtom {
    match h {
        HeadAtom::Concept(c, v) => HeadAtom::Concept(*c, f(*v)),
        HeadAtom::Role(r, a, b) => HeadAtom::Role(*r, f(*a), f(*b)),
        other => other.clone(),
    }
}
