//! Explanations of what OWL 2 DL entails (docs/design/owl2-dl.md §10, version 1 of the
//! black box for the hypertableau): a **justification**, a minimal set of the ontology's
//! axioms that entails the conclusion, each with the triples and graph it was read from.
//!
//! 1. The conclusion is checked to hold at all (an entailment test, or the consistency
//!    check for an inconsistency).
//! 2. The axioms are minimised by divide and conquer (QuickXplain, Junker 2004): with the
//!    DL engines as the oracle it takes O(k log(n/k)) tests for a justification of `k`
//!    of `n` axioms, not one per axiom. Declarations are kept as the background (they
//!    carry no logical content).
//! 3. The result is checked with a fresh test (`verified`), and given in the proof IR's
//!    terms ([`nrese_owl::ProofGraph`]): one inference by the engine from the axioms to
//!    the conclusion, so glass-box steps (the context core's, the RL rules') can join it.
//!
//! Each test has the query's budgets; past `MAX_TESTS` or `dl.timeout` the set found so
//! far is returned, marked not `minimal` (it still entails the conclusion).

use std::time::Instant;

use nrese_owl::{Axiom, Ontology, ProofGraph};

use super::consistency::{self, Budget, Verdict};
use super::entailment::{self, Entailed};

/// The most entailment tests one explanation runs.
const MAX_TESTS: usize = 256;

/// What is explained: an axiom the ontology entails, or its inconsistency.
#[derive(Debug, Clone)]
pub(crate) enum Goal {
    Entails(Axiom),
    Inconsistent,
}

/// A justification: the axioms (indexes into the ontology's), and what it took.
#[derive(Debug, Clone)]
pub struct Justification {
    pub axioms: Vec<usize>,
    /// No axiom can be left out (false: a budget ran out before minimisation ended).
    pub minimal: bool,
    /// A fresh test confirmed that the axioms entail the conclusion.
    pub verified: bool,
    /// Entailment tests run.
    pub tests: usize,
    /// The proof IR: one inference by the DL engines from the axioms to the conclusion
    /// (the conclusion `0`).
    pub proof: ProofGraph<u8, usize>,
}

struct Search<'a> {
    ontology: &'a Ontology,
    goal: &'a Goal,
    fresh: &'a [u64],
    budget: &'a Budget,
    background: Vec<usize>,
    tests: usize,
    deadline: Instant,
    spent: bool,
}

impl Search<'_> {
    /// Whether the axioms `subset` (with the background) give the goal; a test past the
    /// budget counts as "no" (so the axioms stay in), and marks the search spent.
    fn holds(&mut self, subset: &[usize]) -> bool {
        if self.tests >= MAX_TESTS || Instant::now() >= self.deadline {
            self.spent = true;
            return false;
        }
        self.tests += 1;
        let mut o = Ontology {
            axioms: Vec::new(),
            sources: Vec::new(),
            ..self.ontology.clone()
        };
        for &i in self.background.iter().chain(subset) {
            o.axioms.push(self.ontology.axioms[i].clone());
            o.sources.push(Vec::new());
        }
        match self.goal {
            Goal::Inconsistent => match consistency::check(&o, self.budget).verdict {
                Verdict::Inconsistent => true,
                Verdict::Consistent => false,
                Verdict::Unknown(_) => {
                    self.spent = true;
                    false
                }
            },
            Goal::Entails(axiom) => match entailment::entails(&o, axiom, self.fresh, self.budget) {
                Entailed::Yes => true,
                Entailed::No => false,
                Entailed::Unknown(_) => {
                    self.spent = true;
                    false
                }
            },
        }
    }

    /// QuickXplain: a minimal part of `candidates` that, with `base`, gives the goal
    /// (given that `base` ∪ `candidates` does). `tested`: whether `base` was just grown.
    fn qx(&mut self, base: &[usize], tested: bool, candidates: &[usize]) -> Vec<usize> {
        if tested && self.holds(base) {
            return Vec::new();
        }
        if candidates.len() == 1 {
            return candidates.to_vec();
        }
        let (c1, c2) = candidates.split_at(candidates.len() / 2);
        let with_c1: Vec<usize> = base.iter().chain(c1).copied().collect();
        let x2 = self.qx(&with_c1, true, c2);
        let with_x2: Vec<usize> = base.iter().chain(&x2).copied().collect();
        let x1 = self.qx(&with_x2, !x2.is_empty(), c1);
        let mut out = x1;
        out.extend(x2);
        out
    }
}

