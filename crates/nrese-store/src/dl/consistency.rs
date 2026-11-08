//! Consistency under OWL 2 DL (docs/design/owl2-dl.md §8, "Consistency on commit"), by
//! the engine the ontology's profile allows: the context core where its Horn stage takes
//! the ontology (complete there, and polynomial), else the hypertableau.

use std::time::{Duration, Instant};

use nrese_dl::limits::{Deadline, Limit};
use nrese_dl::tableau;
use nrese_exec::workers::Workers;
use nrese_owl::Ontology;

/// What a consistency check found: the DL engines' one verdict, which classification
/// states too.
pub use nrese_dl::verdict::Verdict;

/// A check's verdict, which engine gave it and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub verdict: Verdict,
    /// `upper-bound`, `context-core` or `hypertableau`.
    pub engine: &'static str,
    pub elapsed: Duration,
}

/// The budgets of a check.
#[derive(Debug, Clone)]
pub struct Budget {
    /// The check's time (`None`: unlimited).
    pub timeout: Option<Duration>,
    /// What one engine run may hold (`dl.memory`, shared among a batch's workers; `None`:
    /// only the envelope).
    pub memory_bytes: Option<usize>,
    /// The request's envelope, which the runs' capacity and their compilation scratch
    /// draw from as well ([`nrese_dl::memory`]); `None`: `memory_bytes` alone.
    pub memory: Option<nrese_exec::Envelope>,
    /// The operation's allowance within its physical owner.
    pub workers: Workers,
    /// Stops the context core and hypertableau at their next budget check.
    pub cancel: Option<tableau::Cancel>,
    /// The deterministic budgets of a hypertableau run (`None`: unlimited).
    pub max_nodes: Option<usize>,
    pub max_branch_points: Option<u64>,
    pub max_merges: Option<u64>,
    /// The most conclusions one join of the context core may produce (`dl.max_join`): a
    /// route threshold, past it the Horn stage stops and the hypertableau decides.
    pub max_join: Option<usize>,
    /// The entailment tests a justification of the check's verdict may run
    /// (`dl.explanation_tests`).
    pub explanation_tests: Option<usize>,
}

impl Budget {
    /// A nested check spends the operation's remaining time, never a fresh full timeout.
    pub(crate) fn remaining(&self, deadline: Deadline) -> Self {
        Self {
            timeout: deadline.within(self.timeout),
            ..self.clone()
        }
    }

    /// Whether no time is left (a zero timeout).
    pub(crate) fn spent(&self) -> bool {
        self.timeout.is_some_and(|timeout| timeout.is_zero())
    }
}

/// Why a model of `ontology` says nothing for the source: parts of the source weren't
/// read (fatal diagnostics), so they may have no model. An inconsistency found stands.
pub fn left_out(ontology: &Ontology) -> Option<String> {
    let fatal: Vec<_> = ontology
        .diagnostics
        .iter()
        .filter(|d| d.is_fatal())
        .collect();
    let first = fatal.first()?;
    Some(format!(
        "{} part(s) of the data aren't OWL 2 DL and were left out; first: {first:?}",
        fatal.len()
    ))
}

/// Whether `ontology` is consistent: the context core's Horn stage where it takes the
/// ontology, else the hypertableau within `budget`. `Consistent` only if nothing of the
/// source was left out ([`left_out`]).
pub fn check(ontology: &Ontology, budget: &Budget) -> Checked {
    let started = Instant::now();
    let mut checked = check_read(ontology, budget);
    checked.elapsed = started.elapsed();
    if checked.verdict == Verdict::Consistent
        && let Some(why) = left_out(ontology)
    {
        checked.verdict = Verdict::Unknown(why);
    }
    checked
}

/// Whether `ontology` is consistent, given that it was before a change that removed
/// assertions and added assertions about the individuals `touched`, its schema as it was
/// (docs/design/owl2-dl.md §9): only the ABox islands with a touched individual are
/// decided ([`nrese_dl::islands::after_assertions`]). `None` where the ABox doesn't split;
/// the caller checks the whole ([`check`]).
pub(crate) fn check_after_assertions(
    ontology: &Ontology,
    touched: &std::collections::HashSet<u64>,
    budget: &Budget,
) -> Option<Checked> {
    let started = Instant::now();
    if left_out(ontology).is_some() {
        return None;
    }
    let outcome = nrese_dl::islands::after_assertions(ontology, touched, &tableau_config(budget))?;
    Some(Checked {
        verdict: verdict(outcome.answer),
        engine: "touched-islands",
        elapsed: started.elapsed(),
    })
}

