//! The engine's state and its deterministic part: asserting facts with their clash
//! checks, and saturation, which runs the queues (the unprocessed suffixes of the fact
//! tables) to a fixpoint: merges first, then the clauses that hold everywhere, then the
//! clauses a new concept or edge can trigger (docs/design/owl2-dl.md §6, "queues by
//! kind, deterministic first").

use std::time::Instant;

use super::Config;
use super::depset::{DepSetId, DepSets};
use super::graph::{Annot, Graph, Mark, NONE};
use super::hyper::{Firing, join_concept, join_edge};
use super::program::{ConceptId, Head, Program, RoleId};
use super::telemetry::Telemetry;

/// Proofs of facts not made by a clause.
pub mod proof {
    pub const ASSERTED: u32 = u32::MAX - 1;
    pub const EXPANSION: u32 = u32::MAX - 2;
    pub const MERGE: u32 = u32::MAX - 3;
    pub const CHOICE: u32 = u32::MAX - 4;
    pub const NEGATION: u32 = u32::MAX - 5;
}

/// Why a run stops going forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// A clash, with the branch points it depends on.
    Clash(DepSetId),
    /// A budget ran out, or a rule this engine doesn't have is needed.
    GaveUp(String),
    /// This branch can't be completed within the budgets (an at-least restriction too
    /// large to expand), by what it depends on: the search goes on with the others, and
    /// a model found there is an answer, a refutation of all of them is not.
    Abandon(DepSetId, String),
}

pub type Step<T> = Result<T, Stop>;

/// The stop where the NI rule would be needed but the equality's at-most restriction
/// isn't known (a clause whose equalities don't spell one out).
pub fn ni_stop() -> Stop {
    Stop::GaveUp("the NI rule on an equality without its at-most restriction".into())
}

/// A head atom with its variables bound to nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lit {
    Concept(ConceptId, u32),
    Role(RoleId, u32, u32),
    Number {
        at_most: bool,
        number: u32,
        node: u32,
    },
    /// `a ≈ b` with its annotation (the at-most restriction at a root that raised it).
    Equal(u32, u32, Annot),
    /// `a ≉ b` (a key's alternative: two data values differ).
    Unequal(u32, u32),
}

/// A disjunction waiting for a choice: the clause and its binding.
#[derive(Debug, Clone, Copy)]
pub struct Pending {
    pub clause: u32,
    pub bind: u32,
    pub dep: DepSetId,
}

/// A branch point.
#[derive(Debug, Clone)]
pub struct Frame {
    pub mark: Mark,
    pub pending: u32,
    pub bindings: u32,
    pub pending_open: u32,
    pub ni_pending: u32,
    pub ni_open: u32,
    /// Take later alternatives with the failed ones' negations (not for the NI rule's
    /// choice, which picks among fresh individuals rather than a disjunction that holds).
    pub semantic: bool,
    pub alternatives: Vec<Lit>,
    /// For a disjunction of a clause: the clause (else `NONE`) and each alternative's
    /// head atom, to count its failures.
    pub clause: u32,
    pub heads: Vec<u8>,
    pub next: usize,
    /// What the disjunction itself depends on.
    pub premise: DepSetId,
    /// What the failed alternatives' clashes depended on, besides this branch point.
    pub failed: DepSetId,
}

/// How far each queue is processed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Done {
    pub nodes: u32,
    pub unary: u32,
    pub edges: u32,
    pub equalities: u32,
}

pub struct Engine<'a> {
    pub p: &'a Program,
    pub config: &'a Config,
    pub g: Graph,
    pub deps: DepSets,
    pub done: Done,
    pub pending: Vec<Pending>,
    pub bindings: Vec<u32>,
    pub pending_open: u32,
    pub frames: Vec<Frame>,
    /// The root node of each individual (follow merges with `find`).
    pub roots: Vec<u32>,
    pub stats: Telemetry,
    pub started: Instant,
    pub firings: Vec<Firing>,
    pub blocking: super::blocking::Blocking,
    pub ni: super::ni::Ni,
    /// Per clause and head atom: how often that disjunct failed (disjunct learning).
    pub failures: Vec<Vec<u32>>,
    /// How far the datatype stage has read the fact tables.
    pub data_done: super::data::DataDone,
    pub theory: crate::datatypes::DatatypeTheory,
    /// Why a datatype check was only approximate, if one was: a model is then no answer.
    pub data_approximate: Option<String>,
}

