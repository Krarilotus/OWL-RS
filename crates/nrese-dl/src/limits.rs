//! The DL engines' budgets. Each is set by its caller, and none is built in: a budget
//! nobody set is unlimited ([`crate::tableau::Config`], [`crate::classify::Options`] and
//! [`crate::context::Budget`] default to none). The store's settings (`dl.*`, defaults
//! with their reasons in docs/ops/config-reference.md) and the CLIs' options set them.
//! Every report of a budget reached names its setting ([`Limit::reached`]), so a run
//! that stops at a limit says which one, and `tests/limits.rs` holds the engines to it.
//!
//! What else bounds a run is no budget: the representation's capacities (variables per
//! clause, patterns per set of values), reported as unsupported or approximate.

use std::fmt::Display;
use std::time::{Duration, Instant};

/// When a task's time runs out: an instant, or never (an unlimited time budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    /// No deadline.
    pub const NONE: Self = Self(None);

    /// `timeout` from now (`None`: never).
    pub fn after(timeout: Option<Duration>) -> Self {
        Self(timeout.and_then(|timeout| Instant::now().checked_add(timeout)))
    }

    pub fn at(instant: Instant) -> Self {
        Self(Some(instant))
    }

    pub fn instant(self) -> Option<Instant> {
        self.0
    }

    pub fn passed(self) -> bool {
        self.0.is_some_and(|at| Instant::now() >= at)
    }

    /// The time left (`None`: unlimited).
    pub fn remaining(self) -> Option<Duration> {
        self.0
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// The tighter of `timeout` and what is left of this deadline (`None`: neither limits).
    pub fn within(self, timeout: Option<Duration>) -> Option<Duration> {
        match (timeout, self.remaining()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// A budget a DL task can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Limit {
    /// The task's time (`dl.timeout`).
    Time,
    /// What one engine run may hold (`dl.memory`).
    Memory,
    /// The request's memory envelope, which every run of the request draws from.
    Envelope,
    /// Nodes of one hypertableau run (`dl.max_nodes`).
    Nodes,
    /// Branch points of one hypertableau run (`dl.max_branch_points`).
    BranchPoints,
    /// Merges of one hypertableau run (`dl.max_merges`).
    Merges,
    /// Candidate answers a query checks with the exact services (`dl.max_candidates`).
    Candidates,
    /// Entailment tests of one explanation (`dl.explanation_tests`).
    ExplanationTests,
}

impl Limit {
    pub const ALL: [Self; 8] = [
        Self::Time,
        Self::Memory,
        Self::Envelope,
        Self::Nodes,
        Self::BranchPoints,
        Self::Merges,
        Self::Candidates,
        Self::ExplanationTests,
    ];

    /// The setting that sets it: the server's key; the CLIs take the same name as an
    /// option (`--max-nodes`).
    pub fn setting(self) -> &'static str {
        match self {
            Self::Time => "dl.timeout",
            Self::Memory => "dl.memory",
            Self::Envelope => "the request's memory envelope (budgets.query_memory)",
            Self::Nodes => "dl.max_nodes",
            Self::BranchPoints => "dl.max_branch_points",
            Self::Merges => "dl.max_merges",
            Self::Candidates => "dl.max_candidates",
            Self::ExplanationTests => "dl.explanation_tests",
        }
    }

    /// Why a run stopped at this budget: its setting, and `detail` (the value set, what
    /// was used).
    pub fn reached(self, detail: impl Display) -> String {
        format!("{} reached ({detail})", self.setting())
    }

    /// The budget a stop reason names, if it names one.
    pub fn named_in(reason: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|limit| reason.contains(&format!("{} reached", limit.setting())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_deadline_never_passes_and_limits_nothing() {
        assert!(!Deadline::after(None).passed());
        assert_eq!(Deadline::NONE.within(None), None);
        let soon = Some(Duration::from_secs(5));
        assert_eq!(Deadline::NONE.within(soon), soon);
        assert!(Deadline::after(Some(Duration::ZERO)).passed());
        let a_minute = Deadline::after(Some(Duration::from_secs(60)));
        assert!(a_minute.within(None).unwrap() <= Duration::from_secs(60));
        assert_eq!(a_minute.within(soon), soon);
        assert!(
            Deadline::after(Some(Duration::MAX)).instant().is_none(),
            "no overflow"
        );
    }

    #[test]
    fn every_reason_names_its_setting_and_is_read_back() {
        for limit in Limit::ALL {
            let why = limit.reached(42);
            assert!(why.starts_with(limit.setting()), "{why}");
            assert_eq!(Limit::named_in(&format!("gave up: {why}")), Some(limit));
        }
        assert_eq!(Limit::named_in("cancelled"), None);
        assert_eq!(
            Limit::named_in(crate::memory::EXHAUSTED),
            Some(Limit::Envelope)
        );
    }
}
