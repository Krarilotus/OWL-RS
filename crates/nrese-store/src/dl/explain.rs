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
//! All tests, including final verification, share one deadline and cancellation token.
//! Past `MAX_TESTS` or `dl.timeout`, return a known-entailing set marked not `minimal`
//! or `verified`; no fresh verification timeout is granted.

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
        if self.tests >= MAX_TESTS
            || Instant::now() >= self.deadline
            || self
                .budget
                .cancel
                .as_ref()
                .is_some_and(|c| c.is_cancelled())
        {
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
        let budget = super::gate::remaining_budget(self.budget, self.deadline);
        match self.goal {
            Goal::Inconsistent => match consistency::check(&o, &budget).verdict {
                Verdict::Inconsistent => true,
                Verdict::Consistent => false,
                Verdict::Unknown(_) => {
                    self.spent = true;
                    false
                }
            },
            Goal::Entails(axiom) => match entailment::entails(&o, axiom, self.fresh, &budget) {
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
        if self.spent {
            return candidates.to_vec();
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

    /// Verify with the time and test count still left, or return the full set whose
    /// entailment was established before minimisation. Never restart a spent search.
    fn finish(&mut self, candidates: Vec<usize>, mut axioms: Vec<usize>) -> Justification {
        let mut minimal = !self.spent;
        let verified = self.holds(&axioms);
        if !verified {
            axioms = candidates;
            minimal = false;
        }
        axioms.sort_unstable();
        let mut proof = ProofGraph::new(0u8);
        proof.add("owl2-dl", &[], &axioms, 0u8);
        Justification {
            axioms,
            minimal,
            verified,
            tests: self.tests,
            proof,
        }
    }
}

/// A justification of `goal` in `ontology`, or `None` where it doesn't hold (or that
/// can't be decided within the budget). All nested checks and verification use the
/// caller's absolute deadline.
pub(crate) fn justify(
    ontology: &Ontology,
    goal: &Goal,
    fresh: &[u64],
    budget: &Budget,
    deadline: Instant,
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
        deadline,
        spent: false,
    };
    if !search.holds(&candidates) {
        return None;
    }
    let axioms = match candidates.is_empty() {
        true => Vec::new(),
        false => search.qx(&[], false, &candidates),
    };
    Some(search.finish(candidates, axioms))
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
        let deadline = started + self.config().dl.timeout;
        // An entailment oracle can contain several consistency checks. The shared
        // token also stops those inner tests at this operation's deadline.
        let justification = super::gate::cancellable(&|| Instant::now() >= deadline, |cancel| {
            justify(
                &ontology,
                &Goal::Entails(axiom),
                &fresh,
                &super::gate::budget(self, Some(cancel)),
                deadline,
            )
        })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_dl::tableau::Cancel;
    use std::time::Duration;

    #[test]
    fn cancellation_and_expiry_do_not_restart_minimisation_or_verification() {
        let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
        let mut ontology = Ontology::default();
        let nothing = nrese_owl::ExprId(ontology.classes.intern(nrese_owl::ClassExpr::Nothing));
        ontology.axioms.push(Axiom::ClassAssertion(nothing, 1));
        ontology.sources.push(Vec::new());
        for stop in ["cancelled", "deadline", "tests"] {
            let cancel = Cancel::default();
            let budget = super::super::gate::budget(&store, Some(cancel.clone()));
            let mut search = Search {
                ontology: &ontology,
                goal: &Goal::Inconsistent,
                fresh: &[],
                budget: &budget,
                background: Vec::new(),
                tests: 0,
                deadline: Instant::now() + budget.timeout,
                spent: false,
            };
            assert!(
                search.holds(&[0]),
                "establish the full set before exhausting a budget"
            );
            match stop {
                "cancelled" => cancel.cancel(),
                "deadline" => search.deadline = Instant::now() - Duration::from_secs(1),
                "tests" => search.tests = MAX_TESTS,
                _ => unreachable!(),
            }
            let tests = search.tests;
            let deadline = search.deadline;
            let candidate = search.qx(&[], true, &[0]);
            let result = search.finish(vec![0], candidate);
            assert_eq!(result.axioms, [0]);
            assert!(!result.minimal && !result.verified, "{stop}: {result:?}");
            assert_eq!(result.tests, tests, "{stop}: no new oracle call");
            assert_eq!(
                search.deadline, deadline,
                "verification cannot extend the deadline"
            );
        }
    }
}