impl<'a> Engine<'a> {
    pub fn new(p: &'a Program, config: &'a Config) -> Self {
        Self {
            p,
            config,
            g: Graph::default(),
            deps: DepSets::default(),
            done: Done::default(),
            pending: Vec::new(),
            bindings: Vec::new(),
            pending_open: 0,
            frames: Vec::new(),
            roots: Vec::new(),
            stats: Telemetry::default(),
            started: Instant::now(),
            firings: Vec::new(),
            blocking: super::blocking::Blocking::default(),
            ni: super::ni::Ni::default(),
            failures: Vec::new(),
            data_done: super::data::DataDone::default(),
            theory: crate::datatypes::DatatypeTheory::default(),
            data_approximate: None,
        }
    }

    pub fn new_node(&mut self, parent: u32, named: u32) -> Step<u32> {
        if self.g.nodes.len() >= self.config.max_nodes {
            return Err(Stop::GaveUp(format!(
                "the node budget ({}) ran out",
                self.config.max_nodes
            )));
        }
        let id = self.g.new_node(parent, named);
        self.stats.nodes_created += 1;
        self.stats.peak_nodes = self.stats.peak_nodes.max(self.g.nodes.len() as u64);
        Ok(id)
    }

    /// The node of an individual now.
    pub fn root(&self, individual: u32) -> u32 {
        self.g.find(self.roots[individual as usize])
    }

    /// The node `node` stands for now, and what the merges that made it so depend on.
    pub fn canonical(&mut self, node: u32) -> (u32, DepSetId) {
        let mut dep = DepSetId::EMPTY;
        let deps = &mut self.deps;
        let n = self.g.find_with(node, |d| dep = deps.union(dep, d));
        (n, dep)
    }

    /// `a ≈ b` between the nodes `a` and `b` stand for now, with `dep` and what those
    /// merges depend on.
    pub fn canonical_pair(&mut self, a: u32, b: u32, dep: DepSetId) -> (u32, u32, DepSetId) {
        let (a, da) = self.canonical(a);
        let (b, db) = self.canonical(b);
        let d = self.deps.union(da, db);
        (a, b, self.deps.union(dep, d))
    }

    /// Asserts `lit` with `dep`; a clash if its negation holds.
    pub fn assert(&mut self, lit: Lit, dep: DepSetId, proof: u32) -> Step<()> {
        match lit {
            Lit::Concept(c, n) => {
                if let Some(neg) = self.g.negative(n, c) {
                    let d = self.g.negatives[neg as usize].dep;
                    return Err(self.clash(dep, d));
                }
                if self.g.add_concept(n, c, dep, proof) {
                    self.stats.facts += 1;
                }
            }
            Lit::Role(r, a, b) => {
                if self.g.add_edge(r, a, b, dep, proof) {
                    self.stats.facts += 1;
                }
            }
            Lit::Number {
                at_most,
                number,
                node,
            } => {
                if self.g.add_number(node, at_most, number, dep, proof) {
                    self.stats.facts += 1;
                }
            }
            Lit::Equal(a, b, annot) => {
                let (a, b, dep) = self.canonical_pair(a, b, dep);
                if let Some(i) = self.g.unequal(a, b) {
                    let d = self.g.inequalities[i as usize].dep;
                    return Err(self.clash(dep, d));
                }
                if self.needs_ni(Lit::Equal(a, b, annot)) {
                    // The NI rule's, before the ≈-rule (rule precedence), even if a = b.
                    self.ni_defer(a, b, dep, annot);
                    return Ok(());
                }
                if a == b {
                    return Ok(());
                }
                self.g
                    .equalities
                    .push(super::graph::Equality { a, b, dep, annot });
            }
            Lit::Unequal(a, b) => {
                let (a, b, dep) = self.canonical_pair(a, b, dep);
                return self.add_inequality(a, b, dep, proof);
            }
        }
        Ok(())
    }

    /// Asserts the negation of `lit` where the engine has one (semantic branching).
    pub fn negate(&mut self, lit: Lit, dep: DepSetId) -> Step<()> {
        match lit {
            Lit::Concept(c, n) => self.add_negative(n, c, dep, proof::NEGATION),
            Lit::Equal(a, b, _) => {
                let (a, b, dep) = self.canonical_pair(a, b, dep);
                self.add_inequality(a, b, dep, proof::NEGATION)
            }
            Lit::Unequal(a, b) => self.assert(Lit::Equal(a, b, Annot::NONE), dep, proof::NEGATION),
            Lit::Role(..) | Lit::Number { .. } => Ok(()),
        }
    }

    pub fn add_negative(&mut self, n: u32, c: ConceptId, dep: DepSetId, proof: u32) -> Step<()> {
        if let Some(pos) = self.g.concept(n, c) {
            let d = self.g.unary[pos as usize].dep;
            return Err(self.clash(dep, d));
        }
        if self.g.add_negative(n, c, dep, proof) {
            self.stats.facts += 1;
        }
        Ok(())
    }

