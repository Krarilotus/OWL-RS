//! Consistency under OWL 2 DL (docs/design/owl2-dl.md §8, "Consistency on commit"), by
//! the engine the ontology's profile allows: the context core where its Horn stage takes
//! the ontology (complete there, and polynomial), else the hypertableau.

use std::time::{Duration, Instant};

use nrese_dl::limits::{Deadline, Limit};
use nrese_dl::tableau;
use nrese_exec::workers::Workers;
use nrese_owl::Ontology;

/// What a consistency check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Consistent,
    Inconsistent,
    /// Not decided: a budget ran out, the engines lack something the ontology needs, or
    /// the check is off. Answers can't be certain then.
    Unknown(String),
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Consistent => "consistent",
            Self::Inconsistent => "inconsistent",
            Self::Unknown(_) => "unknown",
        }
    }

    /// Why it isn't decided, if it isn't.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Unknown(why) => Some(why),
            _ => None,
        }
    }
}

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

    /// Stop before preparing or starting another part of the operation.
    pub(crate) fn stopped(&self, deadline: Deadline) -> Option<String> {
        if self
            .cancel
            .as_ref()
            .is_some_and(tableau::Cancel::is_cancelled)
        {
            Some("cancelled".to_owned())
        } else if self.spent() || deadline.passed() {
            Some(Limit::Time.reached("the deadline passed"))
        } else {
            None
        }
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
    check_until(ontology, budget, Deadline::after(budget.timeout))
}

/// A nested check inherits its owner's absolute deadline, including preparation.
/// The owner has already folded `budget.timeout` into this deadline.
pub(crate) fn check_until(ontology: &Ontology, budget: &Budget, deadline: Deadline) -> Checked {
    let started = Instant::now();
    let mut checked = check_read(ontology, budget, deadline);
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
    let deadline = Deadline::after(budget.timeout);
    if left_out(ontology).is_some() {
        return None;
    }
    let outcome =
        nrese_dl::islands::after_assertions(ontology, touched, &tableau_config(budget, deadline))?;
    Some(Checked {
        verdict: verdict(outcome.answer),
        engine: "touched-islands",
        elapsed: started.elapsed(),
    })
}

/// The hypertableau's configuration with the owner's deadline and no timeout to renew.
pub(super) fn tableau_config(budget: &Budget, deadline: Deadline) -> tableau::Config {
    tableau::Config {
        deadline: deadline.instant(),
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

fn check_read(ontology: &Ontology, budget: &Budget, deadline: Deadline) -> Checked {
    let started = Instant::now();
    if budget.spent() || deadline.passed() {
        return Checked {
            verdict: Verdict::Unknown(Limit::Time.reached("no time left")),
            engine: "none",
            elapsed: started.elapsed(),
        };
    }
    let options = nrese_dl::context::Options {
        threads: budget.workers.width(),
        proofs: false,
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
    let config = tableau_config(budget, deadline);
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