/// The hypertableau's configuration within `budget`.
fn tableau_config(budget: &Budget) -> tableau::Config {
    tableau::Config {
        timeout: budget.timeout,
        max_memory: budget.memory_bytes,
        memory: budget.memory.clone(),
        max_nodes: budget.max_nodes,
        max_branch_points: budget.max_branch_points,
        max_merges: budget.max_merges,
        cancel: budget.cancel.clone(),
        workers: Some(budget.workers.clone()),
        ..tableau::Config::default()
    }
}

fn verdict(answer: tableau::Answer) -> Verdict {
    match answer {
        tableau::Answer::Consistent => Verdict::Consistent,
        tableau::Answer::Inconsistent => Verdict::Inconsistent,
        tableau::Answer::Unsupported(why) => Verdict::Unknown(format!("unsupported: {why}")),
        tableau::Answer::GaveUp(why) => Verdict::Unknown(format!("gave up: {why}")),
    }
}

fn check_read(ontology: &Ontology, budget: &Budget) -> Checked {
    check_using(ontology, budget, normalisation())
}

fn normalisation() -> nrese_owl::Options {
    // This check consumes only the verdict, never clause proofs or justifications.
    nrese_owl::Options {
        exact_provenance: false,
        ..nrese_owl::Options::default()
    }
}