    pub fn add_inequality(&mut self, a: u32, b: u32, dep: DepSetId, proof: u32) -> Step<()> {
        if a == b {
            return Err(self.clash(dep, DepSetId::EMPTY));
        }
        if self.g.add_inequality(a, b, dep, proof) {
            self.stats.facts += 1;
        }
        Ok(())
    }

    pub fn clash(&mut self, a: DepSetId, b: DepSetId) -> Stop {
        self.stats.clashes += 1;
        Stop::Clash(self.deps.union(a, b))
    }

    /// Whether `lit` holds; `Err(dep)` if it is refuted (with what refutes it).
    pub fn holds(&mut self, lit: Lit) -> Result<bool, DepSetId> {
        Ok(match lit {
            Lit::Concept(c, n) => {
                if self.g.concept(n, c).is_some() {
                    return Ok(true);
                }
                if let Some(neg) = self.g.negative(n, c) {
                    return Err(self.g.negatives[neg as usize].dep);
                }
                false
            }
            Lit::Role(r, a, b) => self.g.edge(r, a, b).is_some(),
            Lit::Number {
                at_most,
                number,
                node,
            } => self.g.number(node, at_most, number).is_some(),
            Lit::Equal(a, b, annot) => {
                let (s, t) = (self.g.find(a), self.g.find(b));
                let ni = self.needs_ni(lit);
                if (s == t && !ni) || (ni && self.ni_pending(a, b, annot)) {
                    return Ok(true);
                }
                if let Some(i) = self.g.unequal(s, t) {
                    // Refuted between the nodes `a` and `b` were merged into: by the
                    // inequality and those merges.
                    let d = self.g.inequalities[i as usize].dep;
                    let (_, _, d) = self.canonical_pair(a, b, d);
                    return Err(d);
                }
                false
            }
            Lit::Unequal(a, b) => {
                let (s, t, d) = self.canonical_pair(a, b, DepSetId::EMPTY);
                if s == t {
                    return Err(d);
                }
                self.g.unequal(s, t).is_some()
            }
        })
    }

    /// Whether `lit` is an annotated equality the NI rule governs (JAIR 2009, Table 5):
    /// raised by an at-most restriction at a root, between blockable nodes of which one
    /// isn't the root's successor. The rule applies even where both sides are one node,
    /// so such an equality never counts as holding.
    pub fn needs_ni(&self, lit: Lit) -> bool {
        let Lit::Equal(a, b, annot) = lit else {
            return false;
        };
        if annot.root == NONE {
            return false;
        }
        let root = self.g.find(annot.root);
        let (a, b) = (self.g.find(a), self.g.find(b));
        let node = |n: u32| &self.g.nodes[n as usize];
        use super::graph::flag::{CONCRETE, ROOT};
        let blockable = |n: u32| node(n).flags & (ROOT | CONCRETE) == 0;
        blockable(a) && blockable(b) && !(node(a).parent == root && node(b).parent == root)
    }

    /// The head atom `h` under `bind`, of a clause whose equalities have the annotation
    /// `annotation` (`NONE` if none).
    pub fn lit(&self, h: Head, bind: &[u32], annotation: u32) -> Lit {
        let b = |v: u8| bind[v as usize];
        match h {
            Head::Concept(c, v) => Lit::Concept(c, b(v)),
            Head::Role(r, x, y) => Lit::Role(r, b(x), b(y)),
            Head::AtLeast(number, v) => Lit::Number {
                at_most: false,
                number,
                node: b(v),
            },
            Head::AtMost(number, v) => Lit::Number {
                at_most: true,
                number,
                node: b(v),
            },
            Head::Equal(x, y) => {
                let at = b(0);
                let root = if self.g.nodes[at as usize].flags & super::graph::flag::ROOT != 0 {
                    at
                } else {
                    NONE
                };
                Lit::Equal(
                    b(x),
                    b(y),
                    Annot {
                        root,
                        number: annotation,
                    },
                )
            }
            // The individual's own node: whoever asserts or tests the equality follows it
            // to its representative with the merges' dependencies.
            Head::Nominal(i, v) => Lit::Equal(b(v), self.roots[i as usize], Annot::NONE),
            Head::Unequal(x, y) => Lit::Unequal(b(x), b(y)),
        }
    }

