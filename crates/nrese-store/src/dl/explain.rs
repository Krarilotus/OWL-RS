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
//! Past `dl.explanation_tests` or `dl.timeout`, return a known-entailing set marked not `minimal`
//! or `verified`; no fresh verification timeout is granted.

use nrese_dl::limits::Deadline;
use std::time::Instant;

use nrese_owl::{Axiom, Ontology, ProofGraph};

use super::consistency::{self, Budget, Verdict};
use super::entailment::{self, Entailed};

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
    deadline: Deadline,
    spent: bool,
    unknown: Option<String>,
}

impl Search<'_> {
    /// Whether the axioms `subset` (with the background) give the goal; a test past the
    /// budget counts as "no" (so the axioms stay in), and marks the search spent.
    fn holds(&mut self, subset: &[usize]) -> bool {
        let stopped = self.budget.stopped(self.deadline).or_else(|| {
            self.budget
                .explanation_tests
                .is_some_and(|most| self.tests >= most)
                .then(|| "dl.explanation_tests reached before the entailment test".to_owned())
        });
        if let Some(why) = stopped {
            self.spent = true;
            self.unknown = Some(why);
            return false;
        }
        let Some(tests) = self.tests.checked_add(1) else {
            self.spent = true;
            self.unknown = Some("explanation test count is unrepresentable".to_owned());
            return false;
        };
        self.tests = tests;
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
            Goal::Inconsistent => {
                match consistency::check_until(&o, self.budget, self.deadline).verdict {
                    Verdict::Inconsistent => true,
                    Verdict::Consistent => false,
                    Verdict::Unknown(why) => {
                        self.spent = true;
                        self.unknown = Some(why);
                        false
                    }
                }
            }
            Goal::Entails(axiom) => {
                match entailment::entails_until(&o, axiom, self.fresh, self.budget, self.deadline) {
                    Entailed::Yes => true,
                    Entailed::No => false,
                    Entailed::Unknown(why) => {
                        self.spent = true;
                        self.unknown = Some(why);
                        false
                    }
                }
            }
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
    deadline: Deadline,
) -> Option<Justification> {
    justify_outcome(ontology, goal, fresh, budget, deadline)
        .ok()
        .flatten()
}

/// `Ok(None)` is definitive non-entailment; `Err` preserves an initial unknown.
pub(crate) fn justify_outcome(
    ontology: &Ontology,
    goal: &Goal,
    fresh: &[u64],
    budget: &Budget,
    deadline: Deadline,
) -> Result<Option<Justification>, String> {
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
        unknown: None,
    };
    if !search.holds(&candidates) {
        return match search.unknown {
            Some(why) => Err(why),
            None => Ok(None),
        };
    }
    let axioms = match candidates.is_empty() {
        true => Vec::new(),
        false => search.qx(&[], false, &candidates),
    };
    Ok(Some(search.finish(candidates, axioms)))
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

/// A bounded search which never established truth is not non-entailment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DlExplanationOutcome {
    Explained(DlExplanation),
    DoesNotHold,
    Unknown(String),
}

// The same statement-independent wording for an incapable read scope and an input
// the snapshot cannot represent. Neither branch established a countermodel, and
// neither may disclose private graphs or distinguish hidden from missing terms.
const INPUT_UNDECIDED: &str = "the DL explanation cannot be decided from the available input";

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
                .flat_map(|source| by_graph(source, decode))
                .collect(),
        })
        .collect()
}

/// `source`'s triples under each graph they are stated in, the source's own graph first
/// (`None`: the default graph).
fn by_graph(
    source: &nrese_owl::Source,
    decode: &dyn Fn(u64) -> String,
) -> Vec<(Option<String>, Vec<[String; 3]>)> {
    let mut groups: Vec<(u64, Vec<[String; 3]>)> = vec![(source.graph, Vec::new())];
    for (i, triple) in source.triples.iter().enumerate() {
        for graph in source.graphs(i) {
            let at = match groups.iter().position(|(g, _)| *g == graph) {
                Some(at) => at,
                None => {
                    groups.push((graph, Vec::new()));
                    groups.len() - 1
                }
            };
            groups[at].1.push(triple.map(decode));
        }
    }
    groups
        .into_iter()
        .filter(|(_, triples)| !triples.is_empty())
        .map(|(graph, triples)| {
            let named = graph != nrese_engine::TermId::DEFAULT_GRAPH.raw();
            (named.then(|| decode(graph)), triples)
        })
        .collect()
}

