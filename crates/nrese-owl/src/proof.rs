//! The proof IR and justifications (docs/design/owl2-dl.md §10): one form for the
//! derivations of every engine (the RL rules, the EL classifier, later the context core),
//! and the "why" answers computed on it.
//!
//! - **The graph** ([`ProofGraph`]) is a directed hypergraph: inferences from premises
//!   (facts) and axioms (the leaves a justification is made of) to a conclusion. Facts and
//!   axioms are whatever the engine has: triples and asserted triples for RL, subsumptions
//!   and OWL axioms elsewhere.
//! - **Justifications** of the goal are the minimal sets of axioms from which it is derived,
//!   offered in order of cost (E): [`ProofGraph::one`] (one, by deletion from a proof's
//!   leaves), [`ProofGraph::core`] (the axioms every justification has: those without which
//!   the goal is lost), and [`ProofGraph::justifications`], an enumeration that yields them
//!   smallest first (`top-k` is its first k, `union` and `all` its end), with a budget and
//!   a completeness flag.
//! - **The enumeration** is resolution over the inferences with a selected premise
//!   (Kazakov and Skočovský, *Enumerating Justifications using Resolution*, IJCAR 2018:
//!   PULi), the clauses taken smallest first so that every justification comes out
//!   minimal and in order of size; clauses subsumed by a processed one, or containing a
//!   found justification, are dropped.
//! - **Proofs** ([`Proof`]) are the inferences that derive the goal from a set of axioms,
//!   shallowest first; [`Proof::check`] validates one step by step, with the engine's own
//!   check of each inference against its rule.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;

/// What facts and axioms need to be.
pub trait Node: Copy + Eq + Hash + Ord + Debug {}
impl<T: Copy + Eq + Hash + Ord + Debug> Node for T {}

/// One inference: `rule` derives `conclusion` from `premises` and `axioms`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Inference<F, A> {
    pub rule: String,
    pub premises: Vec<F>,
    pub axioms: Vec<A>,
    pub conclusion: F,
}

/// The inferences that may derive a goal.
#[derive(Debug, Clone)]
pub struct ProofGraph<F: Node, A: Node> {
    pub goal: F,
    inferences: Vec<Inference<F, A>>,
    by_conclusion: HashMap<F, Vec<usize>>,
    seen: HashSet<Inference<F, A>>,
    /// False when the engine stopped collecting inferences early (a budget): answers are
    /// then over the inferences collected.
    pub complete: bool,
}

/// Justifications found, smallest first, and whether they are all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Justifications<A> {
    pub found: Vec<Vec<A>>,
    /// Every justification was found (no limit or budget stopped the enumeration, and the
    /// graph is complete).
    pub complete: bool,
}

impl<F: Node, A: Node> ProofGraph<F, A> {
    pub fn new(goal: F) -> Self {
        Self {
            goal,
            inferences: Vec::new(),
            by_conclusion: HashMap::new(),
            seen: HashSet::new(),
            complete: true,
        }
    }

    /// Adds an inference (premises and axioms as sets; duplicates are one). An inference
    /// with its conclusion among its premises derives nothing and is left out.
    pub fn add(&mut self, rule: impl Into<String>, premises: &[F], axioms: &[A], conclusion: F) {
        let mut premises = premises.to_vec();
        premises.sort_unstable();
        premises.dedup();
        if premises.contains(&conclusion) {
            return;
        }
        let mut axioms = axioms.to_vec();
        axioms.sort_unstable();
        axioms.dedup();
        let inference = Inference {
            rule: rule.into(),
            premises,
            axioms,
            conclusion,
        };
        if !self.seen.insert(inference.clone()) {
            return;
        }
        self.by_conclusion
            .entry(conclusion)
            .or_default()
            .push(self.inferences.len());
        self.inferences.push(inference);
    }

    pub fn inferences(&self) -> &[Inference<F, A>] {
        &self.inferences
    }

