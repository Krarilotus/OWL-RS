//! The ontology as the context core reads it: DL-clauses in the form of Bate et al.
//! (JAIR 2018, §2.4), over dense ids, with the indexes the rules look clauses up by.
//!
//! A DL-clause has a central variable `x` and neighbour variables `z₀, z₁, …`. Its body
//! atoms are `B(x)`, `S(x, x)`, `S(x, zᵢ)` and `S(zᵢ, x)`; its head is one atom (Horn) or
//! none (`⊥`), and may name a successor `f(x)`: `∃R.B` in a head is `R(x, f(x))` and
//! `B(f(x))`, two clauses, with `f` the Skolem function of `∃R.B`.

use nrese_owl::Term;

use super::atoms::{Atom, CTerm, ConceptId, FuncId, Kind, RoleId, TermOrder};

/// A variable of a DL-clause: the centre or a neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Var {
    X,
    Z(u8),
}

/// A body atom of a DL-clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BodyPat {
    /// `B(x)`.
    Concept(ConceptId),
    /// `S(x, v)`; `S(x, x)` for `v = X`.
    Out(RoleId, Var),
    /// `S(zᵢ, x)`.
    In(RoleId, u8),
}

/// A term in a DL-clause head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TermPat {
    Var(Var),
    /// The successor `f(x)`.
    Func(FuncId),
}

/// A head atom of a DL-clause: `kind`, predicate, term (as in [`Atom`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HeadPat {
    pub kind: KindPat,
    pub pred: u32,
    pub term: TermPat,
}

/// [`Kind`] with an order, for sorting clauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KindPat {
    Concept,
    Out,
    In,
}

impl KindPat {
    pub fn kind(self) -> Kind {
        match self {
            KindPat::Concept => Kind::Concept,
            KindPat::Out => Kind::Out,
            KindPat::In => Kind::In,
        }
    }
}

/// A DL-clause with the source axioms it stands for (any one of them gives it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlClause {
    pub body: Box<[BodyPat]>,
    /// `None`: `⊥`.
    pub head: Option<HeadPat>,
    /// Indexes into the ontology's axioms.
    pub sources: Box<[u32]>,
}

/// A successor function: the existential restriction it is the Skolem function of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Func {
    /// The edge `S(x, f(x))` (`Out`) or `S(f(x), x)` (`In`, an inverse role).
    pub role: (KindPat, RoleId),
    /// The filler `B` of `B(f(x))`; `None` for `⊤`.
    pub filler: Option<ConceptId>,
}

/// Where a body atom of a DL-clause sits: clause and position.
pub type Slot = (u32, u8);

/// The compiled ontology, immutable while the contexts saturate.
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub clauses: Vec<DlClause>,
    /// Clauses with an empty body (applied when a context starts).
    pub facts: Vec<u32>,
    /// Per clause, how many neighbour variables it has.
    pub vars: Vec<u8>,
    /// Hyper's index (the paper's `ontologyIndex1`): body atoms `B(x)` by concept, and
    /// `S(x, ·)` and `S(·, x)` by role.
    pub by_concept: Vec<Vec<Slot>>,
    pub by_out: Vec<Vec<Slot>>,
    pub by_in: Vec<Vec<Slot>>,
    pub funcs: Vec<Func>,
    /// The successor triggers `Su(O)` (Definition 2), with the fillers of existentials
    /// added: `B(x)`, `S(x, y)`, `S(y, x)`.
    pub su_concept: Vec<bool>,
    pub su_out: Vec<bool>,
    pub su_in: Vec<bool>,
    /// The named class of each concept below [`Program::named`]; fresh ones after.
    pub names: Vec<Term>,
    pub concepts: u32,
    pub roles: u32,
    pub order: TermOrder,
}

impl Program {
    /// Whether `atom`, an atom of a successor context, is a successor trigger: the
    /// predecessor sends it as a possible atom (`K₂` of the Succ rule).
    pub fn is_su(&self, atom: Atom) -> bool {
        let p = atom.pred() as usize;
        match (atom.kind(), atom.term()) {
            (Kind::Concept, CTerm::X) => self.su_concept[p],
            (Kind::Out, CTerm::Y) => self.su_out[p],
            (Kind::In, CTerm::Y) => self.su_in[p],
            _ => false,
        }
    }

    /// Whether a head literal is a predecessor trigger `Pr(O)` (or `⊥`): the Pred rule
    /// propagates clauses with such heads to the predecessors.
    pub fn is_pr(&self, head: Atom) -> bool {
        if head.is_bottom() {
            return true;
        }
        let p = head.pred() as usize;
        match (head.kind(), head.term()) {
            (Kind::Concept, CTerm::Y) => true,
            // S(x, y) ∈ Pr iff S(y, x) ∈ Su, and back.
            (Kind::Out, CTerm::Y) => self.su_in[p],
            (Kind::In, CTerm::Y) => self.su_out[p],
            _ => false,
        }
    }

    /// The DL-clause body slots an atom of a context may match (with `σ(x) = x`).
    pub fn slots(&self, atom: Atom) -> (&[Slot], &[Slot]) {
        let p = atom.pred() as usize;
        match (atom.kind(), atom.term()) {
            (Kind::Concept, CTerm::X) => (&self.by_concept[p], &[]),
            (Kind::Concept, _) => (&[], &[]),
            // S(x, x) is S(x, z) and S(z, x) with z = x.
            (Kind::Out, CTerm::X) => (&self.by_out[p], &self.by_in[p]),
            (Kind::Out, _) => (&self.by_out[p], &[]),
            (Kind::In, _) => (&self.by_in[p], &[]),
        }
    }

    /// Builds the indexes and trigger sets from `clauses` and `funcs`.
    pub fn index(&mut self) {
        let (c, r) = (self.concepts as usize, self.roles as usize);
        self.by_concept = vec![Vec::new(); c];
        self.by_out = vec![Vec::new(); r];
        self.by_in = vec![Vec::new(); r];
        self.su_concept = vec![false; c];
        self.su_out = vec![false; r];
        self.su_in = vec![false; r];
        self.facts.clear();
        self.vars = self
            .clauses
            .iter()
            .map(|clause| {
                clause
                    .body
                    .iter()
                    .filter_map(|b| match *b {
                        BodyPat::Out(_, Var::Z(i)) | BodyPat::In(_, i) => Some(i + 1),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for (i, clause) in self.clauses.iter().enumerate() {
            if clause.body.is_empty() {
                self.facts.push(i as u32);
            }
            for (pos, atom) in clause.body.iter().enumerate() {
                let slot = (i as u32, pos as u8);
                match *atom {
                    BodyPat::Concept(b) => {
                        self.by_concept[b as usize].push(slot);
                        self.su_concept[b as usize] = true;
                    }
                    BodyPat::Out(s, v) => {
                        self.by_out[s as usize].push(slot);
                        if v != Var::X {
                            self.su_out[s as usize] = true;
                        }
                    }
                    BodyPat::In(s, _) => {
                        self.by_in[s as usize].push(slot);
                        self.su_in[s as usize] = true;
                    }
                }
            }
        }
        for f in &self.funcs {
            if let Some(b) = f.filler {
                self.su_concept[b as usize] = true;
            }
        }
    }

    /// The named class of a concept, if it is one.
    pub fn name(&self, c: ConceptId) -> Option<Term> {
        self.names.get(c as usize).copied()
    }

    pub fn named(&self) -> u32 {
        self.names.len() as u32
    }
}
