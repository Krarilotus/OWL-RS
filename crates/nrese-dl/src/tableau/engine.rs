//! The engine's state and its deterministic part: asserting facts with their clash
//! checks, and saturation, which runs the queues (the unprocessed suffixes of the fact
//! tables) to a fixpoint: merges first, then the clauses that hold everywhere, then the
//! clauses a new concept or edge can trigger (docs/design/owl2-dl.md §6, "queues by
//! kind, deterministic first").

use std::time::Instant;

use super::Config;
use super::depset::{DepSetId, DepSets};
use super::graph::{Graph, Mark, NONE};
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
}

pub type Step<T> = Result<T, Stop>;

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
    /// `a ≈ b`; `at_root` is the root an at-most restriction raised it at, else NONE.
    Equal(u32, u32, u32),
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
    pub alternatives: Vec<Lit>,
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
            Lit::Equal(a, b, at_root) => {
                let (a, b) = (self.g.find(a), self.g.find(b));
                if a == b {
                    return Ok(());
                }
                if let Some(i) = self.g.unequal(a, b) {
                    let d = self.g.inequalities[i as usize].dep;
                    return Err(self.clash(dep, d));
                }
                self.g
                    .equalities
                    .push(super::graph::Equality { a, b, dep, at_root });
            }
        }
        Ok(())
    }

    /// Asserts the negation of `lit` where the engine has one (semantic branching).
    pub fn negate(&mut self, lit: Lit, dep: DepSetId) -> Step<()> {
        match lit {
            Lit::Concept(c, n) => self.add_negative(n, c, dep, proof::NEGATION),
            Lit::Equal(a, b, _) => {
                let (a, b) = (self.g.find(a), self.g.find(b));
                self.add_inequality(a, b, dep, proof::NEGATION)
            }
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
    pub fn holds(&self, lit: Lit) -> Result<bool, DepSetId> {
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
            Lit::Equal(a, b, _) => {
                let (a, b) = (self.g.find(a), self.g.find(b));
                if a == b {
                    return Ok(true);
                }
                if let Some(i) = self.g.unequal(a, b) {
                    return Err(self.g.inequalities[i as usize].dep);
                }
                false
            }
        })
    }

    /// The head atom `h` under `bind`.
    pub fn lit(&self, h: Head, bind: &[u32]) -> Lit {
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
                Lit::Equal(b(x), b(y), root)
            }
            Head::Nominal(i, v) => Lit::Equal(b(v), self.root(i), NONE),
        }
    }

    /// Applies a clause instance: nothing if its head holds, a clash if every head atom is
    /// refuted, the one atom left if only one is, else a disjunction for later.
    pub fn fire(&mut self, clause: u32, bind: &[u32], mut dep: DepSetId) -> Step<()> {
        let c = &self.p.clauses[clause as usize];
        let mut open: [Option<Lit>; 2] = [None, None];
        let mut count = 0;
        for &h in &c.head {
            let lit = self.lit(h, bind);
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
            }
            if (self.done.equalities as usize) < self.g.equalities.len() {
                let e = self.g.equalities[self.done.equalities as usize];
                self.done.equalities += 1;
                self.merge(e.a, e.b, e.dep, e.at_root)?;
                continue;
            }
            if (self.done.nodes as usize) < self.g.nodes.len() {
                let n = self.done.nodes;
                self.done.nodes += 1;
                if self.g.live(n) {
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

    pub fn check_time(&self) -> Step<()> {
        if let Some(limit) = self.config.timeout
            && self.started.elapsed() > limit
        {
            return Err(Stop::GaveUp(format!("the time budget ({limit:?}) ran out")));
        }
        Ok(())
    }
}