/// A justification of `goal` in `ontology`, or `None` where it doesn't hold (or that
/// can't be decided within the budget).
pub(crate) fn justify(
    ontology: &Ontology,
    goal: &Goal,
    fresh: &[u64],
    budget: &Budget,
) -> Option<Justification> {
    let (background, candidates): (Vec<usize>, Vec<usize>) = (0..ontology.axioms.len())
        .partition(|&i| matches!(ontology.axioms[i], Axiom::Declaration(..)));
    let mut search = Search {
        ontology,
        goal,
        fresh,
        budget,
        background,
        tests: 0,
        deadline: Instant::now() + budget.timeout,
        spent: false,
    };
    if !search.holds(&candidates) {
        return None;
    }
    let mut axioms = match candidates.is_empty() {
        true => Vec::new(),
        false => search.qx(&[], false, &candidates),
    };
    let mut minimal = !search.spent;
    // A spent search may have dropped nothing it needed (a "no" past the budget keeps an
    // axiom in), but check: the set must still give the goal.
    let verified = {
        let spent = search.spent;
        search.tests = search.tests.min(MAX_TESTS - 1);
        search.deadline = search.deadline.max(Instant::now() + budget.timeout);
        let holds = search.holds(&axioms);
        search.spent = spent;
        holds
    };
    if !verified {
        // Fall back to every candidate: what certainly gives the goal.
        axioms = candidates;
        minimal = false;
    }
    axioms.sort_unstable();
    let mut proof = ProofGraph::new(0u8);
    proof.add("owl2-dl", &[], &axioms, 0u8);
    Some(Justification {
        axioms,
        minimal,
        verified,
        tests: search.tests,
        proof,
    })
}

/// An axiom of a justification, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainedAxiom {
    /// The axiom in the functional-style syntax.
    pub axiom: String,
    /// Where it was read from: per graph (`None`: the default graph), the triples of its
    /// RDF form (N-Triples terms).
    pub sources: Vec<(Option<String>, Vec<[String; 3]>)>,
}

/// Why a statement holds under OWL 2 DL ([`crate::StoreService::explain_dl`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlExplanation {
    pub axioms: Vec<ExplainedAxiom>,
    pub minimal: bool,
    pub verified: bool,
    pub tests: usize,
    pub micros: u64,
}

/// The justification's axioms with their sources, decoded.
pub(crate) fn explained(
    ontology: &Ontology,
    justification: &Justification,
    decode: &dyn Fn(u64) -> String,
) -> Vec<ExplainedAxiom> {
    justification
        .axioms
        .iter()
        .map(|&i| ExplainedAxiom {
            axiom: ontology.functional(&ontology.axioms[i], decode),
            sources: ontology.sources[i]
                .iter()
                .map(|source| {
                    let graph = (source.graph != nrese_engine::TermId::DEFAULT_GRAPH.raw())
                        .then(|| decode(source.graph));
                    let triples = source.triples.iter().map(|t| t.map(decode)).collect();
                    (graph, triples)
                })
                .collect(),
        })
        .collect()
}

impl crate::StoreService {
    /// Why the statement `subject predicate object` holds under OWL 2 DL: a minimal set
    /// of the asserted ontology's axioms entailing it, with where each was read from
    /// (the module docs). `None` if it doesn't hold (or that wasn't decided), or the
    /// reader doesn't see every graph (DL answers are only theirs). On a replica, `None`
    /// where the entailment tests' terms haven't arrived.
    pub fn explain_dl(
        &self,
        scope: &crate::ReadScope,
        statement: [nrese_rdf::TermRef<'_>; 3],
    ) -> Option<DlExplanation> {
        use nrese_engine::TermId;
        use nrese_rdf::NamedNodeRef;
        if !matches!(scope, crate::ReadScope::All) {
            return None;
        }
        let started = Instant::now();
        let snapshot = self.engine().snapshot();
        let [s, p, o] = statement.map(|t| snapshot.lookup(t).map(TermId::raw));
        let (s, p, o) = (s?, p?, o?);
        let base = super::query::ontology_at(self, &snapshot);
        let mut ontology = (*base).clone();
        let ids = super::query::Ids::of(&snapshot);
        let axiom = super::query::ground_axiom(&mut ontology, &snapshot, [s, p, o], ids).ok()?;
        let tx = self.engine().speculative();
        let replica = self.dl().replica();
        let fresh: Vec<u64> = (0..super::source::FRESH)
            .map(|i| {
                super::source::resolve(
                    replica,
                    &tx,
                    NamedNodeRef::new_unchecked(&super::source::fresh_iri(i)).into(),
                )
                .map(TermId::raw)
            })
            .collect::<Option<_>>()?;
        let justification = justify(
            &ontology,
            &Goal::Entails(axiom),
            &fresh,
            &super::gate::budget(self, None),
        )?;
        let decode = |t: u64| {
            snapshot
                .decode(TermId::from_raw(t))
                .map_or_else(|| format!("#{t}"), |term| term.to_string())
        };
        Some(DlExplanation {
            axioms: explained(&ontology, &justification, &decode),
            minimal: justification.minimal,
            verified: justification.verified,
            tests: justification.tests,
            micros: started.elapsed().as_micros() as u64,
        })
    }
}