impl crate::StoreService {
    /// Compatibility wrapper: an initial unknown has no explanation. Outcome-sensitive
    /// consumers use explain_dl_outcome to distinguish it from non-entailment.
    pub fn explain_dl(
        &self,
        scope: &crate::ReadScope,
        statement: [nrese_rdf::TermRef<'_>; 3],
    ) -> Option<DlExplanation> {
        match self.explain_dl_outcome(scope, statement).ok()? {
            DlExplanationOutcome::Explained(answer) => Some(answer),
            DlExplanationOutcome::DoesNotHold | DlExplanationOutcome::Unknown(_) => None,
        }
    }

    /// Legacy DL construction with a truthful initial outcome. This compatibility
    /// method does not claim full checked mapper/import/output resource ownership.
    pub fn explain_dl_outcome(
        &self,
        scope: &crate::ReadScope,
        statement: [nrese_rdf::TermRef<'_>; 3],
    ) -> crate::StoreResult<DlExplanationOutcome> {
        use nrese_engine::TermId;
        use nrese_rdf::NamedNodeRef;
        if !matches!(scope, crate::ReadScope::All) {
            return Ok(DlExplanationOutcome::Unknown(INPUT_UNDECIDED.to_owned()));
        }
        let started = Instant::now();
        let snapshot = self.engine().snapshot();
        let [s, p, o] = statement.map(|t| snapshot.lookup(t).map(TermId::raw));
        let (Some(s), Some(p), Some(o)) = (s, p, o) else {
            return Ok(DlExplanationOutcome::Unknown(INPUT_UNDECIDED.to_owned()));
        };
        let base = super::query::ontology_at(self, &snapshot);
        // The request's envelope: its copy of the premise, and the tests' engines.
        let memory = self.envelope(None);
        let mut scratch = nrese_dl::memory::Scratch::new(Some(&memory));
        scratch.charge(|| nrese_dl::memory::ontology_bytes(&base))?;
        let mut ontology = (*base).clone();
        let ids = super::query::Ids::of(&snapshot);
        let axiom = match super::query::ground_axiom(&mut ontology, &snapshot, [s, p, o], ids) {
            Ok(axiom) => axiom,
            Err(why) => return Ok(DlExplanationOutcome::Unknown(why)),
        };
        let tx = self.engine().speculative();
        let replica = self.dl().replica();
        let fresh: Option<Vec<u64>> = (0..super::source::FRESH)
            .map(|i| {
                super::source::resolve(
                    replica,
                    &tx,
                    NamedNodeRef::new_unchecked(&super::source::fresh_iri(i)).into(),
                )
                .map(TermId::raw)
            })
            .collect();
        let Some(fresh) = fresh else {
            return Ok(DlExplanationOutcome::Unknown(
                "replica entailment terms have not arrived".to_owned(),
            ));
        };
        let deadline = Deadline::after(self.config().dl.timeout);
        // An entailment oracle can contain several consistency checks. The shared
        // token also stops those inner tests at this operation's deadline.
        let justification = super::gate::cancellable(&|| deadline.passed(), |cancel| {
            justify_outcome(
                &ontology,
                &Goal::Entails(axiom),
                &fresh,
                &super::gate::budget(self, Some(cancel), &memory),
                deadline,
            )
        });
        let justification = match justification {
            Ok(Some(answer)) => answer,
            Ok(None) => return Ok(DlExplanationOutcome::DoesNotHold),
            Err(why) => return Ok(DlExplanationOutcome::Unknown(why)),
        };
        let decode = |t: u64| {
            snapshot
                .decode(TermId::from_raw(t))
                .map_or_else(|| format!("#{t}"), |term| term.to_string())
        };
        Ok(DlExplanationOutcome::Explained(DlExplanation {
            axioms: explained(&ontology, &justification, &decode),
            minimal: justification.minimal,
            verified: justification.verified,
            tests: justification.tests,
            micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        }))
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
            let mut budget = super::super::gate::budget(
                &store,
                Some(cancel.clone()),
                &nrese_exec::Envelope::unlimited(),
            );
            // The test budget as configured: none by default.
            budget.explanation_tests = Some(256);
            let mut search = Search {
                ontology: &ontology,
                goal: &Goal::Inconsistent,
                fresh: &[],
                budget: &budget,
                background: Vec::new(),
                tests: 0,
                deadline: Deadline::after(budget.timeout),
                spent: false,
                unknown: None,
            };
            assert!(
                search.holds(&[0]),
                "establish the full set before exhausting a budget"
            );
            match stop {
                "cancelled" => cancel.cancel(),
                "deadline" => {
                    search.deadline = Deadline::at(Instant::now() - Duration::from_secs(1))
                }
                "tests" => search.tests = 256,
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