    /// The inferences of `fact`.
    pub fn of(&self, fact: F) -> impl Iterator<Item = &Inference<F, A>> {
        self.by_conclusion
            .get(&fact)
            .into_iter()
            .flatten()
            .map(|&i| &self.inferences[i])
    }

    /// Every axiom some inference uses, sorted.
    pub fn axioms(&self) -> Vec<A> {
        let set: BTreeSet<A> = self
            .inferences
            .iter()
            .flat_map(|i| i.axioms.iter().copied())
            .collect();
        set.into_iter().collect()
    }

    /// Forward chaining from the allowed axioms: for each derived fact, the inference that
    /// derived it first (so the shallowest), or `None` if the goal isn't derived.
    fn chain(&self, allowed: &dyn Fn(&A) -> bool) -> Option<HashMap<F, usize>> {
        let mut missing: Vec<usize> = Vec::with_capacity(self.inferences.len());
        let mut waiting: HashMap<F, Vec<usize>> = HashMap::new();
        let mut ready: Vec<usize> = Vec::new();
        for (i, inference) in self.inferences.iter().enumerate() {
            if !inference.axioms.iter().all(allowed) {
                missing.push(usize::MAX);
                continue;
            }
            missing.push(inference.premises.len());
            for &p in &inference.premises {
                waiting.entry(p).or_default().push(i);
            }
            if inference.premises.is_empty() {
                ready.push(i);
            }
        }
        let mut by: HashMap<F, usize> = HashMap::new();
        // Breadth first: proofs as shallow as can be.
        while !ready.is_empty() {
            let mut next = Vec::new();
            for i in ready {
                let fact = self.inferences[i].conclusion;
                if by.contains_key(&fact) {
                    continue;
                }
                by.insert(fact, i);
                if fact == self.goal {
                    return Some(by);
                }
                for &j in waiting.get(&fact).into_iter().flatten() {
                    if missing[j] != usize::MAX {
                        missing[j] -= 1;
                        if missing[j] == 0 {
                            next.push(j);
                        }
                    }
                }
            }
            ready = next;
        }
        None
    }

    /// Whether the goal is derived from the axioms `allowed` lets through.
    pub fn derivable(&self, allowed: &dyn Fn(&A) -> bool) -> bool {
        self.chain(allowed).is_some()
    }

    /// The shallowest proof of the goal from the axioms `allowed` lets through.
    pub fn proof_from(&self, allowed: &dyn Fn(&A) -> bool) -> Option<Proof<F, A>> {
        let by = self.chain(allowed)?;
        // The inferences the goal's proof uses, premises before conclusions.
        let mut order: Vec<usize> = Vec::new();
        let mut placed: HashSet<F> = HashSet::new();
        let mut stack: Vec<(F, bool)> = vec![(self.goal, false)];
        while let Some((fact, expanded)) = stack.pop() {
            if placed.contains(&fact) {
                continue;
            }
            let i = by[&fact];
            if expanded {
                placed.insert(fact);
                order.push(i);
                continue;
            }
            stack.push((fact, true));
            for &p in &self.inferences[i].premises {
                if !placed.contains(&p) {
                    stack.push((p, false));
                }
            }
        }
        Some(Proof {
            goal: self.goal,
            steps: order
                .into_iter()
                .map(|i| self.inferences[i].clone())
                .collect(),
        })
    }

    /// One justification: a proof's axioms, minimised by deletion; `None` if the goal isn't
    /// derived at all.
    pub fn one(&self) -> Option<Vec<A>> {
        let proof = self.proof_from(&|_| true)?;
        let mut keep: BTreeSet<A> = proof.axioms().into_iter().collect();
        for a in keep.clone() {
            keep.remove(&a);
            if !self.derivable(&|x| keep.contains(x)) {
                keep.insert(a);
            }
        }
        Some(keep.into_iter().collect())
    }

    /// The axioms in every justification: those without which the goal isn't derived
    /// (empty if it isn't derived at all).
    pub fn core(&self) -> Vec<A> {
        let Some(one) = self.one() else {
            return Vec::new();
        };
        // The core is within any one justification.
        one.into_iter()
            .filter(|a| !self.derivable(&|x| x != a))
            .collect()
    }