    /// Applies a clause instance: nothing if its head holds, a clash if every head atom is
    /// refuted, the one atom left if only one is, else a disjunction for later.
    pub fn fire(&mut self, clause: u32, bind: &[u32], mut dep: DepSetId) -> Step<()> {
        let c = &self.p.clauses[clause as usize];
        let mut open: [Option<Lit>; 2] = [None, None];
        let mut count = 0;
        for &h in &c.head {
            let lit = self.lit(h, bind, c.annotation);
            match self.holds(lit) {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    if count < 2 {
                        open[count] = Some(lit);
                    }
                    count += 1;
                }
                Err(refuted) => dep = self.deps.union(dep, refuted),
            }
        }
        self.stats.clauses_fired += 1;
        match (count, open[0]) {
            (0, _) => Err(self.clash(dep, DepSetId::EMPTY)),
            (1, Some(lit)) => self.assert(lit, dep, c.source),
            _ => {
                let at = self.bindings.len() as u32;
                self.bindings.extend_from_slice(&bind[..c.vars as usize]);
                self.pending.push(Pending {
                    clause,
                    bind: at,
                    dep,
                });
                Ok(())
            }
        }
    }

    // Saturation ---------------------------------------------------------------------------

    /// Runs every queue to a fixpoint.
    pub fn saturate(&mut self) -> Step<()> {
        let started = Instant::now();
        let out = self.saturate_inner();
        self.stats.saturate += started.elapsed();
        out
    }

    fn saturate_inner(&mut self) -> Step<()> {
        let mut steps = 0u32;
        loop {
            steps = steps.wrapping_add(1);
            if steps.is_multiple_of(4096) {
                self.check_time()?;
                self.check_memory()?;
            }
            if (self.done.equalities as usize) < self.g.equalities.len() {
                let e = self.g.equalities[self.done.equalities as usize];
                self.done.equalities += 1;
                self.merge(e.a, e.b, e.dep, e.annot)?;
                continue;
            }
            if (self.done.nodes as usize) < self.g.nodes.len() {
                let n = self.done.nodes;
                self.done.nodes += 1;
                if self.g.live(n)
                    && self.g.nodes[n as usize].flags & super::graph::flag::CONCRETE == 0
                {
                    for i in 0..self.p.everywhere.len() {
                        let clause = self.p.everywhere[i];
                        self.fire(clause, &[n], DepSetId::EMPTY)?;
                    }
                }
                continue;
            }
            if (self.done.unary as usize) < self.g.unary.len() {
                let i = self.done.unary;
                self.done.unary += 1;
                let f = self.g.unary[i as usize];
                if self.g.live(f.node) {
                    join_concept(self.p, &self.g, &mut self.deps, i, &mut self.firings);
                    self.apply_firings()?;
                }
                continue;
            }
            if (self.done.edges as usize) < self.g.edges.len() {
                let i = self.done.edges;
                self.done.edges += 1;
                let e = self.g.edges[i as usize];
                if self.g.live(e.from) && self.g.live(e.to) {
                    join_edge(self.p, &self.g, &mut self.deps, i, &mut self.firings);
                    self.apply_firings()?;
                }
                continue;
            }
            return Ok(());
        }
    }

    fn apply_firings(&mut self) -> Step<()> {
        if self.firings.len() >= super::hyper::MAX_FIRINGS {
            self.firings.clear();
            return Err(Stop::GaveUp(format!(
                "a join reached {} clause instances",
                super::hyper::MAX_FIRINGS
            )));
        }
        let firings = std::mem::take(&mut self.firings);
        let mut out = Ok(());
        for f in &firings {
            if let Err(stop) = self.fire(f.clause, &f.bind[..], f.dep) {
                out = Err(stop);
                break;
            }
        }
        self.firings = firings;
        self.firings.clear();
        out
    }

    /// The engine's memory now: every table, index and arena, by capacity.
    pub fn bytes(&self) -> usize {
        use std::mem::size_of;
        self.g.bytes()
            + self.deps.len() * 24
            + self.pending.capacity() * size_of::<Pending>()
            + self.bindings.capacity() * 4
            + self.firings.capacity() * size_of::<Firing>()
            + self.frames.capacity() * size_of::<Frame>()
            + self.blocking.bytes
            + self.ni.bytes()
    }

    pub fn check_memory(&self) -> Step<()> {
        let used = self.bytes();
        if used > self.config.max_memory {
            return Err(Stop::GaveUp(format!(
                "the memory budget ({} MiB) ran out",
                self.config.max_memory >> 20
            )));
        }
        Ok(())
    }

    pub fn check_time(&self) -> Step<()> {
        if let Some(limit) = self.config.timeout
            && self.started.elapsed() > limit
        {
            return Err(Stop::GaveUp(format!("the time budget ({limit:?}) ran out")));
        }
        Ok(())
    }
}
