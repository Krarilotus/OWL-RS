//! Consistency under OWL 2 DL (docs/design/owl2-dl.md §8, "Consistency on commit"), by
//! the engine the ontology's profile allows: the context core where its Horn stage takes
//! the ontology (complete there, and polynomial), else the hypertableau.

use std::time::{Duration, Instant};

use nrese_dl::tableau;
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
    pub timeout: Duration,
    pub memory_bytes: usize,
    pub threads: usize,
    /// Stops the context core and hypertableau at their next budget check.
    pub cancel: Option<tableau::Cancel>,
    /// The deterministic budgets of a hypertableau run.
    pub max_nodes: usize,
    pub max_branch_points: u64,
}

/// The most conclusions one join of the context core may produce (its deterministic
/// budget; past it the Horn stage gives up and the hypertableau decides).
const MAX_JOIN: usize = 1 << 20;

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
    let mut checked = check_read(ontology, budget);
    if checked.verdict == Verdict::Consistent
        && let Some(why) = left_out(ontology)
    {
        checked.verdict = Verdict::Unknown(why);
    }
    checked
}

fn check_read(ontology: &Ontology, budget: &Budget) -> Checked {
    let started = Instant::now();
    let options = nrese_dl::context::Options {
        threads: budget.threads.max(1),
        proofs: false,
        budget: nrese_dl::context::Budget {
            deadline: Some(started + budget.timeout),
            max_join: Some(MAX_JOIN),
            task_memory: (budget.memory_bytes != usize::MAX).then_some(budget.memory_bytes),
            ..nrese_dl::context::Budget::default()
        },
        ..nrese_dl::context::Options::default()
    };
    if let Ok(saturated) =
        nrese_dl::context::saturate_with_cancel(ontology, &options, budget.cancel.clone())
    {
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
    let config = tableau::Config {
        timeout: Some(budget.timeout.saturating_sub(started.elapsed())),
        max_memory: budget.memory_bytes,
        max_nodes: budget.max_nodes,
        max_branch_points: Some(budget.max_branch_points),
        cancel: budget.cancel.clone(),
        ..tableau::Config::default()
    };
    // Island by island where the ABox splits (`nrese_dl::islands`).
    let outcome = nrese_dl::islands::consistency(ontology, &config);
    let verdict = match outcome.answer {
        tableau::Answer::Consistent => Verdict::Consistent,
        tableau::Answer::Inconsistent => Verdict::Inconsistent,
        tableau::Answer::Unsupported(why) => Verdict::Unknown(format!("unsupported: {why}")),
        tableau::Answer::GaveUp(why) => Verdict::Unknown(format!("gave up: {why}")),
    };
    Checked {
        verdict,
        engine: "hypertableau",
        elapsed: started.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget {
            timeout: Duration::from_secs(10),
            memory_bytes: usize::MAX,
            threads: 1,
            cancel: None,
            max_nodes: 10_000,
            max_branch_points: 10_000,
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