    /// The justifications, smallest first: at most `limit`, processing at most `budget`
    /// clauses.
    pub fn justifications(&self, limit: usize, budget: usize) -> Justifications<A> {
        let mut found = Vec::new();
        let mut it = self.enumerate(budget);
        let complete = loop {
            if found.len() == limit {
                // More may follow: complete only if nothing is left to look at.
                break it.queue.is_empty();
            }
            match it.next() {
                Some(j) => found.push(j),
                None => break !it.stopped,
            }
        };
        Justifications {
            found,
            complete: complete && self.complete,
        }
    }

    /// The union of all justifications (the axioms relevant to the goal), if the
    /// enumeration ends within `budget`; else what was found and `complete: false`.
    pub fn union(&self, budget: usize) -> (Vec<A>, bool) {
        let all = self.justifications(usize::MAX, budget);
        let set: BTreeSet<A> = all.found.into_iter().flatten().collect();
        (set.into_iter().collect(), all.complete)
    }

    /// The justifications as a lazy iterator, smallest first, processing at most
    /// `budget` clauses.
    pub fn enumerate(&self, budget: usize) -> Enumeration<'_, F, A> {
        let mut e = Enumeration {
            graph: self,
            queue: BinaryHeap::new(),
            arena: Vec::new(),
            by_conclusion: HashMap::new(),
            derived: HashMap::new(),
            waiting: HashMap::new(),
            found: Vec::new(),
            seq: 0,
            budget,
            stopped: false,
        };
        for inference in &self.inferences {
            e.push(Clause {
                premises: inference.premises.iter().copied().collect(),
                axioms: inference.axioms.iter().copied().collect(),
                conclusion: inference.conclusion,
            });
        }
        e
    }
}

/// A clause of the enumeration: `conclusion` follows from `premises` (still to be
/// resolved) and `axioms`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Clause<F: Node, A: Node> {
    premises: BTreeSet<F>,
    axioms: BTreeSet<A>,
    conclusion: F,
}

impl<F: Node, A: Node> Clause<F, A> {
    fn subsumes(&self, other: &Self) -> bool {
        self.conclusion == other.conclusion
            && self.premises.is_subset(&other.premises)
            && self.axioms.is_subset(&other.axioms)
    }
}

/// The resolution enumeration of justifications ([`ProofGraph::enumerate`]).
pub struct Enumeration<'g, F: Node, A: Node> {
    graph: &'g ProofGraph<F, A>,
    /// Smallest first: by axioms, then premises, then age.
    queue: BinaryHeap<Reverse<QueueEntry<F, A>>>,
    /// The processed clauses.
    arena: Vec<Clause<F, A>>,
    /// Processed clauses by conclusion (for subsumption).
    by_conclusion: HashMap<F, Vec<usize>>,
    /// Processed clauses without premises, by conclusion.
    derived: HashMap<F, Vec<usize>>,
    /// Processed clauses by their selected premise.
    waiting: HashMap<F, Vec<usize>>,
    found: Vec<BTreeSet<A>>,
    seq: u64,
    budget: usize,
    /// The budget ran out.
    stopped: bool,
}

/// A queued clause with its key: axioms, premises, age.
type QueueEntry<F, A> = (usize, usize, u64, Queued<F, A>);

/// A clause in the queue (ordered by the key beside it only).
#[derive(Debug, Clone)]
struct Queued<F: Node, A: Node>(Clause<F, A>);
impl<F: Node, A: Node> PartialEq for Queued<F, A> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl<F: Node, A: Node> Eq for Queued<F, A> {}
impl<F: Node, A: Node> PartialOrd for Queued<F, A> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<F: Node, A: Node> Ord for Queued<F, A> {
    fn cmp(&self, _: &Self) -> std::cmp::Ordering {
        std::cmp::Ordering::Equal
    }
}