fn check_using(ontology: &Ontology, budget: &Budget, normalise: nrese_owl::Options) -> Checked {
    let started = Instant::now();
    let deadline = Deadline::after(budget.timeout);
    if budget.spent() {
        return Checked {
            verdict: Verdict::Unknown(Limit::Time.reached("no time left")),
            engine: "none",
            elapsed: started.elapsed(),
        };
    }
    let options = nrese_dl::context::Options {
        threads: budget.workers.width(),
        proofs: false,
        normalise,
        budget: nrese_dl::context::Budget {
            deadline: deadline.instant(),
            max_join: budget.max_join,
            task_memory: budget.memory_bytes,
            memory: budget.memory.clone(),
            ..nrese_dl::context::Budget::default()
        },
        ..nrese_dl::context::Options::default()
    };
    if let Ok(saturated) = nrese_dl::context::classify::saturate_with_workers(
        ontology,
        &options,
        budget.cancel.clone(),
        &budget.workers,
    ) {
        let verdict = match saturated.consistent() {
            true => Verdict::Consistent,
            false => Verdict::Inconsistent,
        };
        return Checked {
            verdict,
            engine: "context-core",
            elapsed: started.elapsed(),
        };
    }
    if budget
        .cancel
        .as_ref()
        .is_some_and(tableau::Cancel::is_cancelled)
    {
        return Checked {
            verdict: Verdict::Unknown("cancelled".to_owned()),
            engine: "context-core",
            elapsed: started.elapsed(),
        };
    }
    let config = tableau_config(&budget.remaining(deadline));
    // Island by island where the ABox splits (`nrese_dl::islands`).
    let outcome = nrese_dl::islands::consistency(ontology, &config);
    Checked {
        verdict: verdict(outcome.answer),
        engine: "hypertableau",
        elapsed: started.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_owl::{Axiom, ClassExpr, ExprId};

    fn budget() -> Budget {
        Budget {
            timeout: Some(Duration::from_secs(10)),
            memory_bytes: None,
            memory: None,
            workers: Workers::serial(),
            cancel: None,
            max_nodes: Some(10_000),
            max_branch_points: Some(10_000),
            max_merges: None,
            max_join: None,
            explanation_tests: None,
        }
    }

    fn shared_name() -> Ontology {
        let mut ontology = Ontology::default();
        let a = ExprId(ontology.classes.intern(ClassExpr::Class(1)));
        let b = ExprId(ontology.classes.intern(ClassExpr::Class(2)));
        let both = ExprId(ontology.classes.intern(ClassExpr::And(vec![a, b])));
        for individual in 100..132 {
            ontology
                .axioms
                .push(Axiom::ClassAssertion(both, individual));
            ontology.sources.push(Vec::new());
        }
        ontology
    }

    // R1: verdict-only normalisation retains one definition source per shared name,
    // while exact normalisation still supports every axiom that uses that name.
    #[test]
    fn consistency_normalisation_saves_shared_name_source_sets() {
        let ontology = shared_name();
        let sets = |options| {
            nrese_owl::normalise_with(&ontology, options)
                .clauses
                .iter()
                .map(|clause| clause.sources.len())
                .sum::<usize>()
        };
        let exact = sets(nrese_owl::Options::default());
        let consistency = sets(normalisation());
        eprintln!("shared-name source sets: exact={exact}, consistency={consistency}");
        assert_eq!(
            exact, 64,
            "32 source alternatives on each of two definitions"
        );
        assert_eq!(consistency, 2, "one source set on each definition suffices");
    }

    // R1 guards: exact and verdict-only normalisation decide the same ontology;
    // deterministic limits stay undecided, including the non-Horn fallback path.
    #[test]
    fn consistency_normalisation_preserves_every_verdict() {
        let consistent = shared_name();
        let mut inconsistent = consistent.clone();
        let nothing = ExprId(inconsistent.classes.intern(ClassExpr::Nothing));
        inconsistent
            .axioms
            .push(Axiom::ClassAssertion(nothing, 100));
        inconsistent.sources.push(Vec::new());
        let mut nonhorn = Ontology::default();
        let a = ExprId(nonhorn.classes.intern(ClassExpr::Class(1)));
        let b = ExprId(nonhorn.classes.intern(ClassExpr::Class(2)));
        let either = ExprId(nonhorn.classes.intern(ClassExpr::Or(vec![a, b])));
        nonhorn.axioms.push(Axiom::ClassAssertion(either, 100));
        nonhorn.sources.push(Vec::new());
        let mut clash = nonhorn.clone();
        clash.axioms.push(Axiom::DisjointClasses(vec![a, b]));
        clash.axioms.push(Axiom::ClassAssertion(a, 100));
        clash.axioms.push(Axiom::ClassAssertion(b, 100));
        clash.sources.resize(clash.axioms.len(), Vec::new());
        let nodes = Budget {
            max_nodes: Some(0),
            ..budget()
        };
        let expired = Budget {
            timeout: Some(Duration::ZERO),
            ..budget()
        };
        let cancel = tableau::Cancel::default();
        cancel.cancel();
        let cancelled = Budget {
            cancel: Some(cancel),
            ..budget()
        };
        let rows = [
            ("empty", Ontology::default(), budget(), "consistent"),
            ("shared name", consistent.clone(), budget(), "consistent"),
            ("contradiction", inconsistent, budget(), "inconsistent"),
            ("tableau", nonhorn.clone(), budget(), "consistent"),
            ("tableau contradiction", clash, budget(), "inconsistent"),
            ("zero nodes", nonhorn, nodes, "unknown"),
            ("zero time", consistent.clone(), expired, "unknown"),
            ("cancelled", consistent, cancelled, "unknown"),
        ];
        let mut wrong = Vec::new();
        for (name, ontology, budget, expected) in rows {
            let exact = check_using(&ontology, &budget, nrese_owl::Options::default());
            let actual = check(&ontology, &budget);
            eprintln!(
                "guard {name}: exact={:?}, consistency={:?} ({})",
                exact.verdict, actual.verdict, actual.engine
            );
            if exact.verdict.as_str() != expected || actual.verdict != exact.verdict {
                wrong.push(format!(
                    "{name}: expected {expected}, exact={:?}, consistency={:?}",
                    exact.verdict, actual.verdict
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_request_flag_reaches_context_consistency_and_entailment() {
        let request = nrese_sparql::CancellationToken::new();
        let budget = Budget {
            cancel: Some(tableau::Cancel::from_flag(request.flag())),
            ..budget()
        };
        let mut ontology = Ontology::default();
        let class = nrese_owl::ExprId(ontology.classes.intern(nrese_owl::ClassExpr::Class(1)));
        assert_eq!(check(&ontology, &budget).verdict, Verdict::Consistent);
        // The already-created nested budget must observe subsequent request cancellation.
        request.cancel();
        let checked = check(&ontology, &budget);
        assert_eq!(checked.engine, "context-core");
        assert_eq!(checked.verdict, Verdict::Unknown("cancelled".to_owned()));
        assert!(matches!(
            crate::dl::entailment::nonempty(&ontology, class, &budget),
            crate::dl::entailment::Entailed::Unknown(_)
        ));
    }
}