impl<F: Node, A: Node> Enumeration<'_, F, A> {
    fn push(&mut self, clause: Clause<F, A>) {
        // Containing a justification found: nothing minimal follows from it.
        if self.found.iter().any(|j| j.is_subset(&clause.axioms)) {
            return;
        }
        self.seq += 1;
        self.queue.push(Reverse((
            clause.axioms.len(),
            clause.premises.len(),
            self.seq,
            Queued(clause),
        )));
    }

    /// Whether the budget stopped the enumeration.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    fn resolve(&mut self, waiting: usize, premise: F, derived: usize) {
        let (w, d) = (&self.arena[waiting], &self.arena[derived]);
        let mut premises = w.premises.clone();
        premises.remove(&premise);
        let clause = Clause {
            premises,
            axioms: w.axioms.union(&d.axioms).copied().collect(),
            conclusion: w.conclusion,
        };
        self.push(clause);
    }
}

impl<F: Node, A: Node> Iterator for Enumeration<'_, F, A> {
    type Item = Vec<A>;

    fn next(&mut self) -> Option<Vec<A>> {
        while let Some(Reverse((_, _, _, Queued(clause)))) = self.queue.pop() {
            if self.budget == 0 {
                self.stopped = true;
                self.queue.clear();
                return None;
            }
            self.budget -= 1;
            if self.found.iter().any(|j| j.is_subset(&clause.axioms)) {
                continue;
            }
            let same = self.by_conclusion.entry(clause.conclusion).or_default();
            if same.iter().any(|&i| self.arena[i].subsumes(&clause)) {
                continue;
            }
            let at = self.arena.len();
            same.push(at);
            let conclusion = clause.conclusion;
            let selected = clause.premises.iter().next_back().copied();
            let axioms = clause.axioms.clone();
            self.arena.push(clause);
            match selected {
                None => {
                    if conclusion == self.graph.goal {
                        self.found.push(axioms.clone());
                        return Some(axioms.into_iter().collect());
                    }
                    self.derived.entry(conclusion).or_default().push(at);
                    let partners = self.waiting.get(&conclusion).cloned().unwrap_or_default();
                    for w in partners {
                        self.resolve(w, conclusion, at);
                    }
                }
                // The selected premise: the greatest.
                Some(premise) => {
                    self.waiting.entry(premise).or_default().push(at);
                    let partners = self.derived.get(&premise).cloned().unwrap_or_default();
                    for d in partners {
                        self.resolve(at, premise, d);
                    }
                }
            }
        }
        None
    }
}

/// A proof: inferences, each premise derived by an earlier one, the last deriving the goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof<F, A> {
    pub goal: F,
    pub steps: Vec<Inference<F, A>>,
}

/// Why a proof doesn't check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofError<F> {
    /// A step uses a premise no earlier step derives.
    Unproved { step: usize, premise: F },
    /// A step isn't a valid instance of its rule.
    InvalidStep { step: usize },
    /// No step derives the goal.
    NoGoal,
}

impl<F: Node, A: Node> Proof<F, A> {
    /// The axioms the proof uses, sorted.
    pub fn axioms(&self) -> Vec<A> {
        let set: BTreeSet<A> = self
            .steps
            .iter()
            .flat_map(|s| s.axioms.iter().copied())
            .collect();
        set.into_iter().collect()
    }

    /// Checks the proof: every premise derived by an earlier step, every step valid for
    /// its rule (`valid`: the engine's own check), the goal derived.
    pub fn check(&self, valid: &dyn Fn(&Inference<F, A>) -> bool) -> Result<(), ProofError<F>> {
        let mut derived: HashSet<F> = HashSet::new();
        for (step, inference) in self.steps.iter().enumerate() {
            if let Some(&premise) = inference.premises.iter().find(|p| !derived.contains(p)) {
                return Err(ProofError::Unproved { step, premise });
            }
            if !valid(inference) {
                return Err(ProofError::InvalidStep { step });
            }
            derived.insert(inference.conclusion);
        }
        if derived.contains(&self.goal) {
            Ok(())
        } else {
            Err(ProofError::NoGoal)
        }
    }
}
